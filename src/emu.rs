// emu.rs — a stepper for the lifted IR.
//
// It executes the pre-simplification instruction stream, which is a faithful
// statement-level model of the machine: the analysed stream has had its frame
// folded away and its dead stores removed, so it no longer describes what the
// processor would do.
//
// Imported functions are not present in the image, so a handful of libc calls
// are modelled directly. That is a deliberate limit, not an oversight: without
// it every `printf` argument is a dangling pointer and nothing observable
// happens.

use crate::analysis::Program;
use crate::ir::*;
use std::collections::{HashMap, HashSet};

pub const STACK_TOP: u64 = 0x7fff_ffff_f000;
pub const HEAP_BASE: u64 = 0x0000_6000_0000_0000;
const SENTINEL: u64 = 0xdead_beef_dead_beef;
pub const ARG_REGS: [&str; 6] = ["rdi", "rsi", "rdx", "rcx", "r8", "r9"];

pub struct Emu {
    pub regs: HashMap<String, u64>,
    pub prev: HashMap<String, u64>,
    pub mem: HashMap<u64, u8>,
    pub pc: Option<u64>,
    pub out: String,
    pub halted: bool,
    pub reason: String,
    pub steps: u64,
    heap: u64,
    depth: u32,
    index: HashMap<u64, (usize, usize)>,
    pub breakpoints: HashSet<u64>,
    pub hit_breakpoint: Option<u64>,
    /// text the emulated program reads from stdin
    pub stdin: String,
    stdin_pos: usize,
    pub regions: Vec<Region>,
    heap_top: u64,
}

/// One mapped range, for the memory map view and for saying what a value
/// points at.
#[derive(Clone)]
pub struct Region {
    pub start: u64,
    pub end: u64,
    pub name: String,
    pub perm: &'static str,
}

/// What a 64-bit value looks like it is.
pub enum Kind {
    Zero,
    Small(i64),
    Pointer { region: String, extra: String },
    Unknown,
}

impl Kind {
    pub fn describe(&self) -> String {
        match self {
            Kind::Zero => String::new(),
            Kind::Small(v) => format!("{}", v),
            Kind::Pointer { region, extra } => {
                if extra.is_empty() {
                    format!("-> {}", region)
                } else {
                    format!("-> {}  {}", region, extra)
                }
            }
            Kind::Unknown => String::new(),
        }
    }
}

pub const SHOWN_REGS: [&str; 16] = [
    "rax", "rbx", "rcx", "rdx", "rsi", "rdi", "rbp", "rsp", "r8", "r9", "r10", "r11", "r12", "r13",
    "r14", "r15",
];

impl Emu {
    pub fn new(prog: &Program, entry: usize) -> Emu {
        let mut e = Emu {
            regs: HashMap::new(),
            prev: HashMap::new(),
            mem: HashMap::new(),
            pc: None,
            out: String::new(),
            halted: false,
            reason: String::new(),
            steps: 0,
            heap: HEAP_BASE,
            depth: 0,
            index: HashMap::new(),
            breakpoints: HashSet::new(),
            hit_breakpoint: None,
            stdin: String::new(),
            stdin_pos: 0,
            regions: Vec::new(),
            heap_top: HEAP_BASE,
        };
        for r in SHOWN_REGS {
            e.regs.insert(r.to_string(), 0);
        }
        e.regs.insert("rsp".into(), STACK_TOP);
        e.regs.insert("rbp".into(), STACK_TOP);

        for (_, base, data) in &prog.memory {
            for (i, b) in data.iter().enumerate() {
                e.mem.insert(base + i as u64, *b);
            }
        }
        for (fi, f) in prog.funcs.iter().enumerate() {
            for (ii, ins) in f.raw.iter().enumerate() {
                e.index.insert(ins.addr, (fi, ii));
            }
        }
        for (name, base, data) in &prog.memory {
            e.regions.push(Region {
                start: *base,
                end: base + data.len() as u64,
                name: format!("{} [{}]", name, short_name(&prog.path)),
                perm: perm_of(name),
            });
        }
        e.regions.push(Region {
            start: STACK_TOP - 0x21000,
            end: STACK_TOP + 0x1000,
            name: "[stack]".into(),
            perm: "rw-",
        });
        e.regions.push(Region {
            start: HEAP_BASE,
            end: HEAP_BASE,
            name: "[heap]".into(),
            perm: "rw-",
        });
        e.regions.sort_by_key(|r| r.start);

        e.push(SENTINEL);
        e.pc = prog.funcs.get(entry).map(|f| f.addr);
        e
    }

    fn sync_heap(&mut self) {
        let top = self.heap;
        if let Some(r) = self.regions.iter_mut().find(|r| r.name == "[heap]") {
            r.end = top;
        }
    }

    pub fn region_of(&self, a: u64) -> Option<&Region> {
        self.regions.iter().find(|r| a >= r.start && a < r.end.max(r.start + 1))
    }

    /// pwndbg-style: say what a value actually is.
    pub fn classify(&self, v: u64, prog: &Program) -> Kind {
        if v == 0 {
            return Kind::Zero;
        }
        // a value that is only a small number is not worth annotating

        if let Some(f) = prog.funcs.iter().find(|f| f.addr == v) {
            return Kind::Pointer { region: ".text".into(), extra: format!("<{}>", f.name) };
        }
        if let Some(n) = prog.symbols.get(&v) {
            return Kind::Pointer { region: ".text".into(), extra: format!("<{}>", n) };
        }
        if let Some(r) = self.region_of(v) {
            let s = self.cstr(v);
            let printable = !s.is_empty()
                && s.len() >= 2
                && s.bytes().all(|b| (0x20..0x7f).contains(&b) || b == b'\n' || b == b'\t');
            let extra = if printable {
                let cut: String = s.chars().take(28).collect();
                format!("\"{}\"", cut.escape_debug())
            } else {
                format!("{:#x}", self.read(v, 8))
            };
            return Kind::Pointer { region: r.name.clone(), extra };
        }
        let s = v as i64;
        if (-1_000_000..1_000_000).contains(&s) {
            return Kind::Small(s);
        }
        Kind::Unknown
    }

    /// Put a value in the register the ABI uses for argument `i`.
    pub fn set_arg(&mut self, i: usize, v: u64) {
        if let Some(r) = ARG_REGS.get(i) {
            self.regs.insert(r.to_string(), v);
        }
    }

    pub fn reg(&self, name: &str) -> u64 {
        self.regs.get(name).copied().unwrap_or(0)
    }
    pub fn changed(&self, name: &str) -> bool {
        self.prev.get(name).map_or(false, |p| *p != self.reg(name))
    }

    pub fn read(&self, a: u64, sz: u8) -> u64 {
        let mut v = 0u64;
        for i in (0..sz as u64).rev() {
            v = (v << 8) | *self.mem.get(&a.wrapping_add(i)).unwrap_or(&0) as u64;
        }
        v
    }
    pub fn write(&mut self, a: u64, sz: u8, v: u64) {
        for i in 0..sz as u64 {
            self.mem.insert(a.wrapping_add(i), ((v >> (8 * i)) & 0xff) as u8);
        }
    }
    /// The next line the emulated program would read from stdin.
    fn read_line(&mut self) -> Option<String> {
        if self.stdin_pos >= self.stdin.len() {
            return None;
        }
        let rest = &self.stdin[self.stdin_pos..];
        let (line, adv) = match rest.find('\n') {
            Some(i) => (rest[..i].to_string(), i + 1),
            None => (rest.to_string(), rest.len()),
        };
        self.stdin_pos += adv;
        Some(line)
    }

    fn push(&mut self, v: u64) {
        let sp = self.reg("rsp").wrapping_sub(8);
        self.regs.insert("rsp".into(), sp);
        self.write(sp, 8, v);
    }
    fn pop(&mut self) -> u64 {
        let sp = self.reg("rsp");
        let v = self.read(sp, 8);
        self.regs.insert("rsp".into(), sp.wrapping_add(8));
        v
    }

    pub fn cstr(&self, a: u64) -> String {
        let mut s = String::new();
        for i in 0..4096u64 {
            match self.mem.get(&a.wrapping_add(i)) {
                Some(0) | None => break,
                Some(b) => s.push(*b as char),
            }
        }
        s
    }

    // ---------------------------------------------------------- evaluation --

    /// The width an expression is naturally computed at. Signed operations
    /// depend on it: `cmp dword [x], 0` against 0xfffffff9 is -7 < 0, but the
    /// same bits read as 64 bits are a large positive number and the branch
    /// goes the other way.
    fn width(e: &Expr) -> u8 {
        match e {
            Expr::Reg(r) => r.size,
            Expr::Mem(m) => m.size,
            Expr::Load { ty, .. } | Expr::Cast { ty, .. } => match ty {
                Type::Int { bits, .. } | Type::Float { bits } => (bits / 8) as u8,
                _ => 8,
            },
            Expr::Un { e, .. } => Self::width(e),
            Expr::Bin { l, r, .. } => Self::width(l).max(Self::width(r)),
            Expr::Ternary { t, f, .. } => Self::width(t).max(Self::width(f)),
            // a literal has no width of its own and must not claim one
            Expr::Const(_) => 0,
            _ => 8,
        }
    }

    fn sext(v: u64, sz: u8) -> i64 {
        match sz {
            1 => v as u8 as i8 as i64,
            2 => v as u16 as i16 as i64,
            4 => v as u32 as i32 as i64,
            _ => v as i64,
        }
    }
    fn trunc(v: u64, sz: u8) -> u64 {
        match sz {
            1 => v & 0xff,
            2 => v & 0xffff,
            4 => v & 0xffff_ffff,
            _ => v,
        }
    }

    fn mem_addr(&self, m: &MemOp) -> u64 {
        let mut a = m.base.as_ref().map_or(0, |b| self.reg(&b.full));
        if let Some(ix) = &m.index {
            a = a.wrapping_add(self.reg(&ix.full).wrapping_mul(m.scale as u64));
        }
        a.wrapping_add(m.disp as u64)
    }

    pub fn eval(&mut self, e: &Expr, prog: &Program) -> u64 {
        match e {
            Expr::Const(v) => *v as u64,
            Expr::Lit(s) => prog
                .funcs
                .iter()
                .find(|f| &f.name == s)
                .map(|f| f.addr)
                .unwrap_or(0),
            Expr::Reg(r) => {
                let v = self.reg(&r.full);
                if r.high8 {
                    (v >> 8) & 0xff
                } else {
                    Self::trunc(v, r.size)
                }
            }
            Expr::Mem(m) => {
                let a = self.mem_addr(m);
                let v = self.read(a, m.size);
                if m.signed {
                    Self::sext(v, m.size) as u64
                } else {
                    v
                }
            }
            Expr::Load { addr, ty } => {
                let a = self.eval(addr, prog);
                let sz = match ty {
                    Type::Int { bits, .. } => (bits / 8) as u8,
                    _ => 8,
                };
                self.read(a, sz)
            }
            Expr::AddrOf(inner) => match &**inner {
                Expr::Mem(m) => self.mem_addr(m),
                other => self.eval(other, prog),
            },
            Expr::Cast { ty, e } => {
                let v = self.eval(e, prog);
                match ty {
                    Type::Int { bits, signed: true } => Self::sext(v, (bits / 8) as u8) as u64,
                    Type::Int { bits, signed: false } => Self::trunc(v, (bits / 8) as u8),
                    _ => v,
                }
            }
            Expr::Un { op, e } => {
                let v = self.eval(e, prog);
                match op {
                    UnOp::Neg => (v as i64).wrapping_neg() as u64,
                    UnOp::Not => !v,
                    UnOp::LNot => (v == 0) as u64,
                }
            }
            Expr::Ternary { c, t, f } => {
                if self.eval(c, prog) != 0 {
                    self.eval(t, prog)
                } else {
                    self.eval(f, prog)
                }
            }
            Expr::Bin { op, l, r } => {
                let a = self.eval(l, prog);
                let b = self.eval(r, prog);
                let w = Self::width(l).max(Self::width(r)).max(1);
                let (sa, sb) = (Self::sext(a, w), Self::sext(b, w));
                match op {
                    BinOp::Add => a.wrapping_add(b),
                    BinOp::Sub => a.wrapping_sub(b),
                    BinOp::Mul => sa.wrapping_mul(sb) as u64,
                    BinOp::Div => {
                        if sb == 0 {
                            0
                        } else {
                            sa.wrapping_div(sb) as u64
                        }
                    }
                    BinOp::UDiv => {
                        if b == 0 {
                            0
                        } else {
                            a / b
                        }
                    }
                    BinOp::Rem => {
                        if sb == 0 {
                            0
                        } else {
                            sa.wrapping_rem(sb) as u64
                        }
                    }
                    BinOp::URem => {
                        if b == 0 {
                            0
                        } else {
                            a % b
                        }
                    }
                    BinOp::And => a & b,
                    BinOp::Or => a | b,
                    BinOp::Xor => a ^ b,
                    BinOp::Shl => a.wrapping_shl((b & 63) as u32),
                    BinOp::Shr => a.wrapping_shr((b & 63) as u32),
                    BinOp::Sar => (sa >> (b & 63)) as u64,
                    BinOp::Eq => (a == b) as u64,
                    BinOp::Ne => (a != b) as u64,
                    BinOp::Lt => (sa < sb) as u64,
                    BinOp::Le => (sa <= sb) as u64,
                    BinOp::Gt => (sa > sb) as u64,
                    BinOp::Ge => (sa >= sb) as u64,
                    BinOp::Ltu => (a < b) as u64,
                    BinOp::Leu => (a <= b) as u64,
                    BinOp::Gtu => (a > b) as u64,
                    BinOp::Geu => (a >= b) as u64,
                    BinOp::LAnd => (a != 0 && b != 0) as u64,
                    BinOp::LOr => (a != 0 || b != 0) as u64,
                }
            }
            Expr::Call { name, args, .. } => self.call(name, args, prog),
            _ => 0,
        }
    }

    fn call(&mut self, name: &str, args: &[Expr], prog: &Program) -> u64 {
        if let Some(i) = prog.funcs.iter().position(|f| f.name == name) {
            return self.call_local(i, prog);
        }
        // At this stage argument recovery has not run, so the arguments are
        // wherever the ABI put them.
        let vals: Vec<u64> = if args.is_empty() {
            ARG_REGS.iter().map(|r| self.reg(r)).collect()
        } else {
            args.iter().map(|a| self.eval(a, prog)).collect()
        };
        let a = |i: usize| vals.get(i).copied().unwrap_or(0);

        match name {
            "puts" => {
                let s = self.cstr(a(0));
                self.out.push_str(&s);
                self.out.push('\n');
                1
            }
            "printf" | "fprintf" | "sprintf" => {
                let fi = if name == "printf" { 0 } else { 1 };
                let s = self.format(&self.cstr(a(fi)), &vals[fi + 1..]);
                let n = s.len() as u64;
                self.out.push_str(&s);
                n
            }
            "putchar" => {
                self.out.push(a(0) as u8 as char);
                1
            }
            "malloc" | "calloc" => {
                let p = self.heap;
                self.heap += a(0).max(32) + 32;
                self.heap_top = self.heap;
                self.sync_heap();
                p
            }
            "gets" | "fgets" => {
                let Some(l) = self.read_line() else { return 0 };
                let dst = a(0);
                for (i, b) in l.bytes().enumerate() {
                    self.write(dst + i as u64, 1, b as u64);
                }
                self.write(dst + l.len() as u64, 1, 0);
                dst
            }
            "scanf" | "__isoc99_scanf" => {
                let Some(l) = self.read_line() else { return 0 };
                let fmt = self.cstr(a(0));
                let mut n = 0;
                let mut words = l.split_whitespace();
                for (i, spec) in fmt.match_indices('%').enumerate() {
                    let _ = spec;
                    let Some(w) = words.next() else { break };
                    let dst = a(i + 1);
                    if fmt.contains("%s") {
                        for (j, b) in w.bytes().enumerate() {
                            self.write(dst + j as u64, 1, b as u64);
                        }
                        self.write(dst + w.len() as u64, 1, 0);
                    } else {
                        let v: i64 = w.parse().unwrap_or(0);
                        self.write(dst, 4, v as u64);
                    }
                    n += 1;
                }
                n
            }
            "getchar" => match self.read_line() {
                Some(l) => l.bytes().next().unwrap_or(b'\n') as u64,
                None => u64::MAX,
            },
            "free" => 0,
            "strlen" => self.cstr(a(0)).len() as u64,
            "strcpy" | "strcat" => {
                let s = self.cstr(a(1));
                for (i, b) in s.bytes().enumerate() {
                    self.write(a(0) + i as u64, 1, b as u64);
                }
                self.write(a(0) + s.len() as u64, 1, 0);
                a(0)
            }
            "memcpy" | "memmove" => {
                for i in 0..a(2) {
                    let b = self.read(a(1) + i, 1);
                    self.write(a(0) + i, 1, b);
                }
                a(0)
            }
            "memset" => {
                for i in 0..a(2) {
                    self.write(a(0) + i, 1, a(1) & 0xff);
                }
                a(0)
            }
            // the stack canary: any stable value works so long as the check
            // at the end of the function reads back what the prologue stored
            n if n.starts_with("__read") => 0x00c0_ffee_5a5a_5a00,
            "__stack_chk_fail" => {
                self.stop("stack check failed");
                0
            }
            "exit" | "abort" | "_exit" => {
                self.stop("program called exit");
                0
            }
            other => {
                self.out.push_str(&format!("[unmodelled call {}]\n", other));
                0
            }
        }
    }

    fn format(&self, fmt: &str, args: &[u64]) -> String {
        let mut out = String::new();
        let mut it = fmt.chars().peekable();
        let mut n = 0usize;
        while let Some(c) = it.next() {
            if c != '%' {
                out.push(c);
                continue;
            }
            let mut long = false;
            let mut spec = ' ';
            while let Some(&d) = it.peek() {
                it.next();
                match d {
                    'l' | 'h' | 'z' => long = long || d == 'l' || d == 'z',
                    '0'..='9' | '.' | '-' | '+' | ' ' | '#' => {}
                    other => {
                        spec = other;
                        break;
                    }
                }
            }
            let v = args.get(n).copied().unwrap_or(0);
            match spec {
                '%' => out.push('%'),
                's' => {
                    out.push_str(&self.cstr(v));
                    n += 1;
                }
                'c' => {
                    out.push(v as u8 as char);
                    n += 1;
                }
                'd' | 'i' => {
                    out.push_str(&Self::sext(v, if long { 8 } else { 4 }).to_string());
                    n += 1;
                }
                'u' => {
                    out.push_str(&Self::trunc(v, if long { 8 } else { 4 }).to_string());
                    n += 1;
                }
                'x' => {
                    out.push_str(&format!("{:x}", Self::trunc(v, if long { 8 } else { 4 })));
                    n += 1;
                }
                'p' => {
                    out.push_str(&format!("{:#x}", v));
                    n += 1;
                }
                'f' | 'g' | 'e' => {
                    out.push_str(&format!("{}", f64::from_bits(v)));
                    n += 1;
                }
                _ => out.push(spec),
            }
        }
        out
    }

    fn call_local(&mut self, fi: usize, prog: &Program) -> u64 {
        if self.depth > 64 {
            return 0;
        }
        self.depth += 1;
        let saved = self.pc;
        const RET: u64 = 0xfeed_face_feed_face;
        self.push(RET);
        self.pc = Some(prog.funcs[fi].addr);
        for _ in 0..2_000_000 {
            if self.halted || self.pc == Some(RET) || self.pc.is_none() {
                break;
            }
            self.exec(prog);
        }
        self.pc = saved;
        self.depth -= 1;
        self.reg("rax")
    }

    fn assign(&mut self, dst: &Expr, val: u64, prog: &Program) {
        match dst {
            Expr::Reg(r) => {
                if r.high8 {
                    let cur = self.reg(&r.full);
                    self.regs.insert(r.full.clone(), (cur & !0xff00) | ((val & 0xff) << 8));
                } else if r.size >= 4 {
                    // a 32-bit write clears the upper half, as the machine does
                    self.regs.insert(r.full.clone(), Self::trunc(val, r.size));
                } else {
                    let cur = self.reg(&r.full);
                    let m = (1u64 << (r.size * 8)) - 1;
                    self.regs.insert(r.full.clone(), (cur & !m) | (val & m));
                }
            }
            Expr::Mem(m) => {
                let a = self.mem_addr(m);
                self.write(a, m.size, val);
            }
            Expr::Load { addr, ty } => {
                let a = self.eval(addr, prog);
                let sz = match ty {
                    Type::Int { bits, .. } => (bits / 8) as u8,
                    _ => 8,
                };
                self.write(a, sz, val);
            }
            _ => {}
        }
    }

    pub fn stop(&mut self, why: &str) {
        self.halted = true;
        self.reason = why.to_string();
        self.pc = None;
    }

    pub fn current<'p>(&self, prog: &'p Program) -> Option<&'p LiftedInsn> {
        let pc = self.pc?;
        let (fi, ii) = *self.index.get(&pc)?;
        prog.funcs.get(fi).and_then(|f| f.raw.get(ii))
    }

    /// One machine instruction.
    pub fn exec(&mut self, prog: &Program) {
        if self.halted {
            return;
        }
        let Some(pc) = self.pc else { return };
        let Some(ins) = self.current(prog).cloned() else {
            self.stop(&format!("pc outside the analysed code: {:#x}", pc));
            return;
        };
        self.prev = self.regs.clone();
        let mut next = pc + ins.len as u64;

        for st in &ins.stmts {
            match st {
                Stmt::Assign { dst, src } => {
                    let v = self.eval(src, prog);
                    self.assign(dst, v, prog);
                }
                Stmt::Do(e) => {
                    self.eval(e, prog);
                }
                Stmt::If { cond, target } => {
                    if self.eval(cond, prog) != 0 {
                        next = *target;
                    }
                }
                Stmt::Goto(t) => next = *t,
                Stmt::Return(_) => {
                    let ra = self.pop();
                    if ra == SENTINEL {
                        self.stop("returned from the entry function");
                        return;
                    }
                    self.pc = Some(ra);
                    self.steps += 1;
                    return;
                }
                Stmt::Asm(t) => {
                    if t.contains("hlt") || t.contains("ud2") {
                        self.stop("halted");
                        return;
                    }
                }
                Stmt::Nop => {}
            }
        }
        self.pc = Some(next);
        self.steps += 1;
    }

    /// Run until the program stops or a breakpoint is reached.
    pub fn run(&mut self, prog: &Program, budget: u64) {
        self.hit_breakpoint = None;
        for i in 0..budget {
            if self.halted || self.pc.is_none() {
                break;
            }
            // a breakpoint stops *before* the instruction executes, and not
            // on the very first step or resuming would be impossible
            if i > 0 {
                if let Some(pc) = self.pc {
                    if self.breakpoints.contains(&pc) {
                        self.hit_breakpoint = Some(pc);
                        return;
                    }
                }
            }
            let before = self.pc;
            self.exec(prog);
            if self.pc == before {
                break;
            }
        }
    }

    /// Execute one instruction, but if it is a call, run the callee to
    /// completion instead of stopping inside it.
    pub fn step_over(&mut self, prog: &Program) {
        let Some(pc) = self.pc else { return };
        let is_call = self.current(prog).map_or(false, |i| i.is_call);
        self.exec(prog);
        if !is_call {
            return;
        }
        // a lifted call is executed as an expression, so control has already
        // come back; nothing further to do unless it transferred
        let Some(after) = self.pc else { return };
        if after == pc {
            return;
        }
    }

    pub fn toggle_breakpoint(&mut self, a: u64) {
        if !self.breakpoints.remove(&a) {
            self.breakpoints.insert(a);
        }
    }

    pub fn started(&self) -> bool {
        self.steps > 0 || self.halted
    }
}

/// Permissions the loader would give a section, by name. Close enough to be
/// useful when reading a memory map, which is what it is for.
fn perm_of(name: &str) -> &'static str {
    const EXEC: [&str; 6] = [".text", ".init", ".fini", ".plt", ".plt.sec", "__text"];
    const RO: [&str; 7] =
        [".rodata", ".rdata", ".eh_frame", ".eh_frame_hdr", ".interp", "__cstring", "__const"];
    if EXEC.iter().any(|p| name.starts_with(p)) {
        "r-x"
    } else if RO.iter().any(|p| name.starts_with(p)) {
        "r--"
    } else {
        "rw-"
    }
}

fn short_name(path: &str) -> String {
    path.rsplit('/').next().unwrap_or(path).to_string()
}
