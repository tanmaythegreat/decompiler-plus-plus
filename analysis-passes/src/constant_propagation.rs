//! Constant propagation: the roadmap's example "first-order data-flow
//! analysis pass". `ConstantPropagation` is both a [`TransferFunction`] (so
//! [`dataflow::solve_forward`] can compute, for every program point, which
//! Varnodes are provably a single known constant) and an [`AnalysisPass`]
//! (so it can also *rewrite* the IR using that information: substituting
//! known-constant operands and folding pure arithmetic/comparison
//! instructions with all-constant operands down to a plain `Copy`).
//!
//! The lattice (`ConstValue`) is the textbook three-level one:
//!
//! ```text
//!        Top            <- "no information yet"
//!       /   \
//!  Const(a) Const(b) ...  <- "exactly this one known value"
//!       \   /
//!    NotConstant        <- "proven not a single constant" (e.g. two
//!                            different values merge here from a branch)
//! ```
//!
//! `merge` only ever moves a value *down* this diagram (Top -> some Const ->
//! NotConstant), which is what guarantees the fixpoint iteration terminates.

use crate::dataflow::{self, DataflowResult, Lattice, TransferFunction};
use crate::pass::AnalysisPass;
use decompiler_core::{AddressSpace, Function, Instruction, Varnode};
use std::collections::HashMap;

/// A single Varnode's abstract value at some program point.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConstValue {
    Top,
    Const(u64),
    NotConstant,
}

impl ConstValue {
    fn merge(&self, other: &Self) -> Self {
        use ConstValue::*;
        match (*self, *other) {
            (Top, x) | (x, Top) => x,
            (NotConstant, _) | (_, NotConstant) => NotConstant,
            (Const(a), Const(b)) => {
                if a == b {
                    Const(a)
                } else {
                    NotConstant
                }
            }
        }
    }
}

/// The dataflow state: known constant values for every Varnode the analysis
/// has an opinion about. Varnodes with no entry are implicitly `Top` --
/// this keeps the map small (only ever grows for things actually assigned
/// to) rather than needing to be pre-populated with every Varnode in the
/// function.
#[derive(Debug, Clone, PartialEq)]
pub struct ConstState {
    values: HashMap<Varnode, ConstValue>,
}

impl ConstState {
    /// The abstract value of `vn`: trivially `Const(offset)` for an
    /// immediate/`Constant`-space Varnode (its value doesn't depend on any
    /// analysis), otherwise whatever this state currently knows about it (or
    /// `Top` if it's never been written).
    pub fn get(&self, vn: &Varnode) -> ConstValue {
        if vn.space == AddressSpace::Constant {
            return ConstValue::Const(vn.offset);
        }
        self.values.get(vn).copied().unwrap_or(ConstValue::Top)
    }

    fn set(&mut self, vn: Varnode, value: ConstValue) {
        self.values.insert(vn, value);
    }
}

impl Lattice for ConstState {
    fn top() -> Self {
        ConstState { values: HashMap::new() }
    }

    fn merge(&self, other: &Self) -> Self {
        let mut merged = HashMap::new();
        for key in self.values.keys().chain(other.values.keys()) {
            if merged.contains_key(key) {
                continue;
            }
            let m = self.get(key).merge(&other.get(key));
            // Omitting `Top` entries keeps the map from growing unboundedly
            // with entries that carry no information -- `get` already
            // defaults missing keys to `Top`.
            if m != ConstValue::Top {
                merged.insert(key.clone(), m);
            }
        }
        ConstState { values: merged }
    }
}

/// Mask a raw `u64` computation result down to `size` bytes, matching the
/// Varnode's actual width (so e.g. an 8-bit add wraps at 256, not 2^64).
fn mask(value: u64, size: u8) -> u64 {
    let bits = size as u32 * 8;
    if bits == 0 || bits >= 64 {
        value
    } else {
        value & ((1u64 << bits) - 1)
    }
}

/// Sign-extend a `size`-byte value to `i64` for signed comparisons.
fn signed(value: u64, size: u8) -> i64 {
    let bits = size as u32 * 8;
    if bits == 0 || bits >= 64 {
        return value as i64;
    }
    let x = value & ((1u64 << bits) - 1);
    let sign_bit = 1u64 << (bits - 1);
    if x & sign_bit != 0 {
        (x as i64) - (1i64 << bits)
    } else {
        x as i64
    }
}

/// Evaluate a binary op over two abstract values, propagating `Top`/
/// `NotConstant` appropriately: only `Const op Const` produces a new
/// `Const`; anything involving `NotConstant` is `NotConstant`; two `Top`s
/// (an operand that's genuinely never been assigned) stay `Top`.
fn eval_binop(a: ConstValue, b: ConstValue, f: impl Fn(u64, u64) -> u64) -> ConstValue {
    match (a, b) {
        (ConstValue::Const(x), ConstValue::Const(y)) => ConstValue::Const(f(x, y)),
        (ConstValue::NotConstant, _) | (_, ConstValue::NotConstant) => ConstValue::NotConstant,
        _ => ConstValue::Top,
    }
}

/// The pass itself. Stateless -- all state lives in the `ConstState` the
/// dataflow solver threads through -- so a single value can be reused both
/// as the [`TransferFunction`] passed to [`dataflow::solve_forward`] and as
/// the [`AnalysisPass`] a `PassManager` runs.
pub struct ConstantPropagation;

impl TransferFunction for ConstantPropagation {
    type State = ConstState;

    fn transfer_instruction(&self, op: &Instruction, state: &mut ConstState) {
        match op {
            Instruction::Copy { dest, src } => {
                let v = state.get(src);
                state.set(dest.clone(), v);
            }
            Instruction::IntAdd { dest, lhs, rhs } => {
                let v = eval_binop(state.get(lhs), state.get(rhs), |a, b| {
                    mask(a.wrapping_add(b), dest.size)
                });
                state.set(dest.clone(), v);
            }
            Instruction::IntSub { dest, lhs, rhs } => {
                let v = eval_binop(state.get(lhs), state.get(rhs), |a, b| {
                    mask(a.wrapping_sub(b), dest.size)
                });
                state.set(dest.clone(), v);
            }
            Instruction::IntEqual { dest, lhs, rhs } => {
                let size = lhs.size;
                let v = eval_binop(state.get(lhs), state.get(rhs), move |a, b| {
                    (mask(a, size) == mask(b, size)) as u64
                });
                state.set(dest.clone(), v);
            }
            Instruction::IntNotEqual { dest, lhs, rhs } => {
                let size = lhs.size;
                let v = eval_binop(state.get(lhs), state.get(rhs), move |a, b| {
                    (mask(a, size) != mask(b, size)) as u64
                });
                state.set(dest.clone(), v);
            }
            Instruction::IntSless { dest, lhs, rhs } => {
                let size = lhs.size;
                let v = eval_binop(state.get(lhs), state.get(rhs), move |a, b| {
                    (signed(a, size) < signed(b, size)) as u64
                });
                state.set(dest.clone(), v);
            }
            Instruction::IntSlessEqual { dest, lhs, rhs } => {
                let size = lhs.size;
                let v = eval_binop(state.get(lhs), state.get(rhs), move |a, b| {
                    (signed(a, size) <= signed(b, size)) as u64
                });
                state.set(dest.clone(), v);
            }
            // We don't model memory contents in this pass, so a load's
            // result is conservatively unknown -- but it's still a *known*
            // unknown, i.e. `NotConstant`, not `Top` (a subsequent block
            // that reads it shouldn't re-treat it as "never assigned").
            Instruction::Load { dest, .. } => {
                state.set(dest.clone(), ConstValue::NotConstant);
            }
            // A phi-node's result is the join of all incoming values.
            // If every predecessor assigns the same constant the result is
            // that constant; otherwise it's `NotConstant`.
            Instruction::Phi { dest, srcs } => {
                let v = srcs.iter().fold(ConstValue::Top, |acc, src| acc.merge(&state.get(src)));
                state.set(dest.clone(), v);
            }
            // No tracked Varnode is defined by a store, branch, call,
            // return, or nop.
            Instruction::Store { .. }
            | Instruction::Branch { .. }
            | Instruction::CBranch { .. }
            | Instruction::Call { .. }
            | Instruction::Return
            | Instruction::Nop => {}
        }
    }
}

impl AnalysisPass for ConstantPropagation {
    fn name(&self) -> &'static str {
        "constant-propagation"
    }

    fn run(&self, function: &mut Function) {
        let result = dataflow::solve_forward(&*function, self);
        rewrite(function, self, &result);
    }
}

/// Replace a *use* Varnode with a literal `Constant` Varnode carrying the
/// same value, if the analysis proved it's a known constant at this point.
/// Never touches `dest`/definition positions -- substituting there would be
/// meaningless (you can't "assign to a constant").
fn substitute(vn: &Varnode, state: &ConstState) -> Varnode {
    match state.get(vn) {
        ConstValue::Const(v) if vn.space != AddressSpace::Constant => {
            Varnode::constant(v, vn.size)
        }
        _ => vn.clone(),
    }
}

/// Rewrite every instruction in `function` using the solved dataflow
/// `result`: substitute known-constant operands, and fold any
/// arithmetic/comparison instruction whose operands are *both* constant
/// after substitution into a single `Copy` of the computed result.
fn rewrite(function: &mut Function, transfer: &ConstantPropagation, result: &DataflowResult<ConstState>) {
    for block in &mut function.blocks {
        let mut state =
            result.block_in.get(&block.start_address).cloned().unwrap_or_else(ConstState::top);

        for li in &mut block.instructions {
            let mut new_ops = Vec::with_capacity(li.ops.len());
            for op in &li.ops {
                new_ops.push(substitute_and_fold(op, &state));
                // Advance the running state using the *original* op --
                // semantically identical to using the rewritten one (both
                // resolve to the same abstract values through `get`), and
                // avoids re-deriving transfer logic here.
                transfer.transfer_instruction(op, &mut state);
            }
            li.ops = new_ops;
        }
    }
}

fn substitute_and_fold(op: &Instruction, state: &ConstState) -> Instruction {
    match op {
        Instruction::Copy { dest, src } => {
            Instruction::Copy { dest: dest.clone(), src: substitute(src, state) }
        }
        Instruction::Load { dest, addr } => {
            Instruction::Load { dest: dest.clone(), addr: substitute(addr, state) }
        }
        Instruction::Store { addr, src } => {
            Instruction::Store { addr: substitute(addr, state), src: substitute(src, state) }
        }
        Instruction::IntAdd { dest, lhs, rhs } => fold_or_keep(dest, lhs, rhs, state, |a, b| {
            mask(a.wrapping_add(b), dest.size)
        })
        .unwrap_or_else(|| Instruction::IntAdd {
            dest: dest.clone(),
            lhs: substitute(lhs, state),
            rhs: substitute(rhs, state),
        }),
        Instruction::IntSub { dest, lhs, rhs } => fold_or_keep(dest, lhs, rhs, state, |a, b| {
            mask(a.wrapping_sub(b), dest.size)
        })
        .unwrap_or_else(|| Instruction::IntSub {
            dest: dest.clone(),
            lhs: substitute(lhs, state),
            rhs: substitute(rhs, state),
        }),
        Instruction::IntEqual { dest, lhs, rhs } => {
            let size = lhs.size;
            fold_or_keep(dest, lhs, rhs, state, move |a, b| (mask(a, size) == mask(b, size)) as u64)
                .unwrap_or_else(|| Instruction::IntEqual {
                    dest: dest.clone(),
                    lhs: substitute(lhs, state),
                    rhs: substitute(rhs, state),
                })
        }
        Instruction::IntNotEqual { dest, lhs, rhs } => {
            let size = lhs.size;
            fold_or_keep(dest, lhs, rhs, state, move |a, b| (mask(a, size) != mask(b, size)) as u64)
                .unwrap_or_else(|| Instruction::IntNotEqual {
                    dest: dest.clone(),
                    lhs: substitute(lhs, state),
                    rhs: substitute(rhs, state),
                })
        }
        Instruction::IntSless { dest, lhs, rhs } => {
            let size = lhs.size;
            fold_or_keep(dest, lhs, rhs, state, move |a, b| (signed(a, size) < signed(b, size)) as u64)
                .unwrap_or_else(|| Instruction::IntSless {
                    dest: dest.clone(),
                    lhs: substitute(lhs, state),
                    rhs: substitute(rhs, state),
                })
        }
        Instruction::IntSlessEqual { dest, lhs, rhs } => {
            let size = lhs.size;
            fold_or_keep(dest, lhs, rhs, state, move |a, b| (signed(a, size) <= signed(b, size)) as u64)
                .unwrap_or_else(|| Instruction::IntSlessEqual {
                    dest: dest.clone(),
                    lhs: substitute(lhs, state),
                    rhs: substitute(rhs, state),
                })
        }
        Instruction::Branch { target } => Instruction::Branch { target: *target },
        Instruction::CBranch { condition, target } => {
            Instruction::CBranch { condition: substitute(condition, state), target: *target }
        }
        Instruction::Call { target } => Instruction::Call { target: *target },
        Instruction::Return => Instruction::Return,
        Instruction::Nop => Instruction::Nop,
        // Phi sources are use sites — substitute known-constant srcs.
        Instruction::Phi { dest, srcs } => Instruction::Phi {
            dest: dest.clone(),
            srcs: srcs.iter().map(|s| substitute(s, state)).collect(),
        },
    }
}

/// If both `lhs` and `rhs` are known constants at this point, fold the
/// instruction down to that literal result. Returns `None` (meaning "keep
/// it as an instruction, just with substituted operands") if either operand
/// isn't currently known.
fn fold_or_keep(
    dest: &Varnode,
    lhs: &Varnode,
    rhs: &Varnode,
    state: &ConstState,
    f: impl Fn(u64, u64) -> u64,
) -> Option<Instruction> {
    match (state.get(lhs), state.get(rhs)) {
        (ConstValue::Const(a), ConstValue::Const(b)) => {
            Some(Instruction::Copy { dest: dest.clone(), src: Varnode::constant(f(a, b), dest.size) })
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use decompiler_core::{BasicBlock, BranchTarget, LiftedInstruction};

    fn reg(id: u64, size: u8, name: &str) -> Varnode {
        Varnode::named(AddressSpace::Register, id, size, name)
    }

    fn li(address: u64, ops: Vec<Instruction>) -> LiftedInstruction {
        LiftedInstruction { address, ops }
    }

    /// `eax = 5; eax = eax + 3; ret` -- both the copy and the add should
    /// fold: the add ends up as `eax = COPY 0x8`.
    #[test]
    fn folds_straight_line_constant_arithmetic() {
        let eax = reg(0, 4, "EAX");
        let mut block = BasicBlock::new(0x0);
        block.instructions = vec![
            li(0x0, vec![Instruction::Copy { dest: eax.clone(), src: Varnode::constant(5, 4) }]),
            li(
                0x1,
                vec![Instruction::IntAdd {
                    dest: eax.clone(),
                    lhs: eax.clone(),
                    rhs: Varnode::constant(3, 4),
                }],
            ),
            li(0x2, vec![Instruction::Return]),
        ];
        let mut function = Function::new("f", 0x0);
        function.blocks.push(block);

        ConstantPropagation.run(&mut function);

        match &function.blocks[0].instructions[1].ops[0] {
            Instruction::Copy { dest, src } => {
                assert_eq!(*dest, eax);
                assert_eq!(*src, Varnode::constant(8, 4));
            }
            other => panic!("expected the fold to produce a Copy, got {other:?}"),
        }
    }

    /// Two paths assign `EAX` two *different* constants before merging; the
    /// join block must NOT fold a use of `EAX` to either value -- that would
    /// be unsound. This is the whole reason the lattice has a `NotConstant`
    /// bottom rather than just `Top`/`Const`.
    #[test]
    fn does_not_unsoundly_fold_across_divergent_constant_paths() {
        let eax = reg(0, 4, "EAX");

        let mut entry = BasicBlock::new(0x0);
        entry.instructions.push(li(
            0x0,
            vec![Instruction::CBranch {
                condition: Varnode::constant(1, 1),
                target: BranchTarget::Absolute(0x20),
            }],
        ));
        entry.successors = vec![0x10, 0x20];

        let mut left = BasicBlock::new(0x10);
        left.instructions.push(li(
            0x10,
            vec![Instruction::Copy { dest: eax.clone(), src: Varnode::constant(5, 4) }],
        ));
        left.predecessors = vec![0x0];
        left.successors = vec![0x30];

        let mut right = BasicBlock::new(0x20);
        right.instructions.push(li(
            0x20,
            vec![Instruction::Copy { dest: eax.clone(), src: Varnode::constant(6, 4) }],
        ));
        right.predecessors = vec![0x0];
        right.successors = vec![0x30];

        let mut join = BasicBlock::new(0x30);
        join.instructions.push(li(
            0x30,
            vec![Instruction::IntAdd {
                dest: eax.clone(),
                lhs: eax.clone(),
                rhs: Varnode::constant(1, 4),
            }],
        ));
        join.predecessors = vec![0x10, 0x20];

        let mut function = Function::new("f", 0x0);
        function.blocks = vec![entry, left, right, join];

        ConstantPropagation.run(&mut function);

        // The join block's add must still read EAX as a register, not have
        // been folded to a literal Copy.
        match &function.blocks[3].instructions[0].ops[0] {
            Instruction::IntAdd { lhs, .. } => {
                assert_eq!(*lhs, eax, "must not substitute a NotConstant value with a literal");
            }
            other => panic!("expected the add to survive unfolded, got {other:?}"),
        }
    }
}
