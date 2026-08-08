use crate::pass::AnalysisPass;
use decompiler_core::{BranchTarget, Function, HighLevelAst, Instruction, Varnode};
use std::collections::HashSet;

/// Recovers structured control flow (`if`/`while`) from the CFG and stores
/// the result in [`Function::ast`].
///
/// The structurer implements [`AnalysisPass`] so it integrates naturally into
/// a [`PassManager`] pipeline (run after `CfgBuilder` and `ConstantPropagation`).
/// The resulting `HighLevelAst` is available to any consumer (e.g. `output-c`)
/// via `function.ast`.
///
/// # Algorithm
/// 1. Compute the set of **back-edges** (edges `(u, v)` where `v` dominates `u`
///    in a DFS tree) to distinguish loop back-edges from plain converging paths.
/// 2. Recursively descend the CFG. Conditional branches whose true-target is a
///    back-edge head are folded into `While`; others produce `If`/`If-Else`.
/// 3. Unstructured targets fall back to `Goto`.
pub struct Structurer;

impl AnalysisPass for Structurer {
    fn name(&self) -> &'static str {
        "structurer"
    }

    fn run(&self, function: &mut Function) {
        function.ast = Some(Self::do_structure(function));
    }
}

impl Structurer {
    /// Public entry point used by `output-c` when `function.ast` is not yet
    /// set (e.g. in unit tests that bypass the `PassManager`).
    pub fn structure(function: &Function) -> HighLevelAst {
        Self::do_structure(function)
    }

    fn do_structure(function: &Function) -> HighLevelAst {
        if function.blocks.is_empty() {
            return HighLevelAst::Seq(Vec::new());
        }
        let back_edges = find_back_edges(function);
        let mut visited = HashSet::new();
        Self::structure_region(function, function.entry, None, &mut visited, &back_edges)
    }

    fn structure_region(
        function: &Function,
        start_addr: u64,
        stop_addr: Option<u64>,
        visited: &mut HashSet<u64>,
        back_edges: &HashSet<(u64, u64)>,
    ) -> HighLevelAst {
        let mut seq = Vec::new();
        let mut curr_addr = start_addr;

        while Some(curr_addr) != stop_addr && !visited.contains(&curr_addr) {
            visited.insert(curr_addr);

            let block = match function.block_at(curr_addr) {
                Some(b) => b,
                None => break,
            };

            let mut ops = Vec::new();
            let mut term_cond: Option<Varnode> = None;
            let mut term_true: Option<u64> = None;
            let mut unconditional: Option<u64> = None;
            let mut is_return = false;

            for li in &block.instructions {
                for op in &li.ops {
                    match op {
                        Instruction::CBranch { condition, target } => {
                            term_cond = Some(condition.clone());
                            if let BranchTarget::Absolute(addr) = target {
                                term_true = Some(*addr);
                            }
                        }
                        Instruction::Branch { target } => {
                            if let BranchTarget::Absolute(addr) = target {
                                unconditional = Some(*addr);
                            }
                        }
                        Instruction::Return => is_return = true,
                        _ => ops.push(op.clone()),
                    }
                }
            }

            seq.push(HighLevelAst::Label(curr_addr));
            if !ops.is_empty() {
                seq.push(HighLevelAst::Block {
                    start_address: curr_addr,
                    instructions: ops,
                });
            }

            if let Some(cond) = term_cond {
                let true_target = term_true.unwrap_or(0);
                let false_target = *block
                    .successors
                    .iter()
                    .find(|&&s| s != true_target)
                    .unwrap_or(&0);

                // A back-edge to true_target means the condition controls a loop:
                // emit a While node (or fall back to a Goto for other back-edges).
                if back_edges.contains(&(curr_addr, true_target)) {
                    seq.push(HighLevelAst::While {
                        cond,
                        body: Box::new(HighLevelAst::Goto(true_target)),
                    });
                    curr_addr = false_target;
                } else if back_edges.contains(&(curr_addr, false_target)) {
                    // The fall-through path is the back-edge; treat as a do-while
                    seq.push(HighLevelAst::If {
                        cond,
                        then_body: Box::new(Self::structure_region(
                            function, true_target, Some(false_target), visited, back_edges,
                        )),
                        else_body: None,
                    });
                    seq.push(HighLevelAst::Goto(false_target));
                    break;
                } else {
                    // Plain if: false_target is the post-dominator (convergence point).
                    let then_ast = Self::structure_region(
                        function, true_target, Some(false_target), visited, back_edges,
                    );
                    seq.push(HighLevelAst::If {
                        cond,
                        then_body: Box::new(then_ast),
                        else_body: None,
                    });
                    curr_addr = false_target;
                }
            } else if let Some(target) = unconditional {
                // A back-edge on an unconditional branch is a loop tail.
                if back_edges.contains(&(curr_addr, target)) || visited.contains(&target) {
                    seq.push(HighLevelAst::Goto(target));
                    break;
                }
                curr_addr = target;
            } else if is_return {
                seq.push(HighLevelAst::Return);
                break;
            } else {
                // Fallthrough
                if let Some(&next) = block.successors.first() {
                    if visited.contains(&next) {
                        seq.push(HighLevelAst::Goto(next));
                        break;
                    }
                    curr_addr = next;
                } else {
                    break;
                }
            }
        }

        if seq.len() == 1 {
            seq.into_iter().next().unwrap()
        } else {
            HighLevelAst::Seq(seq)
        }
    }
}

/// Compute the set of back-edges in a DFS traversal of the CFG.
///
/// An edge `(from, to)` is a back-edge iff `to` is an ancestor of `from` in
/// the DFS spanning tree (i.e. `to` is currently on the DFS stack when we
/// discover the edge). This is the correct definition: it avoids the false
/// positives that arise from using a plain `visited` set (which conflates
/// cross-edges with back-edges in non-tree graphs).
fn find_back_edges(function: &Function) -> HashSet<(u64, u64)> {
    let mut back_edges = HashSet::new();
    let mut visited = HashSet::new();
    let mut in_stack = HashSet::new();
    dfs_back_edges(function, function.entry, &mut visited, &mut in_stack, &mut back_edges);
    back_edges
}

fn dfs_back_edges(
    function: &Function,
    addr: u64,
    visited: &mut HashSet<u64>,
    in_stack: &mut HashSet<u64>,
    back_edges: &mut HashSet<(u64, u64)>,
) {
    if !visited.insert(addr) {
        return;
    }
    in_stack.insert(addr);
    if let Some(block) = function.block_at(addr) {
        for &succ in &block.successors {
            if in_stack.contains(&succ) {
                // `succ` is on the current DFS path: this is a true back-edge.
                back_edges.insert((addr, succ));
            } else {
                dfs_back_edges(function, succ, visited, in_stack, back_edges);
            }
        }
    }
    in_stack.remove(&addr);
}

#[cfg(test)]
mod tests {
    use super::*;
    use decompiler_core::{BasicBlock, LiftedInstruction};

    fn reg(id: u64, size: u8) -> Varnode {
        Varnode::new(decompiler_core::AddressSpace::Register, id, size)
    }

    #[test]
    fn structures_simple_if_statement() {
        let mut f = Function::new("test", 0x1000);
        let mut b0 = BasicBlock::new(0x1000);
        b0.successors = vec![0x1010, 0x1020];
        b0.instructions.push(LiftedInstruction {
            address: 0x1000,
            ops: vec![Instruction::CBranch {
                condition: reg(1, 1),
                target: BranchTarget::Absolute(0x1010),
            }],
        });
        f.blocks.push(b0);

        let mut b1 = BasicBlock::new(0x1010);
        b1.successors = vec![0x1020];
        b1.instructions.push(LiftedInstruction {
            address: 0x1010,
            ops: vec![Instruction::Branch {
                target: BranchTarget::Absolute(0x1020),
            }],
        });
        f.blocks.push(b1);

        let mut b2 = BasicBlock::new(0x1020);
        b2.successors = vec![];
        b2.instructions.push(LiftedInstruction {
            address: 0x1020,
            ops: vec![Instruction::Return],
        });
        f.blocks.push(b2);

        // Rebuild index so block_at works O(1) without running CfgBuilder.
        f.rebuild_index();

        let ast = Structurer::structure(&f);
        let is_if = match ast {
            HighLevelAst::Seq(seq) => seq.iter().any(|n| matches!(n, HighLevelAst::If { .. })),
            _ => false,
        };
        assert!(is_if, "Expected an If node in the AST");
    }

    #[test]
    fn back_edge_detection_identifies_loop() {
        // entry -> body -> entry (back-edge)
        let mut f = Function::new("loop_test", 0x0);

        let mut entry = BasicBlock::new(0x0);
        entry.successors = vec![0x10, 0x20]; // branch: loop again or exit
        entry.instructions.push(LiftedInstruction {
            address: 0x0,
            ops: vec![Instruction::CBranch {
                condition: reg(0, 1),
                target: BranchTarget::Absolute(0x10),
            }],
        });
        f.blocks.push(entry);

        let mut body = BasicBlock::new(0x10);
        body.successors = vec![0x0]; // back-edge to loop header
        body.instructions.push(LiftedInstruction {
            address: 0x10,
            ops: vec![Instruction::Branch { target: BranchTarget::Absolute(0x0) }],
        });
        f.blocks.push(body);

        let mut exit_block = BasicBlock::new(0x20);
        exit_block.successors = vec![];
        exit_block.instructions.push(LiftedInstruction {
            address: 0x20,
            ops: vec![Instruction::Return],
        });
        f.blocks.push(exit_block);

        f.rebuild_index();

        let back = find_back_edges(&f);
        assert!(back.contains(&(0x10, 0x0)), "Expected back-edge from body to loop header");
        assert!(!back.contains(&(0x0, 0x10)), "Forward edge must not be a back-edge");
    }
}
