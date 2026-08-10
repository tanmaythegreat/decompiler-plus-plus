// frame.rs — variable recovery.
//
// This is the module the original didn't have at all. Without it every
// memory reference printed as its raw addressing mode (`*[rbp-0x4]`,
// `*[rax+0x8]`), which is the main reason the old output read like
// annotated assembly rather than C.
//
// What happens here:
//   1. find the frame base (rbp prologue, or a tracked rsp delta when
//      the compiler omits the frame pointer at -O1 and above),
//   2. collect every stack access with its width and whether anything
//      indexed into it,
//   3. cut the frame into slots, each sized by the gap to the next,
//   4. promote a slot to an array when something indexes it, and to a
//      pointer when its value is used as a base address,
//   5. synthesise a `struct` for each pointer whose target is touched at
//      more than one fixed offset,
//   6. bind slots that receive an argument register in the prologue to
//      named parameters,
//   7. rewrite every `Expr::Mem` into `v3`, `arr[i]`, `p->field_8`, or a
//      typed load.

use crate::ir::*;
use crate::lifter::{sysv_arg_index, CALLER_SAVED};
use std::collections::{BTreeMap, BTreeSet, HashMap};

/// Address -> what lives there, for RIP-relative references.
#[derive(Clone, Debug, Default)]
pub struct GlobalMap {
    pub names: HashMap<u64, String>,
    pub strings: HashMap<u64, String>,
    /// code addresses -> function name, so a function passed *as a value*
    /// (a callback, or `main` handed to `__libc_start_main`) reads as its
    /// name instead of a bare integer
    pub funcs: HashMap<u64, String>,
}

impl GlobalMap {
    fn describe(&self, addr: u64) -> Option<Expr> {
        if let Some(s) = self.strings.get(&addr) {
            return Some(Expr::Lit(format!("\"{}\"", s)));
        }
        self.names.get(&addr).map(|n| Expr::Lit(n.clone()))
    }
    fn is_string(&self, addr: u64) -> bool {
        self.strings.contains_key(&addr)
    }
}

#[derive(Clone, Debug)]
pub struct VarInfo {
    pub name: String,
    pub ty: Type,
    /// frame offset measured from rsp-at-entry, so the return address
    /// sits at 0 and locals are negative
    pub off: i64,
    pub size: usize,
    pub is_param: bool,
    pub param_index: usize,
    pub addr_taken: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum Origin {
    /// register holds the value loaded from this stack slot
    SlotVal(i64),
    /// register holds the address of this stack slot (from lea)
    SlotAddr(i64),
    /// register still holds an untouched incoming argument
    Param(usize),
}

#[derive(Default)]
struct Access {
    sizes: BTreeSet<u8>,
    index_scales: BTreeSet<u8>,
}

pub struct Frame {
    pub vars: Vec<VarInfo>,
    by_off: BTreeMap<i64, VarId>,
    /// var id / param index -> recovered struct layout
    ptr_struct: HashMap<VarId, usize>,
    param_struct: HashMap<usize, usize>,
    pub param_types: Vec<Type>,
    pub globals: GlobalMap,
    pub has_rbp_frame: bool,
    pub frame_size: i64,
}

/// Library functions known to hand back a pointer, used to type the
/// variable the result lands in.
fn returns_pointer(name: &str) -> bool {
    matches!(
        name,
        "malloc"
            | "calloc"
            | "realloc"
            | "strdup"
            | "strndup"
            | "fopen"
            | "memcpy"
            | "memmove"
            | "memset"
            | "strcpy"
            | "strncpy"
            | "strcat"
            | "mmap"
            | "getenv"
            | "fgets"
            | "strchr"
            | "strrchr"
            | "strstr"
    )
}

impl Frame {
    pub fn build(
        instrs: &[LiftedInsn],
        globals: GlobalMap,
        structs: &mut StructTable,
        fname: &str,
    ) -> Frame {
        let has_rbp_frame = detect_rbp_frame(instrs);
        let deltas = compute_rsp_delta(instrs);
        let frame_size = deltas.iter().copied().max().unwrap_or(0);

        let mut stack: BTreeMap<i64, Access> = BTreeMap::new();
        let mut fields: BTreeMap<Origin, BTreeMap<i64, BTreeSet<u8>>> = BTreeMap::new();
        let mut ptr_indexed: BTreeMap<Origin, BTreeSet<u8>> = BTreeMap::new();
        let mut ptr_slots: BTreeSet<i64> = BTreeSet::new();
        let mut ptr_typed: BTreeSet<i64> = BTreeSet::new();
        let mut str_typed: BTreeSet<i64> = BTreeSet::new();
        let mut addr_taken: BTreeSet<i64> = BTreeSet::new();
        let mut param_spill: BTreeMap<i64, usize> = BTreeMap::new();

        let mut origin: HashMap<String, Origin> = HashMap::new();
        let mut written_regs: BTreeSet<String> = BTreeSet::new();
        let mut seen_call = false;
        let mut last_call_ptr = false;

        for (i, ins) in instrs.iter().enumerate() {
            let delta = deltas.get(i).copied().unwrap_or(0);
            let foff = |mo: &MemOp| -> Option<i64> {
                let b = mo.base.as_ref()?;
                if b.full == "rbp" && has_rbp_frame {
                    Some(mo.disp - 8)
                } else if b.full == "rsp" {
                    Some(mo.disp - delta)
                } else {
                    None
                }
            };

            for st in &ins.stmts {
                // prologue spill of an incoming argument register
                if let Stmt::Assign { dst: Expr::Mem(mo), src: Expr::Reg(r) } = st {
                    if let (Some(off), Some(ai)) = (foff(mo), sysv_arg_index(&r.full)) {
                        if !seen_call && !written_regs.contains(&r.full) {
                            param_spill.entry(off).or_insert(ai);
                        }
                    }
                }
                // stack slot receiving a pointer-shaped value
                if let Stmt::Assign { dst: Expr::Mem(mo), src } = st {
                    if let Some(off) = foff(mo) {
                        match src {
                            Expr::AddrOf(inner) => {
                                ptr_typed.insert(off);
                                if let Expr::Mem(m2) = &**inner {
                                    if m2.rip_abs.map(|a| globals.is_string(a)).unwrap_or(false) {
                                        str_typed.insert(off);
                                    }
                                }
                            }
                            Expr::Call { name, .. } if returns_pointer(name) => {
                                ptr_typed.insert(off);
                            }
                            Expr::Reg(r) if r.full == "rax" && last_call_ptr => {
                                ptr_typed.insert(off);
                            }
                            _ => {}
                        }
                    }
                }
                // pointer-origin tracking through register moves
                if let Stmt::Assign { dst: Expr::Reg(d), src } = st {
                    let o = match src {
                        Expr::Reg(s) => origin.get(&s.full).copied().or_else(|| {
                            sysv_arg_index(&s.full)
                                .filter(|_| !seen_call && !written_regs.contains(&s.full))
                                .map(Origin::Param)
                        }),
                        Expr::Mem(mo) => {
                            foff(mo).filter(|_| mo.index.is_none()).map(Origin::SlotVal)
                        }
                        Expr::AddrOf(inner) => match &**inner {
                            Expr::Mem(mo) => foff(mo).map(|off| {
                                addr_taken.insert(off);
                                Origin::SlotAddr(off)
                            }),
                            _ => None,
                        },
                        _ => None,
                    };
                    match o {
                        Some(v) => {
                            origin.insert(d.full.clone(), v);
                        }
                        None => {
                            origin.remove(&d.full);
                        }
                    }
                    written_regs.insert(d.full.clone());
                }

                each_mem(st, &mut |mo| {
                    if mo.rip_abs.is_some() {
                        return;
                    }
                    if let Some(off) = foff(mo) {
                        let e = stack.entry(off).or_default();
                        if mo.index.is_some() {
                            e.index_scales.insert(mo.scale.max(1));
                        } else {
                            e.sizes.insert(mo.size);
                        }
                        return;
                    }
                    let Some(b) = &mo.base else { return };
                    if b.full == "rsp" || b.full == "rbp" {
                        return;
                    }
                    let Some(&o) = origin.get(&b.full) else { return };
                    if let Origin::SlotAddr(base_off) = o {
                        // storage is the stack slot itself
                        let e = stack.entry(base_off).or_default();
                        if mo.index.is_some() {
                            e.index_scales.insert(mo.scale.max(1));
                        } else {
                            e.sizes.insert(mo.size);
                            if mo.disp != 0 {
                                stack
                                    .entry(base_off + mo.disp)
                                    .or_default()
                                    .sizes
                                    .insert(mo.size);
                            }
                        }
                        return;
                    }
                    if let Origin::SlotVal(slot) = o {
                        ptr_slots.insert(slot);
                    }
                    if mo.index.is_some() {
                        ptr_indexed.entry(o).or_default().insert(mo.scale.max(1));
                    } else {
                        fields.entry(o).or_default().entry(mo.disp).or_default().insert(mo.size);
                    }
                });
            }

            if ins.is_call {
                seen_call = true;
                last_call_ptr = ins.stmts.iter().any(|s| {
                    matches!(s, Stmt::Assign { src: Expr::Call { name, .. }, .. } if returns_pointer(name))
                });
                for r in CALLER_SAVED {
                    origin.remove(r);
                }
            }
        }

        // ---- cut the frame into slots -----------------------------------
        let offs: Vec<i64> = stack.keys().copied().collect();
        let mut vars: Vec<VarInfo> = Vec::new();
        let mut by_off: BTreeMap<i64, VarId> = BTreeMap::new();
        let mut local_n = 0usize;

        for (i, &off) in offs.iter().enumerate() {
            let acc = &stack[&off];
            let max_size = acc.sizes.iter().copied().max().unwrap_or(0);
            let extent = match offs.get(i + 1) {
                Some(&n) => (n - off).max(1) as usize,
                None => max_size.max(1) as usize,
            };

            let ty = if let Some(&scale) = acc.index_scales.iter().max() {
                let elem = Type::from_width(scale, true);
                let n = (extent / scale.max(1) as usize).max(1);
                Type::Array(Box::new(elem), n)
            } else if str_typed.contains(&off) {
                Type::char_ptr()
            } else if ptr_slots.contains(&off) || ptr_typed.contains(&off) {
                Type::void_ptr()
            } else {
                let w = max_size.max(1);
                let w = if w.is_power_of_two() && w <= 8 { w } else { 8 };
                Type::from_width(w.min(extent.max(1) as u8), true)
            };

            let (name, is_param, pidx) = match param_spill.get(&off) {
                Some(&ai) => (format!("a{}", ai + 1), true, ai),
                None if off >= 8 => (format!("sa{}", off / 8), false, 0),
                None => {
                    local_n += 1;
                    (format!("v{}", local_n), false, 0)
                }
            };

            by_off.insert(off, vars.len());
            vars.push(VarInfo {
                name,
                ty,
                off,
                size: extent,
                is_param,
                param_index: pidx,
                addr_taken: addr_taken.contains(&off),
            });
        }

        // ---- synthesise structs ------------------------------------------
        let mut ptr_struct: HashMap<VarId, usize> = HashMap::new();
        let mut param_struct: HashMap<usize, usize> = HashMap::new();

        // Only aggregates whose *address* is a frame slot are decided here.
        // Anything reached through a pointer register is left for ptr.rs:
        // at -O0 gcc destroys the base register while computing the address,
        // so the addressing modes seen at this stage are an unreliable and
        // often contradictory view of the same object.
        for (o, layout) in &fields {
            let interesting = layout.len() > 1 || layout.keys().any(|&k| k != 0);
            if !interesting || !matches!(o, Origin::SlotAddr(_)) {
                continue;
            }
            let sid = make_struct(structs, fname, layout);
            match o {
                Origin::SlotVal(off) => {
                    if let Some(&vid) = by_off.get(off) {
                        vars[vid].ty = Type::ptr(Type::Struct(sid));
                        ptr_struct.insert(vid, sid);
                    }
                }
                Origin::Param(i) => {
                    param_struct.insert(*i, sid);
                }
                Origin::SlotAddr(_) => {}
            }
        }

        for (o, scales) in &ptr_indexed {
            let scale = scales.iter().copied().max().unwrap_or(1);
            if let Origin::SlotVal(off) = o {
                if let Some(&vid) = by_off.get(off) {
                    if !ptr_struct.contains_key(&vid) {
                        vars[vid].ty = Type::ptr(Type::from_width(scale, true));
                    }
                }
            }
        }

        // ---- parameter types ---------------------------------------------
        let nparams = param_spill.values().copied().max().map(|m| m + 1).unwrap_or(0);
        let mut param_types = Vec::new();
        for i in 0..nparams {
            let ty = if let Some(&sid) = param_struct.get(&i) {
                Type::ptr(Type::Struct(sid))
            } else if let Some(v) = vars.iter().find(|v| v.is_param && v.param_index == i) {
                v.ty.clone()
            } else {
                Type::int(32)
            };
            param_types.push(ty);
        }

        Frame {
            vars,
            by_off,
            ptr_struct,
            param_struct,
            param_types,
            globals,
            has_rbp_frame,
            frame_size,
        }
    }

    /// Optimised builds never spill their arguments, so the frame holds no
    /// trace of them. Recover them instead from argument registers that are
    /// read before anything in the function writes them.
    pub fn recover_register_params(&mut self, instrs: &[LiftedInsn]) {
        if !self.param_types.is_empty() {
            return;
        }
        let mut written: HashMap<String, bool> = HashMap::new();
        let mut live_in: HashMap<String, u8> = HashMap::new();
        for ins in instrs {
            for st in &ins.stmts {
                let mut note_read = |e: &Expr| {
                    e.walk(&mut |x| {
                        if let Expr::Reg(r) = x {
                            if !written.get(&r.full).copied().unwrap_or(false) {
                                let w = live_in.entry(r.full.clone()).or_insert(r.size);
                                *w = (*w).max(r.size);
                            }
                        }
                    });
                };
                match st {
                    Stmt::Assign { dst, src } => {
                        note_read(src);
                        match dst {
                            Expr::Reg(r) => {
                                written.entry(r.full.clone()).or_insert(true);
                            }
                            other => note_read(other),
                        }
                    }
                    Stmt::Do(e) | Stmt::Return(Some(e)) | Stmt::If { cond: e, .. } => note_read(e),
                    _ => {}
                }
            }
        }

        // only a contiguous prefix of the argument registers can be real
        for (i, reg) in crate::lifter::SYSV_INT_ARGS.iter().enumerate() {
            let Some(&w) = live_in.get(*reg) else { break };
            let vid = self.vars.len();
            self.vars.push(VarInfo {
                name: format!("a{}", i + 1),
                ty: Type::from_width(w.max(4), true),
                off: 0,
                size: w as usize,
                is_param: true,
                param_index: i,
                addr_taken: false,
            });
            self.param_types.push(self.vars[vid].ty.clone());
        }
    }

    pub fn param_count(&self) -> usize {
        self.param_types.len()
    }

    pub fn locals(&self) -> Vec<&VarInfo> {
        let mut v: Vec<&VarInfo> = self.vars.iter().filter(|v| !v.is_param).collect();
        v.sort_by_key(|v| -v.off);
        v
    }

    pub fn var(&self, id: VarId) -> &VarInfo {
        &self.vars[id]
    }

    fn containing(&self, off: i64) -> Option<(VarId, i64)> {
        if let Some(&id) = self.by_off.get(&off) {
            return Some((id, 0));
        }
        for (&o, &id) in self.by_off.iter().rev() {
            if o <= off && off < o + self.vars[id].size as i64 {
                return Some((id, off - o));
            }
        }
        None
    }

    // -------------------------------------------------- rewrite pass

    /// Replace every `Expr::Mem` in the function with a variable, array
    /// element, struct field, or typed load. Re-runs the same
    /// pointer-origin scan used during construction so a memory operand
    /// can be attributed to the variable whose value is in its base
    /// register.
    pub fn rewrite(&self, instrs: &mut Vec<LiftedInsn>, st: &StructTable) {
        let deltas = compute_rsp_delta(instrs);
        let mut origin: HashMap<String, Origin> = HashMap::new();
        let mut written: BTreeSet<String> = BTreeSet::new();
        let mut seen_call = false;

        for (i, ins) in instrs.iter_mut().enumerate() {
            let delta = deltas.get(i).copied().unwrap_or(0);

            // update origins from this instruction *before* rewriting it,
            // using the pre-rewrite (register/mem) form
            let mut updates: Vec<(String, Option<Origin>)> = Vec::new();
            for s in &ins.stmts {
                if let Stmt::Assign { dst: Expr::Reg(d), src } = s {
                    let o = match src {
                        Expr::Reg(sr) => origin.get(&sr.full).copied().or_else(|| {
                            sysv_arg_index(&sr.full)
                                .filter(|_| !seen_call && !written.contains(&sr.full))
                                .map(Origin::Param)
                        }),
                        Expr::Mem(mo) => self
                            .stack_off(mo, delta)
                            .filter(|_| mo.index.is_none())
                            .map(Origin::SlotVal),
                        Expr::AddrOf(inner) => match &**inner {
                            Expr::Mem(mo) => self.stack_off(mo, delta).map(Origin::SlotAddr),
                            _ => None,
                        },
                        _ => None,
                    };
                    updates.push((d.full.clone(), o));
                }
            }

            let ctx = RewriteCtx { frame: self, st, origin: &origin, delta };
            for s in ins.stmts.iter_mut() {
                rewrite_stmt(s, &ctx);
            }
            if let Some(f) = ins.flags.take() {
                ins.flags = Some(match f {
                    FlagSrc::Cmp(a, b) => FlagSrc::Cmp(ctx.expr(a), ctx.expr(b)),
                    FlagSrc::Test(a, b) => FlagSrc::Test(ctx.expr(a), ctx.expr(b)),
                    FlagSrc::Logic(a) => FlagSrc::Logic(ctx.expr(a)),
                });
            }

            for (r, o) in updates {
                match o {
                    Some(v) => {
                        origin.insert(r.clone(), v);
                    }
                    None => {
                        origin.remove(&r);
                    }
                }
                written.insert(r);
            }
            if ins.is_call {
                seen_call = true;
                for r in CALLER_SAVED {
                    origin.remove(r);
                }
            }
        }
    }

    pub fn stack_off(&self, mo: &MemOp, delta: i64) -> Option<i64> {
        let b = mo.base.as_ref()?;
        if mo.rip_abs.is_some() {
            return None;
        }
        if b.full == "rbp" && self.has_rbp_frame {
            Some(mo.disp - 8)
        } else if b.full == "rsp" {
            Some(mo.disp - delta)
        } else {
            None
        }
    }
}

struct RewriteCtx<'a> {
    frame: &'a Frame,
    st: &'a StructTable,
    origin: &'a HashMap<String, Origin>,
    delta: i64,
}

impl<'a> RewriteCtx<'a> {
    fn expr(&self, e: Expr) -> Expr {
        match e {
            // `lea` produces AddrOf(Mem(..)); resolve it as an address
            Expr::AddrOf(inner) => match *inner {
                Expr::Mem(mo) => self.addr_of(&mo),
                other => Expr::AddrOf(Box::new(self.expr(other))),
            },
            Expr::Mem(mo) => self.load(&mo),
            Expr::Load { addr, ty } => Expr::Load { addr: Box::new(self.expr(*addr)), ty },
            Expr::Bin { op, l, r } => {
                Expr::Bin { op, l: Box::new(self.expr(*l)), r: Box::new(self.expr(*r)) }
            }
            Expr::Un { op, e } => Expr::Un { op, e: Box::new(self.expr(*e)) },
            Expr::Cast { ty, e } => Expr::Cast { ty, e: Box::new(self.expr(*e)) },
            Expr::Index { base, idx } => {
                Expr::Index { base: Box::new(self.expr(*base)), idx: Box::new(self.expr(*idx)) }
            }
            Expr::Arrow { base, sid, fid } => {
                Expr::Arrow { base: Box::new(self.expr(*base)), sid, fid }
            }
            Expr::Dot { base, sid, fid } => {
                Expr::Dot { base: Box::new(self.expr(*base)), sid, fid }
            }
            Expr::Call { name, args, indirect } => Expr::Call {
                name,
                args: args.into_iter().map(|a| self.expr(a)).collect(),
                indirect: indirect.map(|i| Box::new(self.expr(*i))),
            },
            Expr::Ternary { c, t, f } => Expr::Ternary {
                c: Box::new(self.expr(*c)),
                t: Box::new(self.expr(*t)),
                f: Box::new(self.expr(*f)),
            },
            leaf => leaf,
        }
    }

    fn base_origin(&self, mo: &MemOp) -> Option<Origin> {
        let b = mo.base.as_ref()?;
        if b.full == "rsp" || b.full == "rbp" {
            return None;
        }
        self.origin.get(&b.full).copied()
    }

    fn load(&self, mo: &MemOp) -> Expr {
        let ty = Type::from_width(mo.size, mo.signed);

        if let Some(abs) = mo.rip_abs {
            if let Some(g) = self.frame.globals.describe(abs) {
                return Expr::Load { addr: Box::new(Expr::AddrOf(Box::new(g))), ty };
            }
            return Expr::Load {
                addr: Box::new(Expr::cast(Type::ptr(ty.clone()), Expr::Const(abs as i64))),
                ty,
            };
        }

        // stack-relative
        if let Some(off) = self.frame.stack_off(mo, self.delta) {
            if let Some(e) = self.stack_ref(off, mo) {
                return e;
            }
        }
        // through a tracked pointer
        if let Some(o) = self.base_origin(mo) {
            if let Some(e) = self.pointer_ref(mo, o) {
                return e;
            }
        }
        Expr::Load { addr: Box::new(self.raw_addr(mo)), ty }
    }

    fn addr_of(&self, mo: &MemOp) -> Expr {
        if let Some(abs) = mo.rip_abs {
            if let Some(g) = self.frame.globals.describe(abs) {
                return g;
            }
            return Expr::Const(abs as i64);
        }
        if mo.base.is_none() && mo.index.is_none() {
            return Expr::Const(mo.disp);
        }
        match self.load(mo) {
            Expr::Load { addr, .. } => match *addr {
                Expr::Cast { e, .. } => *e,
                other => other,
            },
            Expr::Index { base, idx } => {
                if idx.as_const() == Some(0) {
                    *base
                } else {
                    Expr::AddrOf(Box::new(Expr::Index { base, idx }))
                }
            }
            other => Expr::AddrOf(Box::new(other)),
        }
    }

    fn stack_ref(&self, off: i64, mo: &MemOp) -> Option<Expr> {
        let (vid, delta) = self.frame.containing(off)?;
        let v = self.frame.var(vid);
        if let Some(idx) = &mo.index {
            if let Type::Array(elem, _) = &v.ty {
                let esz = elem.size(self.st).max(1) as i64;
                let mut i = Expr::Reg(idx.clone());
                let scale = mo.scale.max(1) as i64;
                if scale != esz {
                    i = Expr::bin(BinOp::Mul, i, Expr::Const(scale / esz.max(1)));
                }
                if delta != 0 {
                    i = Expr::bin(BinOp::Add, i, Expr::Const(delta / esz.max(1)));
                }
                return Some(Expr::Index { base: Box::new(Expr::Var(vid)), idx: Box::new(i) });
            }
            return None;
        }
        if let Type::Array(elem, _) = &v.ty {
            let esz = elem.size(self.st).max(1) as i64;
            return Some(Expr::Index {
                base: Box::new(Expr::Var(vid)),
                idx: Box::new(Expr::Const(delta / esz)),
            });
        }
        if delta == 0 {
            return Some(Expr::Var(vid));
        }
        // partial access into a wider slot
        Some(Expr::Load {
            addr: Box::new(Expr::cast(
                Type::ptr(Type::from_width(mo.size, mo.signed)),
                Expr::bin(
                    BinOp::Add,
                    Expr::cast(Type::char_ptr(), Expr::AddrOf(Box::new(Expr::Var(vid)))),
                    Expr::Const(delta),
                ),
            )),
            ty: Type::from_width(mo.size, mo.signed),
        })
    }

    fn pointer_ref(&self, mo: &MemOp, o: Origin) -> Option<Expr> {
        let (base, sid) = match o {
            Origin::SlotVal(off) => {
                let vid = *self.frame.by_off.get(&off)?;
                (Expr::Var(vid), self.frame.ptr_struct.get(&vid).copied())
            }
            Origin::Param(i) => (
                Expr::Lit(format!("a{}", i + 1)),
                self.frame.param_struct.get(&i).copied(),
            ),
            Origin::SlotAddr(off) => {
                let vid = *self.frame.by_off.get(&off)?;
                let v = self.frame.var(vid);
                if let Some(idx) = &mo.index {
                    let esz = match &v.ty {
                        Type::Array(e, _) => e.size(self.st).max(1) as i64,
                        _ => mo.scale.max(1) as i64,
                    };
                    let mut i = Expr::Reg(idx.clone());
                    let scale = mo.scale.max(1) as i64;
                    if scale != esz {
                        i = Expr::bin(BinOp::Mul, i, Expr::Const(scale / esz.max(1)));
                    }
                    if mo.disp != 0 {
                        i = Expr::bin(BinOp::Add, i, Expr::Const(mo.disp / esz.max(1)));
                    }
                    return Some(Expr::Index {
                        base: Box::new(Expr::Var(vid)),
                        idx: Box::new(i),
                    });
                }
                if mo.disp == 0 {
                    return Some(match &v.ty {
                        Type::Array(..) => Expr::Index {
                            base: Box::new(Expr::Var(vid)),
                            idx: Box::new(Expr::Const(0)),
                        },
                        _ => Expr::Var(vid),
                    });
                }
                let esz = match &v.ty {
                    Type::Array(e, _) => e.size(self.st).max(1) as i64,
                    _ => 1,
                };
                return Some(Expr::Index {
                    base: Box::new(Expr::Var(vid)),
                    idx: Box::new(Expr::Const(mo.disp / esz)),
                });
            }
        };

        if let Some(idx) = &mo.index {
            let mut i = Expr::Reg(idx.clone());
            if mo.disp != 0 {
                i = Expr::bin(BinOp::Add, i, Expr::Const(mo.disp / mo.scale.max(1) as i64));
            }
            return Some(Expr::Index { base: Box::new(base), idx: Box::new(i) });
        }
        if let Some(sid) = sid {
            if let Some(fid) = self.st.field_at(sid, mo.disp) {
                return Some(Expr::Arrow { base: Box::new(base), sid, fid });
            }
        }
        if mo.disp == 0 {
            // Leave this as a plain load rather than committing to `p[0]`:
            // the late pointer pass still has to decide whether this base
            // is an array, a struct pointer, or a scalar pointer, and it
            // can only see accesses that are still Loads.
            return Some(Expr::Load {
                addr: Box::new(base),
                ty: Type::from_width(mo.size, mo.signed),
            });
        }
        None
    }

    fn raw_addr(&self, mo: &MemOp) -> Expr {
        let mut e: Option<Expr> = mo.base.as_ref().map(|b| Expr::Reg(b.clone()));
        if let Some(ix) = &mo.index {
            let scaled = if mo.scale > 1 {
                Expr::bin(BinOp::Mul, Expr::Reg(ix.clone()), Expr::Const(mo.scale as i64))
            } else {
                Expr::Reg(ix.clone())
            };
            e = Some(match e {
                Some(b) => Expr::bin(BinOp::Add, b, scaled),
                None => scaled,
            });
        }
        let mut a = e.unwrap_or(Expr::Const(0));
        if mo.disp != 0 {
            a = if mo.disp < 0 {
                Expr::bin(BinOp::Sub, a, Expr::Const(-mo.disp))
            } else {
                Expr::bin(BinOp::Add, a, Expr::Const(mo.disp))
            };
        }
        Expr::cast(Type::ptr(Type::from_width(mo.size, mo.signed)), a)
    }
}

fn rewrite_stmt(s: &mut Stmt, ctx: &RewriteCtx) {
    let taken = std::mem::replace(s, Stmt::Nop);
    *s = match taken {
        Stmt::Assign { dst, src } => Stmt::Assign { dst: ctx.expr(dst), src: ctx.expr(src) },
        Stmt::Do(e) => Stmt::Do(ctx.expr(e)),
        Stmt::Return(Some(e)) => Stmt::Return(Some(ctx.expr(e))),
        Stmt::If { cond, target } => Stmt::If { cond: ctx.expr(cond), target },
        other => other,
    };
}

fn make_struct(st: &mut StructTable, fname: &str, layout: &BTreeMap<i64, BTreeSet<u8>>) -> usize {
    let mut fields: Vec<Field> = Vec::new();
    let mut cursor: i64 = 0;
    for (&off, sizes) in layout {
        let size = sizes.iter().copied().max().unwrap_or(1);
        if off > cursor {
            fields.push(Field {
                off: cursor,
                ty: Type::Array(
                    Box::new(Type::Int { bits: 8, signed: true }),
                    (off - cursor) as usize,
                ),
                name: format!("pad_{:x}", cursor),
            });
        }
        if off < cursor {
            continue;
        }
        fields.push(Field {
            off,
            ty: Type::from_width(size, true),
            name: format!("field_{:x}", off),
        });
        cursor = off + size as i64;
    }
    let name = format!("{}_s{}", fname, st.defs.len());
    st.add(StructDef { name, fields, size: cursor.max(0) as usize })
}

fn detect_rbp_frame(instrs: &[LiftedInsn]) -> bool {
    let mut saw_push_rbp = false;
    for ins in instrs.iter().take(8) {
        for st in &ins.stmts {
            match st {
                // the store half of `push rbp`: *[rsp+0] = rbp
                Stmt::Assign { dst: Expr::Mem(mo), src: Expr::Reg(r) }
                if r.full == "rbp"
                    && mo.disp == 0
                    && mo.base.as_ref().map_or(false, |b| b.full == "rsp") =>
                    {
                        saw_push_rbp = true;
                    }
                Stmt::Assign { dst: Expr::Reg(d), src: Expr::Reg(s) }
                if saw_push_rbp && d.full == "rbp" && s.full == "rsp" =>
                    {
                        return true;
                    }
                _ => {}
            }
        }
    }
    false
}

/// rsp displacement below its entry value, per instruction index.
/// Needed for frame-pointer-less code, where locals are addressed as
/// `[rsp+k]` and `k` means different things either side of `sub rsp, N`.
fn compute_rsp_delta(instrs: &[LiftedInsn]) -> Vec<i64> {
    let mut out = Vec::with_capacity(instrs.len());
    let mut delta: i64 = 0;
    for ins in instrs {
        out.push(delta);
        let t = ins.asm_text.replace(' ', "");
        if t.starts_with("push") {
            delta += 8;
        } else if t.starts_with("pop") {
            delta -= 8;
        } else if let Some(rest) = t.strip_prefix("subrsp,") {
            if let Some(v) = parse_num(rest) {
                delta += v;
            }
        } else if let Some(rest) = t.strip_prefix("addrsp,") {
            if let Some(v) = parse_num(rest) {
                delta -= v;
            }
        } else if t.starts_with("leave") {
            delta = 8;
        }
    }
    out
}

fn parse_num(s: &str) -> Option<i64> {
    let s = s.trim();
    let s = s.strip_suffix('h').unwrap_or(s);
    i64::from_str_radix(s, 16).ok()
}

fn each_mem<F: FnMut(&MemOp)>(st: &Stmt, f: &mut F) {
    let mut visit = |e: &Expr| {
        e.walk(&mut |x| {
            if let Expr::Mem(mo) = x {
                f(mo);
            }
        })
    };
    match st {
        Stmt::Assign { dst, src } => {
            visit(dst);
            visit(src);
        }
        Stmt::Do(e) | Stmt::Return(Some(e)) | Stmt::If { cond: e, .. } => visit(e),
        _ => {}
    }
}
