// simplify.rs — the passes that turn "assembly with = signs" into C.
//
// The original had none of these. Every machine instruction became one
// output line, so a three-instruction sequence that a compiler generated
// for `return -x;` printed as three assignments to `eax`, and the
// caller-side argument setup printed twice (once as `rdi = ...`, once
// inside the call).
//
// Passes here, in order:
//   resolve_flags       jcc/setcc/cmovcc <- the flag-setting instruction
//   propagate           forward copy/expression propagation per block
//   recover_call_args   fill call argument lists from propagated values
//   liveness + dce      drop register writes nothing reads
//   fold                algebraic and cast cleanup

use crate::ir::*;
use crate::lifter::{sysv_arg_index, CALLER_SAVED, SYSV_INT_ARGS};
use std::collections::{HashMap, HashSet};

/// Prototypes for common libc entry points, so argument lists don't have
/// to be guessed from register writes alone.
#[derive(Clone, Copy)]
pub enum Proto {
    Fixed(usize),
    /// printf-style: fixed count, plus one per conversion spec in the
    /// format string at this argument index
    Variadic(usize, usize),
}

pub fn libc_proto(name: &str) -> Option<Proto> {
    let n = name.trim_start_matches('_');
    let n = n.strip_prefix("isoc99_").unwrap_or(n);
    Some(match n {
        "puts" | "putchar" | "malloc" | "free" | "exit" | "strlen" | "atoi" | "abs" | "perror"
        | "fclose" | "rand" | "srand" | "time" => Proto::Fixed(1),
        "strcpy" | "strcmp" | "strcat" | "fopen" | "calloc" | "realloc" | "strchr" | "strrchr"
        | "strstr" | "fputs" | "atan2" | "pow" => Proto::Fixed(2),
        "memcpy" | "memset" | "memmove" | "strncpy" | "strncmp" | "strndup" | "fwrite"
        | "fread" | "qsort" => Proto::Fixed(3),
        "printf" | "scanf" => Proto::Variadic(1, 0),
        "fprintf" | "sprintf" | "fscanf" | "sscanf" => Proto::Variadic(2, 1),
        "snprintf" => Proto::Variadic(3, 2),
        "rand_r" => Proto::Fixed(1),
        _ => return None,
    })
}

fn count_format_args(fmt: &str) -> usize {
    let b: Vec<char> = fmt.chars().collect();
    let mut n = 0;
    let mut i = 0;
    while i < b.len() {
        if b[i] == '%' {
            if i + 1 < b.len() && b[i + 1] == '%' {
                i += 2;
                continue;
            }
            n += 1;
        }
        i += 1;
    }
    n
}

// ---------------------------------------------------------------- flags

/// Give every `jcc`/`setcc`/`cmovcc` the real boolean expression implied
/// by whatever last set the flags.
///
/// The original stuffed the condition into `Instr::Unknown { text:
/// "__cond__Ne" }` and always assumed the flag source was a `cmp`, so
/// `test eax,eax; je` came out as `eax == eax` — always true — and the
/// generated `main` took the wrong branch every time.
pub fn resolve_flags(instrs: &mut Vec<LiftedInsn>) {
    // index -> flag source in effect on entry to that instruction
    let mut in_effect: Vec<Option<FlagSrc>> = Vec::with_capacity(instrs.len());
    let mut cur: Option<FlagSrc> = None;
    for ins in instrs.iter() {
        in_effect.push(cur.clone());
        if let Some(f) = &ins.flags {
            cur = Some(f.clone());
        }
        // a call clobbers the flags
        if ins.is_call {
            cur = None;
        }
    }

    for (i, ins) in instrs.iter_mut().enumerate() {
        let Some(cc) = ins.cond else { continue };
        let Some(src) = &in_effect[i] else { continue };
        let cond = src.to_expr(cc);
        for st in ins.stmts.iter_mut() {
            match st {
                Stmt::If { cond: c, .. } => *c = cond.clone(),
                Stmt::Assign { src: s, .. } => {
                    if matches!(s, Expr::Unknown(u) if u == "__setcc__") {
                        // setcc materialises the flag as 0/1
                        *s = cond.clone();
                    } else if let Expr::Ternary { c, .. } = s {
                        if matches!(&**c, Expr::Unknown(u) if u == "__cmov__") {
                            *c = Box::new(cond.clone());
                        }
                    }
                }
                _ => {}
            }
        }
    }
}

// ---------------------------------------------- propagation environment

#[derive(Clone)]
struct Def {
    val: Expr,
    /// width the value was defined at
    size: u8,
}

#[derive(Default)]
struct Env {
    regs: HashMap<String, Def>,
}

impl Env {
    fn get(&self, r: &RegRef) -> Option<Expr> {
        let d = self.regs.get(&r.full)?;
        if r.high8 {
            return None;
        }
        if r.size == d.size {
            return Some(d.val.clone());
        }
        // A literal needs no width conversion, and a call returns in the
        // register the ABI says it does -- casting either just adds noise
        // (`abs_val((unsigned long)-7)` for what the source wrote as -7).
        if matches!(d.val, Expr::Const(_) | Expr::Call { .. } | Expr::Lit(_)) {
            return Some(d.val.clone());
        }
        if r.size < d.size {
            // narrowing read of a wider value: an explicit truncation
            return Some(Expr::cast(Type::from_width(r.size, true), d.val.clone()));
        }
        // widening read. Writing a 32-bit register zero-extends into the
        // full 64-bit one, which is the only widening the hardware makes
        // safe to assume.
        if d.size == 4 {
            return Some(Expr::cast(Type::Int { bits: 64, signed: false }, d.val.clone()));
        }
        None
    }

    /// The value in a register at whatever width it was written, with no
    /// width conversion — what a call argument wants.
    fn get_any(&self, full: &str) -> Option<Expr> {
        self.regs.get(full).map(|d| d.val.clone())
    }

    fn set(&mut self, r: &RegRef, val: Expr) {
        if r.high8 {
            self.regs.remove(&r.full);
            return;
        }
        self.regs.insert(r.full.clone(), Def { val, size: r.size });
    }

    #[allow(dead_code)]
    fn kill(&mut self, full: &str) {
        self.regs.remove(full);
    }

    /// Drop anything whose value came out of memory — a store may alias it.
    fn kill_memory_dependent(&mut self) {
        self.regs.retain(|_, d| !d.val.reads_mem());
    }

    fn kill_caller_saved(&mut self) {
        for r in CALLER_SAVED {
            self.regs.remove(r);
        }
        self.regs.retain(|_, d| !d.val.has_call());
    }
}

fn strip_casts(e: &Expr) -> Expr {
    match e {
        Expr::Cast { e, .. } => strip_casts(e),
        other => other.clone(),
    }
}

fn subst(e: Expr, env: &Env) -> Expr {
    match e {
        Expr::Reg(r) => match env.get(&r) {
            Some(v) => v,
            None => Expr::Reg(r),
        },
        Expr::Load { addr, ty } => Expr::Load { addr: Box::new(subst(*addr, env)), ty },
        Expr::AddrOf(e) => Expr::AddrOf(Box::new(subst(*e, env))),
        Expr::Bin { op, l, r } => {
            Expr::Bin { op, l: Box::new(subst(*l, env)), r: Box::new(subst(*r, env)) }
        }
        Expr::Un { op, e } => Expr::Un { op, e: Box::new(subst(*e, env)) },
        Expr::Cast { ty, e } => Expr::Cast { ty, e: Box::new(subst(*e, env)) },
        Expr::Index { base, idx } => {
            Expr::Index { base: Box::new(subst(*base, env)), idx: Box::new(subst(*idx, env)) }
        }
        Expr::Arrow { base, sid, fid } => {
            Expr::Arrow { base: Box::new(subst(*base, env)), sid, fid }
        }
        Expr::Dot { base, sid, fid } => Expr::Dot { base: Box::new(subst(*base, env)), sid, fid },
        Expr::Call { name, args, indirect } => Expr::Call {
            name,
            args: args.into_iter().map(|a| subst(a, env)).collect(),
            indirect: indirect.map(|i| Box::new(subst(*i, env))),
        },
        Expr::Ternary { c, t, f } => Expr::Ternary {
            c: Box::new(subst(*c, env)),
            t: Box::new(subst(*t, env)),
            f: Box::new(subst(*f, env)),
        },
        leaf => leaf,
    }
}

/// What we know about other functions in the binary, so their call sites
/// can be given the right number of arguments.
#[derive(Default, Clone)]
pub struct KnownFns {
    pub params: HashMap<String, usize>,
    pub returns: HashMap<String, bool>,
}

/// Forward propagation + call-argument recovery over one straight-line
/// run of instructions (a basic block).
/// Occurrences (not distinct names) of each register read, per statement
/// position, so a call result is only inlined into its single use. Without
/// this the call is emitted twice: once as its own statement and once
/// again wherever the result was consumed.
fn read_counts(instrs: &[LiftedInsn]) -> (Vec<HashMap<String, usize>>, Vec<Option<String>>) {
    let mut out = Vec::new();
    let mut defs = Vec::new();
    for ins in instrs {
        for st in &ins.stmts {
            defs.push(writes(st));
            let mut m: HashMap<String, usize> = HashMap::new();
            let mut collect = |e: &Expr| {
                e.walk(&mut |x| {
                    if let Expr::Reg(r) = x {
                        *m.entry(r.full.clone()).or_insert(0) += 1;
                    }
                })
            };
            match st {
                Stmt::Assign { dst, src } => {
                    collect(src);
                    if !matches!(dst, Expr::Reg(_)) {
                        collect(dst);
                    }
                }
                Stmt::Do(e) | Stmt::Return(Some(e)) | Stmt::If { cond: e, .. } => collect(e),
                _ => {}
            }
            out.push(m);
        }
    }
    (out, defs)
}

/// Reads of `reg` after position `here`, stopping at the next write to it —
/// counting past a redefinition would attribute a later value's readers to
/// this one, which silently deleted the earlier call.
fn uses_before_redef(
    counts: &[HashMap<String, usize>],
    defs: &[Option<String>],
    here: usize,
    reg: &str,
) -> usize {
    let mut n = 0;
    for i in here + 1..counts.len() {
        n += counts[i].get(reg).copied().unwrap_or(0);
        if defs[i].as_deref() == Some(reg) {
            break;
        }
    }
    n
}

pub fn propagate_block(
    instrs: &mut [LiftedInsn],
    known: &KnownFns,
    ret_width: Option<u8>,
    entry_env: &[(String, Expr, u8)],
) {
    let (counts, defs) = read_counts(instrs);
    let mut pos = 0usize;
    // statement positions (in the rebuilt list) holding a call whose
    // result got inlined elsewhere, to be deleted afterwards
    let mut drop_positions: Vec<(usize, usize)> = Vec::new();
    let mut env = Env::default();
    for (r, v, w) in entry_env {
        env.regs.insert(r.clone(), Def { val: v.clone(), size: *w });
    }
    // argument registers written since the last call, for the fallback
    // "contiguous prefix" argument-count rule
    let mut arg_set: HashSet<usize> = HashSet::new();
    // where each register was last defined, so a definition whose only
    // consumer turns out to be a call argument can be folded into the call
    let mut def_pos: HashMap<String, (usize, usize)> = HashMap::new();
    let mut def_flat: HashMap<String, usize> = HashMap::new();

    for (ins_i, ins) in instrs.iter_mut().enumerate() {
        let is_call = ins.is_call;
        let mut new_stmts: Vec<Stmt> = Vec::new();

        for st in std::mem::take(&mut ins.stmts) {
            let here = pos;
            pos += 1;
            match st {
                Stmt::Assign { dst, src } => {
                    let src = subst(src, &env);
                    let src = fold(src);
                    match dst {
                        Expr::Reg(r) => {
                            // A call result is bound to rax; keep the
                            // statement so DCE can decide, but also make
                            // the value visible to later reads.
                            if let Expr::Call { name, args, indirect } = src {
                                let (args, argc) =
                                    build_args(&name, args, &env, &arg_set, known);
                                for reg in SYSV_INT_ARGS.iter().take(argc) {
                                    let (Some(&at), Some(&p)) =
                                        (def_flat.get(*reg), def_pos.get(*reg))
                                    else {
                                        continue;
                                    };
                                    if uses_before_redef(&counts, &defs, at, reg) == 0 {
                                        drop_positions.push(p);
                                    }
                                }
                                let call = Expr::Call { name, args, indirect };
                                env.kill_caller_saved();
                                arg_set.clear();
                                // Inline the result only where it has
                                // exactly one later reader; otherwise leave
                                // it in the register so nothing is
                                // evaluated twice.
                                let uses = uses_before_redef(&counts, &defs, here, &r.full);
                                if uses == 1 {
                                    env.set(&r, call.clone());
                                    drop_positions.push((ins_i, new_stmts.len()));
                                } else {
                                    // still record where it was defined: a
                                    // later call may consume it as an
                                    // argument, and then this line is dead
                                    def_pos.insert(r.full.clone(), (ins_i, new_stmts.len()));
                                    def_flat.insert(r.full.clone(), here);
                                    env.set(&r, call.clone());
                                }
                                new_stmts.push(Stmt::Assign { dst: Expr::Reg(r), src: call });
                            } else {
                                if let Some(i) = sysv_arg_index(&r.full) {
                                    arg_set.insert(i);
                                }
                                def_pos.insert(r.full.clone(), (ins_i, new_stmts.len()));
                                def_flat.insert(r.full.clone(), here);
                                env.set(&r, src.clone());
                                new_stmts.push(Stmt::Assign { dst: Expr::Reg(r), src });
                            }
                        }
                        other => {
                            let dst = fold(subst(other, &env));
                            // a store may alias anything we loaded
                            env.kill_memory_dependent();
                            // `a1 = a1` from the prologue spill of an
                            // argument register carries no information
                            if dst == strip_casts(&src) {
                                continue;
                            }
                            new_stmts.push(Stmt::Assign { dst, src });
                        }
                    }
                }
                Stmt::Do(e) => {
                    let e = fold(subst(e, &env));
                    new_stmts.push(Stmt::Do(e));
                }
                Stmt::If { cond, target } => {
                    new_stmts.push(Stmt::If { cond: fold(subst(cond, &env)), target });
                }
                Stmt::Return(Some(e)) => {
                    new_stmts.push(Stmt::Return(Some(fold(subst(e, &env)))));
                }
                Stmt::Return(None) => {
                    // `ret` carries no operand; the returned value is
                    // whatever reached rax. The original hard-coded
                    // `return rax` and then guessed void-ness separately.
                    match ret_width {
                        Some(w) => {
                            let r = RegRef::new("rax", w);
                            let v = env.get(&r).unwrap_or(Expr::Reg(r));
                            new_stmts.push(Stmt::Return(Some(fold(v))));
                        }
                        None => new_stmts.push(Stmt::Return(None)),
                    }
                }
                other => new_stmts.push(other),
            }
        }
        ins.stmts = new_stmts;
        if is_call {
            arg_set.clear();
        }
        if let Some(f) = ins.flags.take() {
            ins.flags = Some(match f {
                FlagSrc::Cmp(a, b) => FlagSrc::Cmp(fold(subst(a, &env)), fold(subst(b, &env))),
                FlagSrc::Test(a, b) => FlagSrc::Test(fold(subst(a, &env)), fold(subst(b, &env))),
                FlagSrc::Logic(a) => FlagSrc::Logic(fold(subst(a, &env))),
            });
        }
    }

    for (i, j) in drop_positions {
        if let Some(st) = instrs[i].stmts.get_mut(j) {
            *st = Stmt::Nop;
        }
    }
}

/// Drop frame bookkeeping: the callee-saved push/pop pairs, `leave`, and
/// every assignment to rsp/rbp. `frame.rs` has already turned the frame
/// into named variables, so keeping the raw pointer arithmetic would only
/// add noise like `v1 = (rsp + 8 & -16) - 8 - 8;`.
pub fn strip_frame(instrs: &mut [LiftedInsn]) {
    for ins in instrs.iter_mut() {
        if ins.is_frame_setup {
            ins.stmts.retain(|s| matches!(s, Stmt::Return(_) | Stmt::Goto(_) | Stmt::If { .. }));
            continue;
        }
        ins.stmts.retain(|s| {
            !matches!(s, Stmt::Assign { dst: Expr::Reg(r), .. } if r.full == "rsp" || r.full == "rbp")
        });
    }
}

fn build_args(
    name: &str,
    existing: Vec<Expr>,
    env: &Env,
    arg_set: &HashSet<usize>,
    known: &KnownFns,
) -> (Vec<Expr>, usize) {
    if !existing.is_empty() {
        let n = existing.len();
        return (existing, n);
    }
    let reg_val = |i: usize| -> Option<Expr> { env.get_any(SYSV_INT_ARGS[i]) };

    let count = match libc_proto(name) {
        Some(Proto::Fixed(n)) => n,
        Some(Proto::Variadic(base, fmt_idx)) => {
            let extra = reg_val(fmt_idx)
                .and_then(|e| match e {
                    Expr::Lit(s) if s.starts_with('"') => {
                        Some(count_format_args(s.trim_matches('"')))
                    }
                    _ => None,
                })
                .unwrap_or(0);
            base + extra
        }
        None => match known.params.get(name) {
            Some(&n) => n,
            None => {
                // fall back to the longest contiguous run of argument
                // registers written since the previous call
                let mut n = 0;
                while n < SYSV_INT_ARGS.len() && arg_set.contains(&n) {
                    n += 1;
                }
                n
            }
        },
    };

    let mut args = Vec::new();
    for i in 0..count.min(SYSV_INT_ARGS.len()) {
        args.push(reg_val(i).unwrap_or_else(|| Expr::Reg(RegRef::new(SYSV_INT_ARGS[i], 8))));
    }
    let n = args.len();
    (args, n)
}

// -------------------------------------------------------------- folding

/// Algebraic and cast cleanup. Small, but it is the difference between
/// `(int)((long)v1 + 0) * 1` and `v1`.
pub fn fold(e: Expr) -> Expr {
    if let Expr::AddrOf(inner) = &e {
        if let Expr::Load { addr, .. } = &**inner {
            return (**addr).clone();
        }
    }
    // a cast that is no wider than the one inside it makes the inner one
    // unobservable: `(char)(unsigned int)(unsigned char)c` is `(char)(unsigned char)c`
    if let Expr::Cast { ty: Type::Int { bits: ob, .. }, e: inner } = &e {
        if let Expr::Cast { ty: Type::Int { bits: ib, .. }, e: deep } = &**inner {
            if ob <= ib {
                return Expr::Cast { ty: Type::Int { bits: *ob, signed: matches!(e, Expr::Cast { ty: Type::Int { signed: true, .. }, .. }) }, e: deep.clone() };
            }
        }
    }
    match e {
        Expr::Bin { op, l, r } => {
            let l = fold(*l);
            let r = fold(*r);
            if let (Some(a), Some(b)) = (l.as_const(), r.as_const()) {
                if let Some(v) = const_fold(op, a, b) {
                    return Expr::Const(v);
                }
            }
            match (op, l.as_const(), r.as_const()) {
                (BinOp::Add, _, Some(0))
                | (BinOp::Sub, _, Some(0))
                | (BinOp::Or, _, Some(0))
                | (BinOp::Xor, _, Some(0))
                | (BinOp::Shl, _, Some(0))
                | (BinOp::Shr, _, Some(0))
                | (BinOp::Sar, _, Some(0)) => return l,
                (BinOp::Mul, _, Some(1)) | (BinOp::Div, _, Some(1)) => return l,
                (BinOp::Add, Some(0), _) => return r,
                (BinOp::Mul, _, Some(0)) | (BinOp::And, _, Some(0)) => return Expr::Const(0),
                _ => {}
            }
            // x + (-c)  ->  x - c
            if op == BinOp::Add {
                if let Some(c) = r.as_const() {
                    if c < 0 {
                        return Expr::bin(BinOp::Sub, l, Expr::Const(-c));
                    }
                }
            }
            Expr::Bin { op, l: Box::new(l), r: Box::new(r) }
        }
        Expr::Un { op, e } => {
            let e = fold(*e);
            if let (UnOp::Neg, Some(c)) = (op, e.as_const()) {
                return Expr::Const(-c);
            }
            if let (UnOp::Not, Some(c)) = (op, e.as_const()) {
                return Expr::Const(!c);
            }
            Expr::Un { op, e: Box::new(e) }
        }
        Expr::Cast { ty, e } => {
            let e = fold(*e);
            // (T)(T)x  ->  (T)x
            if let Expr::Cast { ty: inner_ty, e: inner } = &e {
                if *inner_ty == ty {
                    return Expr::Cast { ty, e: inner.clone() };
                }
            }
            if let Some(c) = e.as_const() {
                if let Type::Int { bits, signed } = ty {
                    return Expr::Const(truncate(c, bits, signed));
                }
            }
            if let Expr::Bin { op, .. } = &e {
                if op.is_cmp() {
                    if let Type::Int { bits, .. } = ty {
                        if bits >= 8 {
                            return e;
                        }
                    }
                }
            }
            // (long)(int)v  ->  (long)v  when v is already at most 32 bits
            if let Expr::Cast { ty: it, e: inner } = &e {
                if let (Type::Int { bits: ob, .. }, Type::Int { bits: ib, .. }) = (&ty, it) {
                    if ib <= ob && natural_bits(inner).map(|n| n <= *ib).unwrap_or(false) {
                        return Expr::Cast { ty, e: inner.clone() };
                    }
                }
            }
            Expr::Cast { ty, e: Box::new(e) }
        }
        // *(&x)  ->  x
        Expr::Load { addr, ty } => {
            let addr = fold(*addr);
            if let Expr::AddrOf(inner) = &addr {
                return (**inner).clone();
            }
            Expr::Load { addr: Box::new(addr), ty }
        }
        // &(*x)  ->  x
        Expr::AddrOf(inner) => {
            let inner = fold(*inner);
            if let Expr::Load { addr, .. } = &inner {
                return (**addr).clone();
            }
            Expr::AddrOf(Box::new(inner))
        }
        Expr::Index { base, idx } => {
            Expr::Index { base: Box::new(fold(*base)), idx: Box::new(fold(*idx)) }
        }
        Expr::Arrow { base, sid, fid } => Expr::Arrow { base: Box::new(fold(*base)), sid, fid },
        Expr::Dot { base, sid, fid } => Expr::Dot { base: Box::new(fold(*base)), sid, fid },
        Expr::Call { name, args, indirect } => Expr::Call {
            name,
            args: args.into_iter().map(fold).collect(),
            indirect: indirect.map(|i| Box::new(fold(*i))),
        },
        Expr::Ternary { c, t, f } => {
            let c = fold(*c);
            if let Some(v) = c.as_const() {
                return if v != 0 { fold(*t) } else { fold(*f) };
            }
            Expr::Ternary { c: Box::new(c), t: Box::new(fold(*t)), f: Box::new(fold(*f)) }
        }
        leaf => leaf,
    }
}

/// Width an expression naturally has, when that is obvious from its form.
fn natural_bits(e: &Expr) -> Option<u16> {
    match e {
        Expr::Cast { ty: Type::Int { bits, .. }, .. } => Some(*bits),
        Expr::Load { ty: Type::Int { bits, .. }, .. } => Some(*bits),
        Expr::Reg(r) => Some(r.size as u16 * 8),
        _ => None,
    }
}

fn truncate(c: i64, bits: u16, signed: bool) -> i64 {
    match (bits, signed) {
        (8, true) => c as i8 as i64,
        (8, false) => c as u8 as i64,
        (16, true) => c as i16 as i64,
        (16, false) => c as u16 as i64,
        (32, true) => c as i32 as i64,
        (32, false) => c as u32 as i64,
        _ => c,
    }
}

fn const_fold(op: BinOp, a: i64, b: i64) -> Option<i64> {
    use BinOp::*;
    Some(match op {
        Add => a.wrapping_add(b),
        Sub => a.wrapping_sub(b),
        Mul => a.wrapping_mul(b),
        Div if b != 0 => a.wrapping_div(b),
        Rem if b != 0 => a.wrapping_rem(b),
        And => a & b,
        Or => a | b,
        Xor => a ^ b,
        Shl if (0..64).contains(&b) => a.wrapping_shl(b as u32),
        Sar if (0..64).contains(&b) => a.wrapping_shr(b as u32),
        Eq => (a == b) as i64,
        Ne => (a != b) as i64,
        Lt => (a < b) as i64,
        Le => (a <= b) as i64,
        Gt => (a > b) as i64,
        Ge => (a >= b) as i64,
        _ => return None,
    })
}

// ------------------------------------------------------------- liveness

/// Registers read by a statement.
pub fn reads(st: &Stmt, out: &mut HashSet<String>) {
    let mut collect = |e: &Expr| {
        e.walk(&mut |x| {
            if let Expr::Reg(r) = x {
                out.insert(r.full.clone());
            }
        })
    };
    match st {
        Stmt::Assign { dst, src } => {
            collect(src);
            // a write to a sub-register, or through memory, also reads
            match dst {
                Expr::Reg(r) if r.size < 8 && r.size != 4 => {
                    out.insert(r.full.clone());
                }
                Expr::Reg(_) => {}
                other => collect(other),
            }
        }
        Stmt::Do(e) | Stmt::Return(Some(e)) | Stmt::If { cond: e, .. } => collect(e),
        _ => {}
    }
}

pub fn writes(st: &Stmt) -> Option<String> {
    match st {
        Stmt::Assign { dst: Expr::Reg(r), .. } if !r.high8 => Some(r.full.clone()),
        _ => None,
    }
}

/// Remove register assignments whose value nothing reads. Runs backwards
/// through a block, seeded with the set live on exit.
pub fn dce_block(instrs: &mut [LiftedInsn], live_out: &HashSet<String>) -> HashSet<String> {
    let mut live = live_out.clone();
    for ins in instrs.iter_mut().rev() {
        let mut kept: Vec<Stmt> = Vec::new();
        for st in std::mem::take(&mut ins.stmts).into_iter().rev() {
            let w = writes(&st);
            let keep = match (&w, &st) {
                (Some(r), Stmt::Assign { src, .. }) => {
                    // keep if the target is live, or the value has a side
                    // effect we can't discard
                    live.contains(r) || src.has_call()
                }
                _ => true,
            };
            if keep {
                // a call whose result nothing reads becomes `f(...);`
                let st = match (&w, st) {
                    (Some(r), Stmt::Assign { dst, src })
                        if !live.contains(r) && matches!(src, Expr::Call { .. }) =>
                    {
                        let _ = dst;
                        Stmt::Do(src)
                    }
                    (_, other) => other,
                };
                if let Some(r) = &w {
                    live.remove(r);
                }
                let mut rs = HashSet::new();
                reads(&st, &mut rs);
                live.extend(rs);
                kept.push(st);
            }
        }
        kept.reverse();
        ins.stmts = kept;
    }
    live
}
