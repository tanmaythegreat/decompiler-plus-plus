//! `decompiler-core`: the central hub crate.
//!
//! This crate defines the contracts ([`Disassembler`], [`Lifter`]) that
//! every architecture backend implements, plus the shared types
//! ([`Function`], [`BasicBlock`], [`Variable`]) that flow through the rest
//! of the pipeline. It deliberately contains **no** architecture-specific
//! logic -- that all lives in `arch-x86` and its future siblings -- which is
//! what lets a new ISA be added as "a new implementation of these traits"
//! rather than a change to this crate.

mod traits;
mod types;

pub use traits::{DecodeError, Disassembler, DisassemblyResult, Lifter};
pub use types::{BasicBlock, Function, LiftedInstruction, Variable};

// Re-export the IR crate's types so downstream crates can pull everything
// they need (`decompiler_core::Instruction`, `decompiler_core::Varnode`,
// ...) from one place without also depending on `ir-pcode` directly.
pub use ir_pcode::{AddressSpace, BranchTarget, Instruction, TempAllocator, Varnode};
