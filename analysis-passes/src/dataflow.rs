//! A small, self-contained forward dataflow engine, built on the same
//! monotone-framework theory `rustc_mir_dataflow` uses (a lattice of
//! "abstract states" plus per-instruction transfer functions, iterated to a
//! fixpoint over the CFG). The roadmap's original suggestion was to build on
//! `rustc_mir_dataflow` and the `lattices` crate directly; this workspace
//! has no network access to pull in crates.io dependencies, so this module
//! is a deliberately minimal from-scratch stand-in with the same shape --
//! a [`Lattice`] trait and a worklist fixpoint solver -- so it could be
//! swapped for those crates later without changing how [`constant_propagation`](crate::constant_propagation)
//! or future passes use it.
//!
//! Only the forward direction is implemented: it's what constant
//! propagation needs, and it's what's exercised by tests. A backward
//! solver (for e.g. a future liveness / dead-code-elimination pass) is the
//! natural next extension of this module, not a redesign of it -- the same
//! `Lattice` trait and worklist structure apply, just walking
//! `predecessors`/`successors` and instruction order in the opposite
//! direction.

use decompiler_core::{BasicBlock, Function, Instruction};
use std::collections::{HashMap, HashSet, VecDeque};

/// A join-semilattice of "abstract states" a dataflow analysis tracks at
/// each program point (e.g., for constant propagation: a mapping from
/// Varnode to "known constant / not constant / not yet determined").
///
/// Implementations must satisfy the standard monotone-framework laws for the
/// fixpoint iteration in [`solve_forward`] to be guaranteed to terminate:
/// `merge` must be commutative, associative, idempotent, and can only move a
/// value *down* the lattice (towards less precise information), never back
/// up towards [`Lattice::top`].
pub trait Lattice: Clone + PartialEq {
    /// The "no information yet" starting value: what an unvisited program
    /// point, or a block with no predecessors, is assumed to hold before any
    /// analysis has run.
    fn top() -> Self;

    /// Combine two states that flow into the same program point along
    /// different CFG paths (e.g. from two different predecessors of a block
    /// with multiple incoming edges).
    fn merge(&self, other: &Self) -> Self;
}

/// The per-instruction effect a concrete analysis has on its [`Lattice`]
/// state. `constant_propagation::ConstLattice`'s implementation, for
/// example, updates its map of known-constant Varnodes based on what each
/// `Instruction` computes.
pub trait TransferFunction {
    type State: Lattice;

    /// Apply the effect of a single IR instruction to `state`, in place.
    /// Called once per `Instruction` in program order as the solver walks
    /// each block from its `IN` state to its `OUT` state.
    fn transfer_instruction(&self, op: &Instruction, state: &mut Self::State);
}

/// The fixpoint result of a forward dataflow analysis: the state at the
/// start (`block_in`) and end (`block_out`) of every block, keyed by each
/// block's `start_address`.
pub struct DataflowResult<S> {
    pub block_in: HashMap<u64, S>,
    pub block_out: HashMap<u64, S>,
}

/// Run a forward dataflow analysis to a fixpoint over `function`'s CFG using
/// the worklist algorithm: seed every block's `OUT` state to
/// [`Lattice::top`], then repeatedly recompute any block whose predecessors'
/// `OUT` states have changed, until nothing changes.
///
/// Requires `function`'s blocks to already have real `successors`/
/// `predecessors` edges -- i.e. [`crate::cfg::build`] (or [`crate::cfg::CfgBuilder`])
/// must have already run.
pub fn solve_forward<T: TransferFunction>(
    function: &Function,
    transfer: &T,
) -> DataflowResult<T::State> {
    let block_by_addr: HashMap<u64, &BasicBlock> =
        function.blocks.iter().map(|b| (b.start_address, b)).collect();

    let mut block_in: HashMap<u64, T::State> = HashMap::new();
    let mut block_out: HashMap<u64, T::State> = HashMap::new();
    for b in &function.blocks {
        block_out.insert(b.start_address, T::State::top());
    }

    let mut queue: VecDeque<u64> = function.blocks.iter().map(|b| b.start_address).collect();
    let mut queued: HashSet<u64> = queue.iter().copied().collect();

    while let Some(addr) = queue.pop_front() {
        queued.remove(&addr);
        // `block_by_addr` maps to `&BasicBlock`, so `.get()` hands back
        // `Option<&&BasicBlock>` -- deref once to get back to `&BasicBlock`.
        let block: &BasicBlock = match block_by_addr.get(&addr) {
            Some(b) => *b,
            None => continue,
        };

        let in_state = merge_predecessor_states::<T>(block, &block_out);
        block_in.insert(addr, in_state.clone());

        let out_state = transfer_block(block, in_state, transfer);

        let changed = block_out.get(&addr) != Some(&out_state);
        if changed {
            block_out.insert(addr, out_state);
            for succ in &block.successors {
                if queued.insert(*succ) {
                    queue.push_back(*succ);
                }
            }
        }
    }

    DataflowResult { block_in, block_out }
}

/// Per-machine-instruction states (keyed by instruction address) computed by
/// re-walking each block from its already-solved `IN` state. This is the
/// finer-grained view a rewrite pass (like constant propagation's) needs: it
/// wants to know "what's known at the point *this specific instruction*
/// runs", not just at block boundaries.
pub fn instruction_states<T: TransferFunction>(
    function: &Function,
    transfer: &T,
    result: &DataflowResult<T::State>,
) -> HashMap<u64, T::State> {
    let mut states = HashMap::new();
    for block in &function.blocks {
        let mut state =
            result.block_in.get(&block.start_address).cloned().unwrap_or_else(T::State::top);
        for li in &block.instructions {
            states.insert(li.address, state.clone());
            for op in &li.ops {
                transfer.transfer_instruction(op, &mut state);
            }
        }
    }
    states
}

fn merge_predecessor_states<T: TransferFunction>(
    block: &BasicBlock,
    block_out: &HashMap<u64, T::State>,
) -> T::State {
    if block.predecessors.is_empty() {
        return T::State::top();
    }
    let mut acc: Option<T::State> = None;
    for pred in &block.predecessors {
        let pred_out = block_out.get(pred).cloned().unwrap_or_else(T::State::top);
        acc = Some(match acc {
            None => pred_out,
            Some(a) => a.merge(&pred_out),
        });
    }
    acc.unwrap_or_else(T::State::top)
}

fn transfer_block<T: TransferFunction>(
    block: &BasicBlock,
    in_state: T::State,
    transfer: &T,
) -> T::State {
    let mut state = in_state;
    for li in &block.instructions {
        for op in &li.ops {
            transfer.transfer_instruction(op, &mut state);
        }
    }
    state
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A trivial two-value lattice (`Top` / `Reached`) just to exercise the
    /// solver's fixpoint/worklist mechanics independent of any real
    /// analysis: "was this program point reached at all?".
    #[derive(Clone, PartialEq, Debug)]
    enum Reachability {
        Top,
        Reached,
    }
    impl Lattice for Reachability {
        fn top() -> Self {
            Reachability::Top
        }
        fn merge(&self, other: &Self) -> Self {
            match (self, other) {
                (Reachability::Reached, _) | (_, Reachability::Reached) => Reachability::Reached,
                _ => Reachability::Top,
            }
        }
    }
    struct MarkReached;
    impl TransferFunction for MarkReached {
        type State = Reachability;
        fn transfer_instruction(&self, _op: &Instruction, state: &mut Self::State) {
            *state = Reachability::Reached;
        }
    }

    #[test]
    fn diamond_cfg_reaches_fixpoint() {
        use decompiler_core::{BranchTarget, LiftedInstruction};

        // entry -> {left, right} -> join
        let mut entry = BasicBlock::new(0x0);
        entry.instructions.push(LiftedInstruction {
            address: 0x0,
            ops: vec![Instruction::CBranch {
                condition: decompiler_core::Varnode::constant(1, 1),
                target: BranchTarget::Absolute(0x20),
            }],
        });
        entry.successors = vec![0x10, 0x20];

        let mut left = BasicBlock::new(0x10);
        left.instructions
            .push(LiftedInstruction { address: 0x10, ops: vec![Instruction::Nop] });
        left.predecessors = vec![0x0];
        left.successors = vec![0x30];

        let mut right = BasicBlock::new(0x20);
        right.instructions
            .push(LiftedInstruction { address: 0x20, ops: vec![Instruction::Nop] });
        right.predecessors = vec![0x0];
        right.successors = vec![0x30];

        let mut join = BasicBlock::new(0x30);
        join.instructions
            .push(LiftedInstruction { address: 0x30, ops: vec![Instruction::Return] });
        join.predecessors = vec![0x10, 0x20];

        let mut function = Function::new("f", 0x0);
        function.blocks = vec![entry, left, right, join];

        let result = solve_forward(&function, &MarkReached);

        assert_eq!(result.block_out[&0x0], Reachability::Reached);
        assert_eq!(result.block_out[&0x10], Reachability::Reached);
        assert_eq!(result.block_out[&0x20], Reachability::Reached);
        // The join block's IN state must be `Reached` (merged from both
        // predecessors), not `Top` -- this is what would break if `merge`
        // or the worklist re-queueing were wrong.
        assert_eq!(result.block_in[&0x30], Reachability::Reached);
    }
}
