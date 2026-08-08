// ir.rs — expression-tree IR.
//
// The old IR modelled every operand as `Value::{Reg(String), Imm(i64),
// Mem(String)}`. A memory operand was a *rendered string* like
// "[rbp-0x4]", which meant nothing downstream could ever reason about
// it: no base/index/scale/displacement, no access size, so no stack
// variables, no arrays, no structs, and no type inference. It also had
// no way to nest expressions, so every machine instruction had to
// become its own statement and the output read like assembly with `=`
// signs.
//
// This version is a real expression tree with types, which is what
// makes `v3 = s->count + arr[i]` expressible at all.

use std::fmt;

// ---------------------------------------------------------------- types

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Type {
    Void,
    /// bits = 8/16/32/64
    Int { bits: u16, signed: bool },
    Float { bits: u16 },
    Ptr(Box<Type>),
    Array(Box<Type>, usize),
    /// index into `StructTable`
    Struct(usize),
    /// size known, semantics not
    Unknown(u8),
}

impl Type {
    pub fn int(bits: u16) -> Type {
        Type::Int { bits, signed: true }
    }
    pub fn ptr(t: Type) -> Type {
        Type::Ptr(Box::new(t))
    }
    pub fn void_ptr() -> Type {
        Type::ptr(Type::Void)
    }
    pub fn char_ptr() -> Type {
        Type::ptr(Type::Int { bits: 8, signed: true })
    }

    /// Size in bytes (0 when unknown/void).
    pub fn size(&self, st: &StructTable) -> usize {
        match self {
            Type::Void => 0,
            Type::Int { bits, .. } | Type::Float { bits } => (*bits / 8) as usize,
            Type::Ptr(_) => 8,
            Type::Array(e, n) => e.size(st) * n,
            Type::Struct(i) => st.get(*i).map(|s| s.size).unwrap_or(0),
            Type::Unknown(n) => *n as usize,
        }
    }

    #[allow(dead_code)]
    pub fn is_ptr(&self) -> bool {
        matches!(self, Type::Ptr(_))
    }

    /// Build a scalar integer type from an access width in bytes.
    pub fn from_width(bytes: u8, signed: bool) -> Type {
        match bytes {
            1 => Type::Int { bits: 8, signed },
            2 => Type::Int { bits: 16, signed },
            4 => Type::Int { bits: 32, signed },
            8 => Type::Int { bits: 64, signed },
            n => Type::Unknown(n),
        }
    }

    /// The bare type name, without the declarator part.
    pub fn base_name(&self, st: &StructTable) -> String {
        match self {
            Type::Void => "void".into(),
            Type::Int { bits: 8, signed: true } => "char".into(),
            Type::Int { bits: 8, signed: false } => "unsigned char".into(),
            Type::Int { bits: 16, signed: true } => "short".into(),
            Type::Int { bits: 16, signed: false } => "unsigned short".into(),
            Type::Int { bits: 32, signed: true } => "int".into(),
            Type::Int { bits: 32, signed: false } => "unsigned int".into(),
            Type::Int { bits: 64, signed: true } => "long".into(),
            Type::Int { bits: 64, signed: false } => "unsigned long".into(),
            Type::Int { bits, signed } => {
                format!("{}int{}_t", if *signed { "" } else { "u" }, bits)
            }
            Type::Float { bits: 32 } => "float".into(),
            Type::Float { .. } => "double".into(),
            Type::Ptr(inner) => inner.base_name(st),
            Type::Array(e, _) => e.base_name(st),
            Type::Struct(i) => st
                .get(*i)
                .map(|s| format!("struct {}", s.name))
                .unwrap_or_else(|| "struct unknown".into()),
            Type::Unknown(n) => match n {
                1 => "char".into(),
                2 => "short".into(),
                8 => "long".into(),
                _ => "int".into(),
            },
        }
    }

    /// Full C declaration for `name` with this type — handles the
    /// inside-out declarator syntax C needs (`int *p`, `int a[10]`,
    /// `char *v[4]`). A naive `format!("{} {}", ty, name)` gets arrays
    /// and pointer-to-array wrong, so this is built recursively.
    pub fn declare(&self, name: &str, st: &StructTable) -> String {
        let mut decl = name.to_string();
        let mut cur = self;
        loop {
            match cur {
                Type::Ptr(inner) => {
                    decl = if matches!(**inner, Type::Array(..)) {
                        format!("(*{})", decl)
                    } else {
                        format!("*{}", decl)
                    };
                    cur = inner;
                }
                Type::Array(elem, n) => {
                    decl = format!("{}[{}]", decl, n);
                    cur = elem;
                }
                _ => break,
            }
        }
        let base = cur.base_name(st);
        if decl.is_empty() {
            base
        } else if decl.starts_with('*') || decl.starts_with('(') {
            format!("{} {}", base, decl)
        } else {
            format!("{} {}", base, decl)
        }
    }

    /// Type name usable in a cast expression, e.g. `(unsigned int)`.
    pub fn cast_name(&self, st: &StructTable) -> String {
        self.declare("", st).trim().to_string()
    }
}

// -------------------------------------------------------------- structs

#[derive(Clone, Debug)]
pub struct Field {
    pub off: i64,
    pub ty: Type,
    pub name: String,
}

#[derive(Clone, Debug)]
pub struct StructDef {
    pub name: String,
    pub fields: Vec<Field>,
    pub size: usize,
}

#[derive(Clone, Debug, Default)]
pub struct StructTable {
    pub defs: Vec<StructDef>,
}

impl StructTable {
    pub fn get(&self, i: usize) -> Option<&StructDef> {
        self.defs.get(i)
    }
    pub fn add(&mut self, d: StructDef) -> usize {
        self.defs.push(d);
        self.defs.len() - 1
    }
    pub fn field_at(&self, sid: usize, off: i64) -> Option<usize> {
        let s = self.get(sid)?;
        s.fields.iter().position(|f| f.off == off)
    }
    pub fn render(&self) -> String {
        let mut out = String::new();
        for d in &self.defs {
            out.push_str(&format!("struct {} {{\n", d.name));
            for f in &d.fields {
                out.push_str(&format!(
                    "    {};{}/* +0x{:x} */\n",
                    f.ty.declare(&f.name, self),
                    " ".repeat(24usize.saturating_sub(f.ty.declare(&f.name, self).len())),
                    f.off
                ));
            }
            out.push_str("};\n\n");
        }
        out
    }
}

// ---------------------------------------------------------------- values

/// A register access, canonicalised to its 64-bit family plus the width
/// actually touched. The old code kept only the printed sub-register
/// name ("eax"), so `eax` and `rax` looked like two unrelated variables
/// to every later pass.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct RegRef {
    /// 64-bit family name, e.g. "rax"
    pub full: String,
    /// bytes touched by this access: 1, 2, 4, 8, or 16 for xmm
    pub size: u8,
    /// true for ah/bh/ch/dh (bits 8..16)
    pub high8: bool,
}

impl RegRef {
    pub fn new(full: &str, size: u8) -> RegRef {
        RegRef { full: full.to_string(), size, high8: false }
    }
    pub fn is_xmm(&self) -> bool {
        self.full.starts_with("xmm")
    }
}

/// A decoded x86 memory operand, kept structurally instead of as text.
/// Everything downstream — stack-variable recovery, array detection,
/// struct field detection — reads these fields.
#[derive(Clone, Debug, PartialEq)]
pub struct MemOp {
    pub base: Option<RegRef>,
    pub index: Option<RegRef>,
    pub scale: u8,
    pub disp: i64,
    /// access width in bytes
    pub size: u8,
    pub signed: bool,
    /// resolved absolute address for RIP-relative operands
    pub rip_abs: Option<u64>,
}

pub type VarId = usize;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    UDiv,
    Rem,
    URem,
    And,
    Or,
    Xor,
    Shl,
    Shr,
    Sar,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    Ltu,
    Leu,
    Gtu,
    Geu,
    LAnd,
    LOr,
}

impl BinOp {
    pub fn sym(self) -> &'static str {
        use BinOp::*;
        match self {
            Add => "+",
            Sub => "-",
            Mul => "*",
            Div | UDiv => "/",
            Rem | URem => "%",
            And => "&",
            Or => "|",
            Xor => "^",
            Shl => "<<",
            Shr | Sar => ">>",
            Eq => "==",
            Ne => "!=",
            Lt | Ltu => "<",
            Le | Leu => "<=",
            Gt | Gtu => ">",
            Ge | Geu => ">=",
            LAnd => "&&",
            LOr => "||",
        }
    }
    pub fn is_cmp(self) -> bool {
        use BinOp::*;
        matches!(self, Eq | Ne | Lt | Le | Gt | Ge | Ltu | Leu | Gtu | Geu)
    }
    pub fn is_unsigned_cmp(self) -> bool {
        use BinOp::*;
        matches!(self, Ltu | Leu | Gtu | Geu)
    }
    /// C precedence, higher binds tighter. Used to drop redundant parens.
    pub fn prec(self) -> u8 {
        use BinOp::*;
        match self {
            Mul | Div | UDiv | Rem | URem => 10,
            Add | Sub => 9,
            Shl | Shr | Sar => 8,
            Lt | Le | Gt | Ge | Ltu | Leu | Gtu | Geu => 7,
            Eq | Ne => 6,
            And => 5,
            Xor => 4,
            Or => 3,
            LAnd => 2,
            LOr => 1,
        }
    }
    pub fn negate(self) -> Option<BinOp> {
        use BinOp::*;
        Some(match self {
            Eq => Ne,
            Ne => Eq,
            Lt => Ge,
            Ge => Lt,
            Le => Gt,
            Gt => Le,
            Ltu => Geu,
            Geu => Ltu,
            Leu => Gtu,
            Gtu => Leu,
            _ => return None,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnOp {
    Neg,
    Not,
    LNot,
}

impl UnOp {
    pub fn sym(self) -> &'static str {
        match self {
            UnOp::Neg => "-",
            UnOp::Not => "~",
            UnOp::LNot => "!",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Expr {
    Const(i64),
    /// something already rendered: string literal, symbol name
    Lit(String),
    Reg(RegRef),
    Var(VarId),
    /// unresolved memory operand; the frame pass rewrites these
    Mem(MemOp),
    /// *(T *)addr
    Load { addr: Box<Expr>, ty: Type },
    AddrOf(Box<Expr>),
    Bin { op: BinOp, l: Box<Expr>, r: Box<Expr> },
    Un { op: UnOp, e: Box<Expr> },
    Cast { ty: Type, e: Box<Expr> },
    Arrow { base: Box<Expr>, sid: usize, fid: usize },
    /// `s.field` — reserved for by-value aggregates in frame slots
    #[allow(dead_code)]
    Dot { base: Box<Expr>, sid: usize, fid: usize },
    Index { base: Box<Expr>, idx: Box<Expr> },
    Call { name: String, args: Vec<Expr>, indirect: Option<Box<Expr>> },
    Ternary { c: Box<Expr>, t: Box<Expr>, f: Box<Expr> },
    Unknown(String),
}

impl Expr {
    pub fn bin(op: BinOp, l: Expr, r: Expr) -> Expr {
        Expr::Bin { op, l: Box::new(l), r: Box::new(r) }
    }
    pub fn un(op: UnOp, e: Expr) -> Expr {
        Expr::Un { op, e: Box::new(e) }
    }
    pub fn cast(ty: Type, e: Expr) -> Expr {
        Expr::Cast { ty, e: Box::new(e) }
    }
    pub fn as_const(&self) -> Option<i64> {
        match self {
            Expr::Const(c) => Some(*c),
            _ => None,
        }
    }

    /// Logical negation, pushed into the operator where possible so the
    /// printer emits `a != b` rather than `!(a == b)`.
    pub fn negated(self) -> Expr {
        match self {
            Expr::Bin { op, l, r } => match op.negate() {
                Some(n) => Expr::Bin { op: n, l, r },
                None => match op {
                    BinOp::LAnd => Expr::Bin {
                        op: BinOp::LOr,
                        l: Box::new(l.negated()),
                        r: Box::new(r.negated()),
                    },
                    BinOp::LOr => Expr::Bin {
                        op: BinOp::LAnd,
                        l: Box::new(l.negated()),
                        r: Box::new(r.negated()),
                    },
                    _ => Expr::un(UnOp::LNot, Expr::Bin { op, l, r }),
                },
            },
            Expr::Un { op: UnOp::LNot, e } => *e,
            Expr::Const(c) => Expr::Const((c == 0) as i64),
            other => Expr::un(UnOp::LNot, other),
        }
    }

    /// Rewrite every sub-expression bottom-up.
    #[allow(dead_code)]
    pub fn map<F: FnMut(Expr) -> Expr + Copy>(self, f: F) -> Expr {
        let mut f = f;
        let inner = match self {
            Expr::Load { addr, ty } => Expr::Load { addr: Box::new(addr.map(f)), ty },
            Expr::AddrOf(e) => Expr::AddrOf(Box::new(e.map(f))),
            Expr::Bin { op, l, r } => Expr::Bin { op, l: Box::new(l.map(f)), r: Box::new(r.map(f)) },
            Expr::Un { op, e } => Expr::Un { op, e: Box::new(e.map(f)) },
            Expr::Cast { ty, e } => Expr::Cast { ty, e: Box::new(e.map(f)) },
            Expr::Arrow { base, sid, fid } => Expr::Arrow { base: Box::new(base.map(f)), sid, fid },
            Expr::Dot { base, sid, fid } => Expr::Dot { base: Box::new(base.map(f)), sid, fid },
            Expr::Index { base, idx } => {
                Expr::Index { base: Box::new(base.map(f)), idx: Box::new(idx.map(f)) }
            }
            Expr::Call { name, args, indirect } => Expr::Call {
                name,
                args: args.into_iter().map(|a| a.map(f)).collect(),
                indirect: indirect.map(|i| Box::new(i.map(f))),
            },
            Expr::Ternary { c, t, f: fe } => Expr::Ternary {
                c: Box::new(c.map(f)),
                t: Box::new(t.map(f)),
                f: Box::new(fe.map(f)),
            },
            leaf => leaf,
        };
        f(inner)
    }

    pub fn walk<F: FnMut(&Expr)>(&self, f: &mut F) {
        f(self);
        match self {
            Expr::Load { addr, .. } => addr.walk(f),
            Expr::AddrOf(e) | Expr::Un { e, .. } | Expr::Cast { e, .. } => e.walk(f),
            Expr::Bin { l, r, .. } => {
                l.walk(f);
                r.walk(f);
            }
            Expr::Arrow { base, .. } | Expr::Dot { base, .. } => base.walk(f),
            Expr::Index { base, idx } => {
                base.walk(f);
                idx.walk(f);
            }
            Expr::Call { args, indirect, .. } => {
                for a in args {
                    a.walk(f);
                }
                if let Some(i) = indirect {
                    i.walk(f);
                }
            }
            Expr::Ternary { c, t, f: fe } => {
                c.walk(f);
                t.walk(f);
                fe.walk(f);
            }
            _ => {}
        }
    }

    pub fn has_call(&self) -> bool {
        let mut found = false;
        self.walk(&mut |e| {
            if matches!(e, Expr::Call { .. }) {
                found = true;
            }
        });
        found
    }

    pub fn reads_mem(&self) -> bool {
        let mut found = false;
        self.walk(&mut |e| {
            if matches!(
                e,
                Expr::Load { .. } | Expr::Mem(_) | Expr::Arrow { .. } | Expr::Index { .. }
            ) {
                found = true;
            }
        });
        found
    }

    /// Rough node count — used to stop copy propagation from building
    /// unreadably deep one-liners.
    #[allow(dead_code)]
    pub fn size_of(&self) -> usize {
        let mut n = 0;
        self.walk(&mut |_| n += 1);
        n
    }
}

// ------------------------------------------------------------ conditions

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CondCode {
    E,
    Ne,
    L,
    Le,
    G,
    Ge,
    B,
    Be,
    A,
    Ae,
    S,
    Ns,
    P,
    Np,
    O,
    No,
}

/// What last wrote the flags, so a later `jcc`/`setcc`/`cmovcc` can be
/// turned into a real boolean expression.
///
/// The old code stored the condition as `Instr::Unknown { text:
/// "__cond__Eq" }` — a string smuggled through the instruction stream —
/// and always paired it with a `Cmp`. That silently mistranslated
/// `test eax, eax; je` into `eax == eax` (always true) instead of
/// `eax == 0`, which is why the old `main` output took the wrong branch.
#[derive(Clone, Debug)]
pub enum FlagSrc {
    /// from `cmp a, b` — flags describe a - b
    Cmp(Expr, Expr),
    /// from `test a, b` — flags describe a & b
    Test(Expr, Expr),
    /// from any arithmetic/logic op — flags describe its result
    Logic(Expr),
}

impl FlagSrc {
    /// "flags were set like this" + "branch on this condition" → the
    /// boolean expression a C programmer would have written.
    pub fn to_expr(&self, cc: CondCode) -> Expr {
        use CondCode::*;
        let zero = Expr::Const(0);
        match self {
            FlagSrc::Cmp(a, b) => {
                let op = match cc {
                    E => BinOp::Eq,
                    Ne => BinOp::Ne,
                    L => BinOp::Lt,
                    Le => BinOp::Le,
                    G => BinOp::Gt,
                    Ge => BinOp::Ge,
                    B => BinOp::Ltu,
                    Be => BinOp::Leu,
                    A => BinOp::Gtu,
                    Ae => BinOp::Geu,
                    // SF after `cmp a,b` is the sign of a-b, which equals
                    // a<b whenever the subtraction doesn't overflow —
                    // the case compilers emit js/jns for.
                    S => BinOp::Lt,
                    Ns => BinOp::Ge,
                    P | Np | O | No => return Expr::Unknown(format!("flag_{:?}", cc)),
                };
                Expr::bin(op, a.clone(), b.clone())
            }
            FlagSrc::Test(a, b) => {
                let val =
                    if a == b { a.clone() } else { Expr::bin(BinOp::And, a.clone(), b.clone()) };
                let op = match cc {
                    E => BinOp::Eq,
                    Ne => BinOp::Ne,
                    S | L => BinOp::Lt,
                    Ns | Ge => BinOp::Ge,
                    G => BinOp::Gt,
                    Le => BinOp::Le,
                    // AND clears CF: `jb` never taken, `jae` always taken.
                    B => return Expr::Const(0),
                    Ae => return Expr::Const(1),
                    Be => BinOp::Eq,
                    A => BinOp::Ne,
                    P | Np | O | No => return Expr::Unknown(format!("flag_{:?}", cc)),
                };
                Expr::bin(op, val, zero)
            }
            FlagSrc::Logic(e) => {
                let op = match cc {
                    E => BinOp::Eq,
                    Ne => BinOp::Ne,
                    S | L => BinOp::Lt,
                    Ns | Ge => BinOp::Ge,
                    G => BinOp::Gt,
                    Le => BinOp::Le,
                    B => return Expr::Const(0),
                    Ae => return Expr::Const(1),
                    Be => BinOp::Eq,
                    A => BinOp::Ne,
                    P | Np | O | No => return Expr::Unknown(format!("flag_{:?}", cc)),
                };
                Expr::bin(op, e.clone(), zero)
            }
        }
    }
}

// ------------------------------------------------------------ statements

#[derive(Clone, Debug)]
pub enum Stmt {
    Assign { dst: Expr, src: Expr },
    /// an expression evaluated for effect (a call whose result is unused)
    Do(Expr),
    Return(Option<Expr>),
    If { cond: Expr, target: u64 },
    Goto(u64),
    Nop,
    /// instruction we don't lift — preserved verbatim so nothing is
    /// silently dropped from the output
    Asm(String),
}

/// One lifted machine instruction.
#[derive(Clone)]
pub struct LiftedInsn {
    pub addr: u64,
    pub len: u32,
    pub asm_text: String,
    pub stmts: Vec<Stmt>,
    /// flags written by this instruction, if any
    pub flags: Option<FlagSrc>,
    /// condition this instruction *reads* (jcc/setcc/cmovcc)
    pub cond: Option<CondCode>,
    pub is_block_end: bool,
    pub targets: Vec<u64>,
    pub falls_through: bool,
    pub is_call: bool,
    /// set for the push/mov-rbp/sub-rsp prologue and leave/pop epilogue,
    /// so the emitter can drop frame setup instead of printing it
    pub is_frame_setup: bool,
}

impl fmt::Display for Stmt {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Stmt::Assign { .. } => write!(f, "<assign>"),
            Stmt::Do(_) => write!(f, "<expr>"),
            Stmt::Return(_) => write!(f, "return"),
            Stmt::If { target, .. } => write!(f, "if -> {:x}", target),
            Stmt::Goto(t) => write!(f, "goto {:x}", t),
            Stmt::Nop => write!(f, ""),
            Stmt::Asm(s) => write!(f, "asm({})", s),
        }
    }
}
