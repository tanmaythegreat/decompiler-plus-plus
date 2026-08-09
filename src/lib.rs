// lib.rs — the decompiler as a library, so the CLI and the native viewer
// share one implementation.

pub mod analysis;
pub mod cfg;
pub mod cgen;
pub mod emu;
pub mod frame;
pub mod idiom;
pub mod ir;
pub mod json;
pub mod lifter;
pub mod ptr;
pub mod simplify;
pub mod flirt;
pub mod default_sigs;
