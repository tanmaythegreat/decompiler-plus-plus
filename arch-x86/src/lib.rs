//! `arch-x86`: the x86 architecture backend.
//!
//! Implements `decompiler_core::Disassembler` (see [`decoder::X86Disassembler`])
//! and `decompiler_core::Lifter` (see [`lifter::X86Lifter`]) for a
//! representative subset of x86 instructions. No other crate in the
//! workspace needs to change to add this backend, and adding a future
//! `arch-arm` or `arch-riscv` crate follows exactly the same pattern:
//! implement the same two traits against a new architecture-specific
//! decoded-instruction type.

mod decoder;
mod lifter;
mod registers;

pub use decoder::{X86Disassembler, X86Instruction};
pub use lifter::X86Lifter;
pub use registers::Reg32;
