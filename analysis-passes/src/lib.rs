//! `analysis-passes`: analytical transformations applied to the IR, per the
//! roadmap's "Core Analysis Pipeline" stage.
//!
//! This is Phase 2's crate. It builds on `decompiler-core`'s `Function`/
//! `BasicBlock` types and adds:
//!
//! - [`pass::AnalysisPass`] -- the shared interface every transformation in
//!   this crate (and future ones, like Phase 3's type inference) implements.
//! - [`cfg`] -- Control Flow Graph construction: turns the flat instruction
//!   stream `arch-x86` lifts into real basic blocks with successor/
//!   predecessor edges.
//! - [`dataflow`] -- a small monotone-framework dataflow engine (a
//!   from-scratch stand-in for `rustc_mir_dataflow`/`lattices`, since this
//!   workspace has no network access to depend on them directly).
//! - [`constant_propagation`] -- the roadmap's example analysis, built on
//!   top of `dataflow`: figures out which Varnodes are provably constant at
//!   each program point, then rewrites the IR to fold/substitute them.

pub mod cfg;
pub mod constant_propagation;
pub mod dataflow;
pub mod pass;
pub mod structurer;
pub mod type_inference;

pub use cfg::CfgBuilder;
pub use constant_propagation::ConstantPropagation;
pub use dataflow::{DataflowResult, Lattice, TransferFunction};
pub use pass::{AnalysisPass, PassManager};
pub use structurer::Structurer;
pub use type_inference::TypeInference;
