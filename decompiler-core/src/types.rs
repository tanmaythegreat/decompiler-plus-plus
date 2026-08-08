//! Shared structures used across the whole pipeline, independent of source
//! architecture. These are intentionally minimal in Phase 1 -- real CFG
//! edges, dominator trees, and SSA bookkeeping are Phase 2+ concerns -- but
//! they're defined here now so later phases extend rather than redefine
//! them.

use ir_pcode::{Instruction, Varnode};
use std::collections::HashMap;

/// One machine instruction's worth of lifted IR, kept together with the
/// source address it came from (needed for CFG edge resolution, debug
/// output, and mapping pseudocode back to the original bytes).
#[derive(Debug, Clone)]
pub struct LiftedInstruction {
    pub address: u64,
    pub ops: Vec<Instruction>,
}

/// A straight-line sequence of lifted instructions with a single entry.
///
/// Phase 1 only ever produced one flat `BasicBlock` per function (control
/// flow instructions were lifted, but nothing split the block or recorded
/// edges). Phase 2's `analysis-passes::cfg` pass is what turns that flat
/// stream into real graph nodes: it splits it at leaders and populates
/// `successors`/`predecessors` with the *start addresses* of neighboring
/// blocks. Addresses (rather than indices) are used so blocks remain valid
/// references even as `Function::blocks` is reordered or split further by
/// later passes; `Function::block_at` resolves an address to a block.
#[derive(Debug, Clone, Default)]
pub struct BasicBlock {
    pub start_address: u64,
    pub instructions: Vec<LiftedInstruction>,
    /// Start addresses of blocks this block can transfer control to.
    /// Empty until the CFG-construction pass runs.
    pub successors: Vec<u64>,
    /// Start addresses of blocks that can transfer control to this one.
    /// Kept symmetric with `successors` by whatever pass populates them.
    pub predecessors: Vec<u64>,
}

impl BasicBlock {
    pub fn new(start_address: u64) -> Self {
        BasicBlock {
            start_address,
            instructions: Vec::new(),
            successors: Vec::new(),
            predecessors: Vec::new(),
        }
    }

    pub fn push(&mut self, address: u64, ops: Vec<Instruction>) {
        self.instructions.push(LiftedInstruction { address, ops });
    }

    /// The last lifted machine instruction in the block, i.e. its
    /// terminator once a real CFG has been built. `None` for an empty
    /// block.
    pub fn last_instruction(&self) -> Option<&LiftedInstruction> {
        self.instructions.last()
    }
}

/// A decompiled function: a name/entry point plus its basic blocks. Once
/// `analysis-passes::cfg` has run, `blocks` plus each block's
/// `successors`/`predecessors` *is* the control flow graph -- there is no
/// separate graph type, since a `Vec<BasicBlock>` with address-linked edges
/// is enough to answer every question later passes ask (predecessors of a
/// block, reverse post-order, etc.) without an extra layer of indirection.
///
/// `block_index` is a derived index from `blocks` and must be kept in sync
/// by calling [`Function::rebuild_index`] any time `blocks` changes
/// (currently done by `CfgBuilder`).
#[derive(Debug, Clone)]
pub struct Function {
    pub name: String,
    pub entry: u64,
    pub blocks: Vec<BasicBlock>,
    pub types: std::collections::BTreeMap<Varnode, DataType>,
    /// The recovered high-level AST, populated by the `Structurer` pass.
    /// `None` until that pass runs.
    pub ast: Option<HighLevelAst>,
    /// Address → index into `blocks`. Rebuilt by `rebuild_index` after
    /// any change to `blocks`; `block_at`/`block_at_mut` use it for O(1)
    /// lookup.
    block_index: HashMap<u64, usize>,
}

impl Function {
    pub fn new(name: impl Into<String>, entry: u64) -> Self {
        Function {
            name: name.into(),
            entry,
            blocks: Vec::new(),
            types: std::collections::BTreeMap::new(),
            ast: None,
            block_index: HashMap::new(),
        }
    }

    /// Rebuild the address→index map from the current contents of `blocks`.
    /// Must be called after any modification to `blocks` (pushes, drains,
    /// reorders, splits). Currently called by `CfgBuilder` at the end of
    /// CFG construction.
    pub fn rebuild_index(&mut self) {
        self.block_index = self.blocks
            .iter()
            .enumerate()
            .map(|(i, b)| (b.start_address, i))
            .collect();
    }

    /// Look up the block starting at `address` in O(1) time.
    ///
    /// Falls back to a linear scan when [`rebuild_index`] has not yet been
    /// called (e.g. in unit tests that construct a `Function` directly without
    /// going through `CfgBuilder`). Production code always goes through the
    /// pass pipeline which calls `rebuild_index` at the end of `CfgBuilder`.
    pub fn block_at(&self, address: u64) -> Option<&BasicBlock> {
        if !self.block_index.is_empty() {
            self.block_index.get(&address).map(|&i| &self.blocks[i])
        } else {
            self.blocks.iter().find(|b| b.start_address == address)
        }
    }

    /// Mutable version of [`Function::block_at`].
    pub fn block_at_mut(&mut self, address: u64) -> Option<&mut BasicBlock> {
        if !self.block_index.is_empty() {
            self.block_index.get(&address).map(|&i| &mut self.blocks[i])
        } else {
            self.blocks.iter_mut().find(|b| b.start_address == address)
        }
    }

    /// The function's entry block, i.e. `block_at(self.entry)`.
    pub fn entry_block(&self) -> Option<&BasicBlock> {
        self.block_at(self.entry)
    }
}

/// Represents the inferred data type of a Varnode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DataType {
    Primitive { size: u8, signed: bool },
    Pointer(Box<DataType>),
    Array(Box<DataType>, u64),
    // Additional composite types can be added here
}

/// A named, typed program variable that (eventually, once type inference
/// runs) backs one or more Varnodes. Phase 1 only needs the identity
/// wrapper; `output-c` and the type-inference pass in later phases attach
/// real type information.
#[derive(Debug, Clone)]
pub struct Variable {
    pub varnode: Varnode,
    pub name: Option<String>,
    pub data_type: Option<DataType>,
}

impl Variable {
    pub fn new(varnode: Varnode) -> Self {
        Variable { varnode, name: None, data_type: None }
    }
}

/// A structured, high-level abstract syntax tree node used to represent
/// recovered control flow (if/else, loops) for pseudocode generation.
#[derive(Debug, Clone)]
pub enum HighLevelAst {
    /// A linear sequence of instructions from a basic block (minus control
    /// flow terminators). Named `Block` to avoid confusion with the CFG
    /// node type [`BasicBlock`].
    Block {
        start_address: u64,
        instructions: Vec<Instruction>,
    },
    /// A sequence of statements or structured blocks
    Seq(Vec<HighLevelAst>),
    /// An if statement, optionally with an else clause
    If {
        cond: Varnode,
        then_body: Box<HighLevelAst>,
        else_body: Option<Box<HighLevelAst>>,
    },
    /// A while loop
    While {
        cond: Varnode,
        body: Box<HighLevelAst>,
    },
    /// A fallback goto for unstructured control flow
    Goto(u64),
    /// A label marking the destination of a goto
    Label(u64),
    /// Return statement
    Return,
}
