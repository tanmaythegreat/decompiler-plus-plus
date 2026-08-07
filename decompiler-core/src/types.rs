//! Shared structures used across the whole pipeline, independent of source
//! architecture. These are intentionally minimal in Phase 1 -- real CFG
//! edges, dominator trees, and SSA bookkeeping are Phase 2+ concerns -- but
//! they're defined here now so later phases extend rather than redefine
//! them.

use ir_pcode::{Instruction, Varnode};

/// One machine instruction's worth of lifted IR, kept together with the
/// source address it came from (needed for CFG edge resolution, debug
/// output, and mapping pseudocode back to the original bytes).
#[derive(Debug, Clone)]
pub struct LiftedInstruction {
    pub address: u64,
    pub ops: Vec<Instruction>,
}

/// A straight-line sequence of lifted instructions with a single entry and
/// (in later phases) a single exit. Phase 1 does not yet build real CFG
/// edges between blocks -- see the roadmap's Phase 2 -- so this currently
/// just accumulates instructions in program order.
#[derive(Debug, Clone, Default)]
pub struct BasicBlock {
    pub start_address: u64,
    pub instructions: Vec<LiftedInstruction>,
}

impl BasicBlock {
    pub fn new(start_address: u64) -> Self {
        BasicBlock { start_address, instructions: Vec::new() }
    }

    pub fn push(&mut self, address: u64, ops: Vec<Instruction>) {
        self.instructions.push(LiftedInstruction { address, ops });
    }
}

/// A decompiled function: a name/entry point plus its (currently
/// unstructured) basic blocks. `analysis-passes` in Phase 2 is responsible
/// for turning the raw block list into an actual graph.
#[derive(Debug, Clone)]
pub struct Function {
    pub name: String,
    pub entry: u64,
    pub blocks: Vec<BasicBlock>,
}

impl Function {
    pub fn new(name: impl Into<String>, entry: u64) -> Self {
        Function { name: name.into(), entry, blocks: Vec::new() }
    }
}

/// A named, typed program variable that (eventually, once type inference
/// runs) backs one or more Varnodes. Phase 1 only needs the identity
/// wrapper; `output-c` and the type-inference pass in later phases attach
/// real type information.
#[derive(Debug, Clone)]
pub struct Variable {
    pub varnode: Varnode,
    pub name: Option<String>,
}

impl Variable {
    pub fn new(varnode: Varnode) -> Self {
        Variable { varnode, name: None }
    }
}
