//! The two central contracts of the whole architecture: [`Disassembler`] and
//! [`Lifter`]. An `arch-x86` (or future `arch-arm`, `arch-riscv`, ...) crate
//! implements both against its own architecture-specific decoded-instruction
//! type; nothing in `decompiler-core` or any later analysis pass needs to
//! know anything about x86 opcodes, ModRM bytes, or register encodings.
//!
//! This is the compile-time guarantee the roadmap calls for: forgetting to
//! implement a required method is a build error here, not a runtime
//! surprise three passes downstream.

use ir_pcode::{Instruction, TempAllocator};
use std::fmt;

/// The result of decoding a single machine instruction starting at a given
/// address: the decoded instruction itself plus how many bytes it consumed
/// (needed to know where the *next* instruction starts).
#[derive(Debug, Clone)]
pub struct DisassemblyResult<I> {
    pub instruction: I,
    /// Number of bytes consumed from the input slice.
    pub length: usize,
    /// The address this instruction was decoded at.
    pub address: u64,
}

/// Errors a [`Disassembler`] can report for a single decode attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecodeError {
    /// Fewer bytes were available than the (partially decoded) instruction
    /// requires.
    UnexpectedEnd,
    /// The byte sequence doesn't correspond to any recognized instruction
    /// (or, in this early phase, to one this decoder has implemented yet).
    InvalidOrUnsupported { opcode_byte: u8 },
    /// The opcode was recognized but its addressing mode (e.g. a memory
    /// operand via ModRM) isn't handled yet.
    UnsupportedAddressingMode { modrm: u8 },
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DecodeError::UnexpectedEnd => write!(f, "unexpected end of input while decoding"),
            DecodeError::InvalidOrUnsupported { opcode_byte } => {
                write!(f, "invalid or unsupported opcode byte 0x{opcode_byte:02x}")
            }
            DecodeError::UnsupportedAddressingMode { modrm } => {
                write!(f, "unsupported ModRM addressing mode (modrm=0x{modrm:02x})")
            }
        }
    }
}

/// Turns raw bytes into architecture-specific decoded instructions.
///
/// Implemented once per architecture (e.g. `X86Disassembler` in `arch-x86`).
/// `Self::Instruction` is deliberately an associated type rather than a
/// single shared struct: x86, ARM, and RISC-V instructions carry wildly
/// different operand shapes, and forcing them into one universal struct
/// would either lose information or bloat every architecture with fields it
/// never uses. The *interface* is shared; the representation is not.
pub trait Disassembler {
    /// The architecture-specific decoded instruction type this disassembler
    /// produces.
    type Instruction: fmt::Debug;

    /// Decode a single instruction from `bytes`, which begins at `address`.
    fn disassemble_one(
        &self,
        bytes: &[u8],
        address: u64,
    ) -> Result<DisassemblyResult<Self::Instruction>, DecodeError>;
}

/// Translates one architecture-specific decoded instruction into zero or
/// more IR [`Instruction`]s ("lifting").
///
/// A single machine instruction commonly expands into several IR ops (e.g.
/// an x86 `CMP` becomes an `INT_SUB` into a throwaway temporary plus several
/// flag-bit computations) -- hence `Vec<Instruction>` rather than a 1:1
/// mapping.
pub trait Lifter {
    /// Must match the `Instruction` type of the paired [`Disassembler`].
    type Instruction: fmt::Debug;

    /// Lift one decoded instruction, located at `address` and occupying
    /// `length` bytes, into IR. `length` is required (not re-derived from
    /// the instruction itself) because relative branch/call targets are
    /// defined relative to the *next* instruction's address, and re-deriving
    /// byte length purely from a decoded instruction's shape is ambiguous
    /// whenever two different encodings decode to the same variant (e.g.
    /// x86's short-form `CMP EAX, imm32` vs. the general `CMP r/m32, imm32`).
    /// `temps` is a shared allocator for fresh `Unique`-space Varnodes so
    /// that scratch values never collide across instructions within a
    /// function.
    fn lift(
        &self,
        instruction: &Self::Instruction,
        address: u64,
        length: usize,
        temps: &mut TempAllocator,
    ) -> Vec<Instruction>;
}
