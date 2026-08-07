//! `ir-pcode`: the decompiler's Intermediate Representation.
//!
//! This crate is deliberately dependency-free and architecture-agnostic. It
//! defines only *data*: [`Varnode`] (a generalized value location) and
//! [`Instruction`] (a single low-level, p-code-inspired operation). Every
//! `arch-*` crate lifts machine instructions into `Vec<Instruction>`, and
//! every analysis pass in later phases consumes/produces the same types, so
//! this crate is the one piece of shared vocabulary the whole pipeline
//! agrees on.

mod instruction;
mod varnode;

pub use instruction::{BranchTarget, Instruction};
pub use varnode::{AddressSpace, TempAllocator, Varnode};
