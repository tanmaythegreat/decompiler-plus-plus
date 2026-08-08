pub mod decoder;
pub mod lifter;
pub mod registers;

pub use decoder::{ArmDisassembler, ArmInstruction};
pub use lifter::ArmLifter;
pub use registers::Reg64;

#[cfg(test)]
mod tests {
    use super::*;
    use decompiler_core::{Disassembler, Lifter};
    use ir_pcode::Instruction as IrInstruction;

    #[test]
    fn test_arm_decode_and_lift() {
        let disasm = ArmDisassembler::new();
        let lifter = ArmLifter::new();

        // Decode MOV X0, #5
        // word layout: [opcode=0x10][imm16=5 in bits 20:5][dst=0 in bits 4:0]
        // word = (0x10 << 24) | (5 << 5) | 0 = 0x100000A0
        let word: u32 = (0x10u32 << 24) | (5 << 5) | 0;
        let bytes = word.to_le_bytes();
        
        let decoded = disasm.disassemble_one(&bytes, 0x1000).unwrap();
        assert_eq!(decoded.instruction, ArmInstruction::MovRegImm { dst: Reg64::X0, imm: 5 });

        let mut temps = ir_pcode::TempAllocator::new();
        let ops = lifter.lift(&decoded.instruction, decoded.address, decoded.length, &mut temps);
        assert_eq!(ops.len(), 1);
        
        match &ops[0] {
            IrInstruction::Copy { dest, src } => {
                assert_eq!(dest.name.as_deref(), Some("X0"));
                assert_eq!(src.offset, 5);
            },
            _ => panic!("Expected Copy instruction"),
        }
    }
}
