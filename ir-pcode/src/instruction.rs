//! The IR's `Instruction` type: a tagged enum of low-level, architecture-
//! independent operations, deliberately modeled after Ghidra's p-code
//! opcodes. Every machine instruction is *lifted* into a `Vec<Instruction>`;
//! a single complex x86 instruction (e.g. one that touches several flag
//! bits) may expand into several of these.

use crate::varnode::Varnode;
use std::fmt;

/// Where control transfers to. Lifters resolve relative displacements
/// (rel8/rel32 etc.) to an absolute address at lift time, so the rest of the
/// pipeline never has to re-derive it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BranchTarget {
    /// A statically known absolute address.
    Absolute(u64),
    /// An indirect branch through a Varnode (e.g. `jmp eax`, `call [rax+8]`).
    /// Left unresolved for later analysis passes (e.g. jump-table recovery).
    Indirect,
}

impl fmt::Display for BranchTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BranchTarget::Absolute(addr) => write!(f, "0x{addr:x}"),
            BranchTarget::Indirect => write!(f, "<indirect>"),
        }
    }
}

/// A single P-code-style IR operation.
///
/// This is intentionally a small subset covering the roadmap's Phase 1
/// instruction set (MOV, ADD, CMP, JMP, CALL, plus RET and conditional
/// branches so a lifted CMP/Jcc pair is actually usable downstream). Later
/// phases extend this enum with more opcodes (bitwise ops, shifts, SSA
/// MULTIEQUAL/phi nodes, etc.) without needing to touch `decompiler-core`.
#[derive(Debug, Clone)]
pub enum Instruction {
    /// `dest = src` — a plain copy/move between two Varnodes of equal size.
    Copy { dest: Varnode, src: Varnode },

    /// `dest = *addr` — read `dest.size` bytes from memory at `addr`.
    Load { dest: Varnode, addr: Varnode },

    /// `*addr = src` — write `src` to memory at `addr`.
    Store { addr: Varnode, src: Varnode },

    /// `dest = lhs + rhs`
    IntAdd { dest: Varnode, lhs: Varnode, rhs: Varnode },

    /// `dest = lhs - rhs`
    IntSub { dest: Varnode, lhs: Varnode, rhs: Varnode },

    /// `dest = (lhs == rhs)` — produces a 1-byte boolean Varnode.
    IntEqual { dest: Varnode, lhs: Varnode, rhs: Varnode },

    /// `dest = (lhs != rhs)`
    IntNotEqual { dest: Varnode, lhs: Varnode, rhs: Varnode },

    /// `dest = (lhs <s rhs)` — signed less-than.
    IntSless { dest: Varnode, lhs: Varnode, rhs: Varnode },

    /// `dest = (lhs <=s rhs)` — signed less-than-or-equal.
    IntSlessEqual { dest: Varnode, lhs: Varnode, rhs: Varnode },

    /// Unconditional control transfer.
    Branch { target: BranchTarget },

    /// Branch to `target` iff `condition` (a 1-byte boolean Varnode) is
    /// non-zero; otherwise falls through to the next instruction.
    CBranch { condition: Varnode, target: BranchTarget },

    /// Call a subroutine. Return-value/argument plumbing is handled by
    /// later analysis passes once calling-convention info is available.
    Call { target: BranchTarget },

    /// Return from the current subroutine.
    Return,

    /// A no-op, used as a placeholder for machine instructions not yet
    /// modeled (e.g. prefixes/padding) so lifting never silently drops
    /// bytes.
    Nop,

    /// SSA φ-node: `dest = φ(srcs[0], srcs[1], ...)` where `srcs[i]` is
    /// the value that reaches `dest` from the i-th predecessor block.
    ///
    /// φ-nodes are only present after an SSA construction pass rewrites
    /// the IR. Lifted code never contains them — it uses plain `Copy`.
    /// Every analysis pass that reads definitions (type inference, liveness,
    /// etc.) must handle this variant; analyses that only care about
    /// program-point reachability may safely ignore it.
    Phi { dest: Varnode, srcs: Vec<Varnode> },
}

impl fmt::Display for Instruction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Instruction::Copy { dest, src } => write!(f, "{dest} = COPY {src}"),
            Instruction::Load { dest, addr } => write!(f, "{dest} = LOAD [{addr}]"),
            Instruction::Store { addr, src } => write!(f, "[{addr}] = STORE {src}"),
            Instruction::IntAdd { dest, lhs, rhs } => write!(f, "{dest} = INT_ADD {lhs}, {rhs}"),
            Instruction::IntSub { dest, lhs, rhs } => write!(f, "{dest} = INT_SUB {lhs}, {rhs}"),
            Instruction::IntEqual { dest, lhs, rhs } => {
                write!(f, "{dest} = INT_EQUAL {lhs}, {rhs}")
            }
            Instruction::IntNotEqual { dest, lhs, rhs } => {
                write!(f, "{dest} = INT_NOTEQUAL {lhs}, {rhs}")
            }
            Instruction::IntSless { dest, lhs, rhs } => {
                write!(f, "{dest} = INT_SLESS {lhs}, {rhs}")
            }
            Instruction::IntSlessEqual { dest, lhs, rhs } => {
                write!(f, "{dest} = INT_SLESSEQUAL {lhs}, {rhs}")
            }
            Instruction::Branch { target } => write!(f, "BRANCH {target}"),
            Instruction::CBranch { condition, target } => {
                write!(f, "CBRANCH {target}, {condition}")
            }
            Instruction::Call { target } => write!(f, "CALL {target}"),
            Instruction::Return => write!(f, "RETURN"),
            Instruction::Nop => write!(f, "NOP"),
            Instruction::Phi { dest, srcs } => {
                let src_list: Vec<String> = srcs.iter().map(|s| format!("{s}")).collect();
                write!(f, "{dest} = PHI({})", src_list.join(", "))
            }
        }
    }
}
