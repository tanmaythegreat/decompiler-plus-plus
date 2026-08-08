// ir.rs — tiny "P-code-like" IR. Not full SSA (kept simple for scope),
// but each op maps closely to Ghidra p-code style opcodes as described
// in the design doc (COPY, INT_ADD, INT_SUB, LOAD, STORE, CBRANCH, ...).

use std::fmt;

#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Reg(String),
    Imm(i64),
    // memory operand rendered as text, e.g. "[rbp-0x4]" or "[rax+rcx*4+0x10]"
    Mem(String),
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Reg(r) => write!(f, "{}", r),
            Value::Imm(i) => {
                if *i < 0 {
                    write!(f, "-0x{:x}", -i)
                } else {
                    write!(f, "0x{:x}", i)
                }
            }
            Value::Mem(m) => write!(f, "*{}", m),
        }
    }
}

#[derive(Clone, Debug)]
pub enum UnOp {
    Neg,
    Not,
}

impl fmt::Display for UnOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            UnOp::Neg => "-",
            UnOp::Not => "~",
        };
        write!(f, "{}", s)
    }
}

#[derive(Clone, Debug)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    And,
    Or,
    Xor,
    Shl,
    Shr,
}

impl fmt::Display for BinOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            BinOp::Add => "+",
            BinOp::Sub => "-",
            BinOp::Mul => "*",
            BinOp::And => "&",
            BinOp::Or => "|",
            BinOp::Xor => "^",
            BinOp::Shl => "<<",
            BinOp::Shr => ">>",
        };
        write!(f, "{}", s)
    }
}

#[derive(Clone, Debug)]
pub enum Cond {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    // unsigned variants
    Below,
    BelowEq,
    Above,
    AboveEq,
}

impl fmt::Display for Cond {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Cond::Eq => "==",
            Cond::Ne => "!=",
            Cond::Lt => "<",
            Cond::Le => "<=",
            Cond::Gt => ">",
            Cond::Ge => ">=",
            Cond::Below => "<u",
            Cond::BelowEq => "<=u",
            Cond::Above => ">u",
            Cond::AboveEq => ">=u",
        };
        write!(f, "{}", s)
    }
}

/// One IR "instruction" — deliberately low level, mirrors a small subset
/// of Ghidra p-code opcodes (COPY, INT_ADD/SUB/..., LOAD, STORE, CMP,
/// CBRANCH, BRANCH, CALL, RETURN).
#[derive(Clone, Debug)]
pub enum Instr {
    Copy { dst: Value, src: Value },
    Un { dst: Value, op: UnOp, src: Value },
    Bin { dst: Value, op: BinOp, lhs: Value, rhs: Value },
    Cmp { lhs: Value, rhs: Value, cond: Cond },
    CBranch { target: u64 }, // uses the last Cmp's cond, jumps if true
    Branch { target: u64 },
    Call { target: String, args: Vec<Value> },
    Ret { val: Option<Value> },
    Nop,
    Unknown { text: String },
}

impl fmt::Display for Instr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Instr::Copy { dst, src } => write!(f, "{} = {}", dst, src),
            Instr::Un { dst, op, src } => write!(f, "{} = {}{}", dst, op, src),
            Instr::Bin { dst, op, lhs, rhs } => write!(f, "{} = {} {} {}", dst, lhs, op, rhs),
            Instr::Cmp { lhs, rhs, cond } => write!(f, "test({} {} {})", lhs, cond, rhs),
            Instr::CBranch { target } => write!(f, "if (cond) goto L{:x}", target),
            Instr::Branch { target } => write!(f, "goto L{:x}", target),
            Instr::Call { target, args } => {
                let a: Vec<String> = args.iter().map(|v| v.to_string()).collect();
                write!(f, "{}({})", target, a.join(", "))
            }
            Instr::Ret { val } => match val {
                Some(v) => write!(f, "return {}", v),
                None => write!(f, "return"),
            },
            Instr::Nop => write!(f, ""),
            Instr::Unknown { text } => write!(f, "/* unhandled: {} */", text),
        }
    }
}

/// A lifted machine instruction: original address + resulting IR ops.
pub struct LiftedInsn {
    pub addr: u64,
    pub len: u32,
    pub asm_text: String,
    pub ops: Vec<Instr>,
    /// true if this insn ends a basic block (branch/call/ret)
    pub is_block_end: bool,
    /// branch targets (0, 1, or 2 for conditional)
    pub targets: Vec<u64>,
    pub falls_through: bool,
}
