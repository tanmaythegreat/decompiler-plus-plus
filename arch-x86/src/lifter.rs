//! Lifts [`X86Instruction`]s into the shared IR.
//!
//! Following the roadmap's recommendation, this is *manual* per-instruction
//! lifting (one Rust function per x86 instruction shape) rather than a
//! SLEIGH-style DSL: each function takes a decoded instruction and returns
//! the `Vec<Instruction>` that reproduces its semantics.
//!
//! Flag handling is deliberately simplified for Phase 1: real x86 EFLAGS has
//! six-plus status bits (CF, PF, AF, ZF, SF, OF) computed from the same
//! result via different formulas. Modeling all of them faithfully is future
//! work; here `CMP`/flag-consuming branches only track a single "zero flag"
//! Varnode, which is enough to demonstrate the CMP -> Jcc lifting pattern
//! the roadmap calls out (decomposing one machine instruction into several
//! IR ops, one per flag) without the full EFLAGS model.

use crate::decoder::X86Instruction;
use crate::registers::Reg32;
use decompiler_core::{BranchTarget, Disassembler, Lifter};
use ir_pcode::{Instruction, TempAllocator, Varnode};

const REG_SIZE: u8 = 4;

/// Reserved register-space slot for the simplified zero flag. Real hardware
/// register ids (0..=7, matching ModRM encoding) are used for the GPRs, so
/// this id is chosen well outside that range to avoid collisions.
const ZF_REG_ID: u64 = 1000;

fn reg_varnode(r: Reg32) -> Varnode {
    Varnode::register(r.id(), REG_SIZE, r.name())
}

fn zf_varnode() -> Varnode {
    Varnode::named(ir_pcode::AddressSpace::Register, ZF_REG_ID, 1, "ZF")
}

fn imm_varnode(imm: i32) -> Varnode {
    Varnode::constant(imm as u32 as u64, REG_SIZE)
}

/// Resolve a `rel8`/`rel32` displacement to an absolute address: relative
/// branches are always relative to the address of the *next* instruction.
fn resolve_rel(address: u64, length: usize, rel: i32) -> u64 {
    (address as i64 + length as i64 + rel as i64) as u64
}

fn lift_mov_reg_reg(dst: Reg32, src: Reg32) -> Vec<Instruction> {
    vec![Instruction::Copy { dest: reg_varnode(dst), src: reg_varnode(src) }]
}

fn lift_mov_reg_imm(dst: Reg32, imm: i32) -> Vec<Instruction> {
    vec![Instruction::Copy { dest: reg_varnode(dst), src: imm_varnode(imm) }]
}

fn lift_add(dst: Reg32, rhs: Varnode) -> Vec<Instruction> {
    // dst = dst + rhs. (Flag updates from ADD are omitted in Phase 1's
    // simplified flag model -- only CMP's zero flag is tracked, since that's
    // the pair needed to demonstrate Jcc lifting.)
    let dst_vn = reg_varnode(dst);
    vec![Instruction::IntAdd { dest: dst_vn.clone(), lhs: dst_vn, rhs }]
}

fn lift_cmp(lhs: Reg32, rhs: Varnode, temps: &mut TempAllocator) -> Vec<Instruction> {
    // Real hardware computes lhs - rhs once and derives every flag bit from
    // that single result (INT_SUB into a fresh temporary, matching the
    // roadmap's guidance to model flag-setting instructions as several IR
    // ops). ZF is then (result == 0), derived from *that* temporary rather
    // than recomputed independently as "lhs == rhs" -- deriving it from the
    // shared SUB result is what actually makes the pattern extend cleanly
    // once CF/OF/SF are added in a later phase, since they too are just
    // different functions of this same subtraction result.
    let lhs_vn = reg_varnode(lhs);
    let diff = temps.fresh(REG_SIZE);
    vec![
        Instruction::IntSub { dest: diff.clone(), lhs: lhs_vn, rhs },
        Instruction::IntEqual { dest: zf_varnode(), lhs: diff, rhs: Varnode::constant(0, REG_SIZE) },
    ]
}

fn lift_jmp(target: u64) -> Vec<Instruction> {
    vec![Instruction::Branch { target: BranchTarget::Absolute(target) }]
}

fn lift_je(target: u64) -> Vec<Instruction> {
    // JE branches when ZF == 1: branch directly on the zero-flag Varnode.
    vec![Instruction::CBranch { condition: zf_varnode(), target: BranchTarget::Absolute(target) }]
}

fn lift_jne(target: u64, temps: &mut TempAllocator) -> Vec<Instruction> {
    // JNE branches when ZF == 0, so compute `not_zf = (ZF == 0)` into a fresh
    // temporary and branch on that.
    let not_zf = temps.fresh(1);
    vec![
        Instruction::IntEqual {
            dest: not_zf.clone(),
            lhs: zf_varnode(),
            rhs: Varnode::constant(0, 1),
        },
        Instruction::CBranch { condition: not_zf, target: BranchTarget::Absolute(target) },
    ]
}

fn lift_call(target: u64) -> Vec<Instruction> {
    vec![Instruction::Call { target: BranchTarget::Absolute(target) }]
}

fn lift_ret() -> Vec<Instruction> {
    vec![Instruction::Return]
}

/// The x86 [`Lifter`] implementation, dispatching each [`X86Instruction`]
/// variant to its dedicated `lift_*` function above.
#[derive(Debug, Default, Clone, Copy)]
pub struct X86Lifter;

impl X86Lifter {
    pub fn new() -> Self {
        X86Lifter
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ir_pcode::AddressSpace;

    #[test]
    fn resolve_rel_is_relative_to_next_instruction() {
        // jmp at 0x1000, 2 bytes long, rel=+5 -> lands at 0x1000+2+5 = 0x1007
        assert_eq!(resolve_rel(0x1000, 2, 5), 0x1007);
        // negative displacement (backward jump)
        assert_eq!(resolve_rel(0x1000, 2, -2), 0x1000);
    }

    #[test]
    fn lift_mov_reg_imm_produces_single_copy() {
        let ops = lift_mov_reg_imm(Reg32::Eax, 42);
        assert_eq!(ops.len(), 1);
        match &ops[0] {
            Instruction::Copy { dest, src } => {
                assert_eq!(dest.space, AddressSpace::Register);
                assert_eq!(dest.offset, Reg32::Eax.id());
                assert_eq!(src.space, AddressSpace::Constant);
                assert_eq!(src.offset, 42);
            }
            other => panic!("expected Copy, got {other:?}"),
        }
    }

    #[test]
    fn lift_cmp_produces_sub_and_flag_ops_with_distinct_temps() {
        let mut temps = TempAllocator::new();
        let ops = lift_cmp(Reg32::Eax, reg_varnode(Reg32::Ebx), &mut temps);
        assert_eq!(ops.len(), 2);
        assert!(matches!(ops[0], Instruction::IntSub { .. }));
        assert!(matches!(ops[1], Instruction::IntEqual { .. }));
        if let Instruction::IntSub { dest, .. } = &ops[0] {
            assert_eq!(dest.space, AddressSpace::Unique);
        }
        if let Instruction::IntEqual { dest, .. } = &ops[1] {
            assert_eq!(dest.offset, ZF_REG_ID);
        }
    }

    #[test]
    fn lift_cmp_derives_zero_flag_from_the_sub_result() {
        // ZF must be computed from the INT_SUB's own output temp (result ==
        // 0), not recomputed independently from the original operands --
        // otherwise a later pass that reads only the SUB result (e.g. to add
        // CF/OF/SF) would find ZF disconnected from it.
        let mut temps = TempAllocator::new();
        let ops = lift_cmp(Reg32::Eax, reg_varnode(Reg32::Ebx), &mut temps);
        assert_eq!(ops.len(), 2);

        let sub_dest = match &ops[0] {
            Instruction::IntSub { dest, .. } => dest.clone(),
            other => panic!("expected IntSub, got {other:?}"),
        };
        match &ops[1] {
            Instruction::IntEqual { lhs, rhs, .. } => {
                assert_eq!(
                    lhs, &sub_dest,
                    "zero flag must read the SUB result, not the original operands"
                );
                assert_eq!(rhs.space, AddressSpace::Constant);
                assert_eq!(rhs.offset, 0);
            }
            other => panic!("expected IntEqual, got {other:?}"),
        }
    }

    #[test]
    fn lift_jne_branches_on_derived_not_equal_temp() {
        let mut temps = TempAllocator::new();
        let ops = lift_jne(0x2000, &mut temps);
        assert_eq!(ops.len(), 2);
        assert!(matches!(ops[0], Instruction::IntEqual { .. }));
        match &ops[1] {
            Instruction::CBranch { target, .. } => {
                assert_eq!(*target, BranchTarget::Absolute(0x2000));
            }
            other => panic!("expected CBranch, got {other:?}"),
        }
    }

    #[test]
    fn full_pipeline_lifts_via_lifter_trait() {
        let lifter = X86Lifter::new();
        let mut temps = TempAllocator::new();
        let instr = X86Instruction::AddRegReg { dst: Reg32::Eax, src: Reg32::Ecx };
        let ops = lifter.lift(&instr, 0x1000, 2, &mut temps);
        assert_eq!(ops.len(), 1);
        assert!(matches!(ops[0], Instruction::IntAdd { .. }));
    }
}

impl Lifter for X86Lifter {
    // Tie this lifter to the same decoded-instruction type the
    // X86Disassembler produces, so the CLI can pass one's output straight
    // into the other without any adapter code.
    type Instruction = <crate::X86Disassembler as Disassembler>::Instruction;

    fn lift(
        &self,
        instruction: &Self::Instruction,
        address: u64,
        length: usize,
        temps: &mut TempAllocator,
    ) -> Vec<Instruction> {
        match *instruction {
            X86Instruction::MovRegReg { dst, src } => lift_mov_reg_reg(dst, src),
            X86Instruction::MovRegImm { dst, imm } => lift_mov_reg_imm(dst, imm),
            X86Instruction::AddRegReg { dst, src } => lift_add(dst, reg_varnode(src)),
            X86Instruction::AddRegImm { dst, imm } => lift_add(dst, imm_varnode(imm)),
            X86Instruction::CmpRegReg { lhs, rhs } => lift_cmp(lhs, reg_varnode(rhs), temps),
            X86Instruction::CmpRegImm { lhs, imm } => lift_cmp(lhs, imm_varnode(imm), temps),
            X86Instruction::Jmp { rel } => lift_jmp(resolve_rel(address, length, rel)),
            X86Instruction::Je { rel } => lift_je(resolve_rel(address, length, rel)),
            X86Instruction::Jne { rel } => lift_jne(resolve_rel(address, length, rel), temps),
            X86Instruction::Call { rel } => lift_call(resolve_rel(address, length, rel)),
            X86Instruction::Ret => lift_ret(),
        }
    }
}
