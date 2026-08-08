use crate::decoder::ArmInstruction;
use crate::registers::Reg64;
use decompiler_core::{AddressSpace, BranchTarget, Lifter, Varnode};
use ir_pcode::{Instruction as IrInstruction, TempAllocator};

pub struct ArmLifter;

impl ArmLifter {
    pub fn new() -> Self {
        ArmLifter
    }
}

fn reg(r: Reg64) -> Varnode {
    Varnode::named(AddressSpace::Register, r as u64, 8, &format!("{}", r))
}

fn imm(val: u32) -> Varnode {
    Varnode::constant(val as u64, 8)
}

fn flags() -> Varnode {
    Varnode::named(AddressSpace::Register, 1000, 4, "NZCV")
}

impl Lifter for ArmLifter {
    type Instruction = ArmInstruction;

    fn lift(
        &self,
        instruction: &Self::Instruction,
        address: u64,
        _length: usize,
        _temps: &mut TempAllocator,
    ) -> Vec<IrInstruction> {
        let mut ops = Vec::new();
        
        match instruction {
            ArmInstruction::MovRegImm { dst, imm: val } => {
                ops.push(IrInstruction::Copy { dest: reg(*dst), src: imm(*val) });
            }
            ArmInstruction::AddRegReg { dst, src1, src2 } => {
                ops.push(IrInstruction::IntAdd { dest: reg(*dst), lhs: reg(*src1), rhs: reg(*src2) });
            }
            ArmInstruction::AddRegImm { dst, src, imm: val } => {
                ops.push(IrInstruction::IntAdd { dest: reg(*dst), lhs: reg(*src), rhs: imm(*val) });
            }
            ArmInstruction::CmpRegReg { lhs, rhs } => {
                ops.push(IrInstruction::IntEqual { dest: flags(), lhs: reg(*lhs), rhs: reg(*rhs) });
            }
            ArmInstruction::CmpRegImm { lhs, imm: val } => {
                ops.push(IrInstruction::IntEqual { dest: flags(), lhs: reg(*lhs), rhs: imm(*val) });
            }
            ArmInstruction::B { rel } => {
                let target = BranchTarget::Absolute((address as i64 + *rel as i64) as u64);
                ops.push(IrInstruction::Branch { target });
            }
            ArmInstruction::BL { rel } => {
                let target = BranchTarget::Absolute((address as i64 + *rel as i64) as u64);
                ops.push(IrInstruction::Call { target });
            }
            ArmInstruction::BEQ { rel } => {
                let target = BranchTarget::Absolute((address as i64 + *rel as i64) as u64);
                ops.push(IrInstruction::CBranch { condition: flags(), target });
            }
            ArmInstruction::BNE { rel } => {
                let target = BranchTarget::Absolute((address as i64 + *rel as i64) as u64);
                ops.push(IrInstruction::CBranch { condition: flags(), target });
            }
            ArmInstruction::Ret => {
                ops.push(IrInstruction::Return);
            }
        }

        ops
    }
}
