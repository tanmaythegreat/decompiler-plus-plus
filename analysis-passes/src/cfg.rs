//! Control Flow Graph construction.
//!
//! Phase 1's `arch-x86` + `cli` pipeline lifts every machine instruction in
//! program order into a *single* flat `BasicBlock` -- branches are lifted
//! correctly (`Branch`/`CBranch` IR ops carry real target addresses), but
//! nothing yet groups instructions into real nodes or records edges between
//! them. [`CfgBuilder`] is that missing step: the roadmap's "first stage" of
//! the core analysis pipeline.
//!
//! The algorithm is the standard "leader" method (Aho/Ullman): find every
//! instruction address that must start a new block, then split the flat
//! instruction stream at those addresses, then connect the resulting blocks
//! by examining each block's terminator.
//!
//! A design choice worth calling out: `Call` is *not* treated as a block
//! terminator here. Intraprocedural control returns to the instruction right
//! after a call, so (unlike `Branch`/`CBranch`/`Return`) a call does not by
//! itself split the block it's in -- only real branches and the function's
//! own control-flow instructions do. This matches how Ghidra's default CFG
//! construction works and keeps the graph focused on *this* function's
//! control flow rather than blurring it with the call graph.

use crate::pass::AnalysisPass;
use decompiler_core::{BasicBlock, BranchTarget, Function, Instruction, LiftedInstruction};
use std::collections::BTreeSet;

/// The CFG-construction [`AnalysisPass`]. Always the first pass in a
/// pipeline: every later pass (dataflow analyses, type inference, ...)
/// assumes `Function::blocks` is already a real graph.
pub struct CfgBuilder;

impl AnalysisPass for CfgBuilder {
    fn name(&self) -> &'static str {
        "cfg-construction"
    }

    fn run(&self, function: &mut Function) {
        build(function);
    }
}

/// Rebuild `function.blocks` from scratch as a real CFG: split at leaders,
/// then wire up `successors`/`predecessors`. Exposed as a standalone
/// function (not just via [`CfgBuilder`]) so it's easy to call directly in
/// tests or other passes without going through a `PassManager`.
pub fn build(function: &mut Function) {
    let flat: Vec<LiftedInstruction> =
        function.blocks.drain(..).flat_map(|b| b.instructions).collect();

    if flat.is_empty() {
        return;
    }

    let leaders = find_leaders(&flat, function.entry);
    let mut blocks = split_at_leaders(&flat, &leaders);
    connect_edges(&mut blocks);
    function.blocks = blocks;
    // Rebuild the address→index map so that `block_at`/`block_at_mut`
    // remain O(1) for all subsequent passes (structurer, type inference, etc.)
    function.rebuild_index();
}

/// A block boundary exists wherever a leader address is; this finds every
/// such address per the classic three leader rules.
fn find_leaders(flat: &[LiftedInstruction], entry: u64) -> BTreeSet<u64> {
    let mut leaders = BTreeSet::new();

    // Rule 1: the function's entry point always starts a block. Fall back to
    // the first lifted instruction's address if `entry` doesn't match it for
    // some reason (defensive; keeps this pass total).
    leaders.insert(entry);
    leaders.insert(flat[0].address);

    for (i, li) in flat.iter().enumerate() {
        let mut is_terminator = false;

        for op in &li.ops {
            match op {
                // Rule 2: branch/call-with-return targets start a block.
                Instruction::Branch { target } => {
                    is_terminator = true;
                    if let BranchTarget::Absolute(addr) = target {
                        leaders.insert(*addr);
                    }
                }
                Instruction::CBranch { target, .. } => {
                    is_terminator = true;
                    if let BranchTarget::Absolute(addr) = target {
                        leaders.insert(*addr);
                    }
                }
                Instruction::Return => {
                    is_terminator = true;
                }
                // `Call` deliberately does not set `is_terminator` -- see
                // the module docs. Execution falls through to the next
                // instruction in the same block.
                _ => {}
            }
        }

        // Rule 3: whatever immediately follows a terminator starts a new
        // block (this holds whether or not the terminator's own target was
        // resolvable, e.g. an indirect jump still ends its block).
        if is_terminator {
            if let Some(next) = flat.get(i + 1) {
                leaders.insert(next.address);
            }
        }
    }

    leaders
}

/// Split the flat instruction stream into blocks at each leader address,
/// preserving program order.
fn split_at_leaders(flat: &[LiftedInstruction], leaders: &BTreeSet<u64>) -> Vec<BasicBlock> {
    let mut blocks: Vec<BasicBlock> = Vec::new();

    for li in flat {
        if leaders.contains(&li.address) || blocks.is_empty() {
            blocks.push(BasicBlock::new(li.address));
        }
        // Unwrap is safe: the `blocks.is_empty()` check above guarantees at
        // least one block exists by the time we get here.
        blocks.last_mut().unwrap().instructions.push(li.clone());
    }

    blocks
}

/// Examine each block's terminator (if any) and wire up
/// `successors`/`predecessors` accordingly. Falls back to a fallthrough edge
/// to the next block in program order when a block doesn't end in an
/// explicit `Branch`/`CBranch`/`Return`.
fn connect_edges(blocks: &mut [BasicBlock]) {
    let known_starts: BTreeSet<u64> = blocks.iter().map(|b| b.start_address).collect();
    let next_start: Vec<Option<u64>> =
        (0..blocks.len()).map(|i| blocks.get(i + 1).map(|b| b.start_address)).collect();

    let mut successors: Vec<Vec<u64>> = Vec::with_capacity(blocks.len());

    for (i, block) in blocks.iter().enumerate() {
        let mut succs = Vec::new();
        let mut saw_terminator = false;

        if let Some(last) = block.last_instruction() {
            for op in &last.ops {
                match op {
                    Instruction::Branch { target } => {
                        saw_terminator = true;
                        if let BranchTarget::Absolute(addr) = target {
                            if known_starts.contains(addr) {
                                succs.push(*addr);
                            }
                        }
                        // Indirect: left unresolved, same as the roadmap's
                        // guidance for jump-table-style targets.
                    }
                    Instruction::CBranch { target, .. } => {
                        saw_terminator = true;
                        if let BranchTarget::Absolute(addr) = target {
                            if known_starts.contains(addr) {
                                succs.push(*addr);
                            }
                        }
                        // The false-condition edge always falls through to
                        // the next block in program order.
                        if let Some(fallthrough) = next_start[i] {
                            succs.push(fallthrough);
                        }
                    }
                    Instruction::Return => {
                        saw_terminator = true;
                        // No outgoing edges: this block exits the function.
                    }
                    _ => {}
                }
            }
        }

        if !saw_terminator {
            if let Some(fallthrough) = next_start[i] {
                succs.push(fallthrough);
            }
        }

        succs.sort_unstable();
        succs.dedup();
        successors.push(succs);
    }

    for (block, succs) in blocks.iter_mut().zip(successors.iter()) {
        block.successors = succs.clone();
    }

    // Predecessors are just the reverse of the successor edges we just
    // computed.
    let mut preds: Vec<Vec<u64>> = vec![Vec::new(); blocks.len()];
    let start_to_index: std::collections::HashMap<u64, usize> =
        blocks.iter().enumerate().map(|(i, b)| (b.start_address, i)).collect();
    for (i, succs) in successors.iter().enumerate() {
        let from = blocks[i].start_address;
        for &succ_addr in succs {
            if let Some(&j) = start_to_index.get(&succ_addr) {
                preds[j].push(from);
            }
        }
    }
    for (block, mut p) in blocks.iter_mut().zip(preds.into_iter()) {
        p.sort_unstable();
        p.dedup();
        block.predecessors = p;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use decompiler_core::{Instruction, Varnode};

    fn li(address: u64, ops: Vec<Instruction>) -> LiftedInstruction {
        LiftedInstruction { address, ops }
    }

    fn cond() -> Varnode {
        Varnode::new(decompiler_core::AddressSpace::Register, 1000, 1)
    }

    /// mov eax, 5      @0x1000 (1 instr, 1 block so far)
    /// cmp eax, 8      @0x1005
    /// jne  0x1020     @0x100a  (cbranch)
    /// mov ebx, 1      @0x100f  (fallthrough / "true" path... well "false" path of jne)
    /// jmp  0x1025     @0x1014
    /// mov ebx, 0      @0x1020  (jne target)
    /// ret             @0x1025 (jmp target too)
    #[test]
    fn splits_diamond_shaped_function_into_four_blocks_with_correct_edges() {
        let mut f = Function::new("f", 0x1000);
        let mut flat_block = BasicBlock::new(0x1000);
        flat_block.instructions = vec![
            li(0x1000, vec![Instruction::Nop]), // stand-in for mov eax,5
            li(
                0x1005,
                vec![Instruction::IntEqual {
                    dest: cond(),
                    lhs: Varnode::constant(0, 4),
                    rhs: Varnode::constant(0, 4),
                }],
            ), // stand-in for cmp
            li(
                0x100a,
                vec![Instruction::CBranch {
                    condition: cond(),
                    target: BranchTarget::Absolute(0x1020),
                }],
            ), // jne 0x1020
            li(0x100f, vec![Instruction::Nop]), // mov ebx, 1
            li(0x1014, vec![Instruction::Branch { target: BranchTarget::Absolute(0x1025) }]), // jmp 0x1025
            li(0x1020, vec![Instruction::Nop]), // mov ebx, 0
            li(0x1025, vec![Instruction::Return]), // ret
        ];
        f.blocks.push(flat_block);

        build(&mut f);

        // Leaders: 0x1000 (entry), 0x100f (falls through jne), 0x1020 (jne
        // target, also falls through jmp), 0x1025 (jmp target, also falls
        // through ret... though ret ends the function so that doesn't add a
        // leader by itself here -- 0x1025 is a leader purely as jmp's target).
        let starts: Vec<u64> = f.blocks.iter().map(|b| b.start_address).collect();
        assert_eq!(starts, vec![0x1000, 0x100f, 0x1020, 0x1025]);

        let b0 = f.block_at(0x1000).unwrap();
        assert_eq!(b0.successors, vec![0x100f, 0x1020], "cbranch: fallthrough + target");

        let b1 = f.block_at(0x100f).unwrap();
        assert_eq!(b1.successors, vec![0x1025], "unconditional jmp target");

        let b2 = f.block_at(0x1020).unwrap();
        assert_eq!(b2.successors, vec![0x1025], "plain fallthrough into ret block");

        let b3 = f.block_at(0x1025).unwrap();
        assert!(b3.successors.is_empty(), "ret has no successors");
        assert_eq!(b3.predecessors, vec![0x100f, 0x1020]);
    }

    #[test]
    fn straight_line_function_is_a_single_block() {
        let mut f = Function::new("f", 0x2000);
        let mut flat_block = BasicBlock::new(0x2000);
        flat_block.instructions = vec![
            li(0x2000, vec![Instruction::Nop]),
            li(0x2001, vec![Instruction::Nop]),
            li(0x2002, vec![Instruction::Return]),
        ];
        f.blocks.push(flat_block);

        build(&mut f);

        assert_eq!(f.blocks.len(), 1);
        assert_eq!(f.blocks[0].instructions.len(), 3);
        assert!(f.blocks[0].successors.is_empty());
    }

    #[test]
    fn indirect_branch_leaves_successors_unresolved() {
        let mut f = Function::new("f", 0x3000);
        let mut flat_block = BasicBlock::new(0x3000);
        flat_block.instructions =
            vec![li(0x3000, vec![Instruction::Branch { target: BranchTarget::Indirect }])];
        f.blocks.push(flat_block);

        build(&mut f);

        assert_eq!(f.blocks.len(), 1);
        assert!(f.blocks[0].successors.is_empty());
    }
}
