//! Mock ARM disassembler for Phase 5 multi-ISA extensibility testing.
//!
//! **This is NOT a real AArch64 decoder.** It uses a custom simplified
//! encoding invented solely to exercise the decompiler's multi-ISA pipeline
//! (`Disassembler`/`Lifter` traits, CFG construction, type inference, and
//! C output) without requiring a full AArch64 decode implementation.
//!
//! The encoding scheme (opcode in top 8 bits of a 4-byte LE word):
//!
//! | Top byte | Instruction            |
//! |----------|------------------------|
//! | `0x10`   | MOV Xdst, #imm16       |
//! | `0x11`   | ADD Xdst, Xsrc1, Xsrc2 |
//! | `0x12`   | ADD Xdst, Xsrc, #imm   |
//! | `0x13`   | CMP Xlhs, Xrhs         |
//! | `0x14`   | B   rel24              |
//! | `0x15`   | BL  rel24              |
//! | `0x16`   | B.EQ rel24             |
//! | `0x17`   | B.NE rel24             |
//! | `0x18`   | RET                    |
//!
//! A production `arch-arm` crate would decode real AArch64 fixed-width
//! 32-bit instruction words per the ARM Architecture Reference Manual.

use crate::registers::Reg64;
use decompiler_core::{DecodeError, Disassembler, DisassemblyResult};
use std::fmt;

/// A decoded mock-ARM instruction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArmInstruction {
    MovRegImm { dst: Reg64, imm: u32 },
    AddRegReg { dst: Reg64, src1: Reg64, src2: Reg64 },
    AddRegImm { dst: Reg64, src: Reg64, imm: u32 },
    CmpRegReg { lhs: Reg64, rhs: Reg64 },
    CmpRegImm { lhs: Reg64, imm: u32 },
    B { rel: i32 },
    BL { rel: i32 },
    BEQ { rel: i32 },
    BNE { rel: i32 },
    Ret,
}

impl fmt::Display for ArmInstruction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ArmInstruction::MovRegImm { dst, imm } => write!(f, "MOV {}, #{}", dst, imm),
            ArmInstruction::AddRegReg { dst, src1, src2 } => write!(f, "ADD {}, {}, {}", dst, src1, src2),
            ArmInstruction::AddRegImm { dst, src, imm } => write!(f, "ADD {}, {}, #{}", dst, src, imm),
            ArmInstruction::CmpRegReg { lhs, rhs } => write!(f, "CMP {}, {}", lhs, rhs),
            ArmInstruction::CmpRegImm { lhs, imm } => write!(f, "CMP {}, #{}", lhs, imm),
            ArmInstruction::B { rel } => write!(f, "B {rel:+}"),
            ArmInstruction::BL { rel } => write!(f, "BL {rel:+}"),
            ArmInstruction::BEQ { rel } => write!(f, "B.EQ {rel:+}"),
            ArmInstruction::BNE { rel } => write!(f, "B.NE {rel:+}"),
            ArmInstruction::Ret => write!(f, "RET"),
        }
    }
}

/// Decode a sign-extended 24-bit immediate from the lower 24 bits of `word`.
fn sign_extend_24(word: u32) -> i32 {
    let imm24 = word & 0xFF_FFFF;
    if (imm24 & 0x80_0000) != 0 {
        (imm24 | 0xFF00_0000) as i32
    } else {
        imm24 as i32
    }
}

/// Decode a register field, mapping decoder errors to `DecodeError`.
fn decode_reg(bits: u8) -> Result<Reg64, DecodeError> {
    Reg64::from_bits(bits).map_err(|_| DecodeError::InvalidOrUnsupported { opcode_byte: bits })
}

pub fn decode_one(bytes: &[u8]) -> Result<(ArmInstruction, usize), DecodeError> {
    if bytes.len() < 4 {
        return Err(DecodeError::UnexpectedEnd);
    }
    let word = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
    let opcode = (word >> 24) as u8;

    match opcode {
        0x10 => {
            let dst  = decode_reg((word & 0x1F) as u8)?;
            let imm  = (word >> 5) & 0xFFFF;
            Ok((ArmInstruction::MovRegImm { dst, imm }, 4))
        }
        0x11 => {
            let dst  = decode_reg((word & 0x1F) as u8)?;
            let src1 = decode_reg(((word >> 5)  & 0x1F) as u8)?;
            let src2 = decode_reg(((word >> 10) & 0x1F) as u8)?;
            Ok((ArmInstruction::AddRegReg { dst, src1, src2 }, 4))
        }
        0x12 => {
            let dst  = decode_reg((word & 0x1F) as u8)?;
            let src  = decode_reg(((word >> 5)  & 0x1F) as u8)?;
            let imm  = (word >> 10) & 0x3FFF;
            Ok((ArmInstruction::AddRegImm { dst, src, imm }, 4))
        }
        0x13 => {
            let lhs  = decode_reg((word & 0x1F) as u8)?;
            let rhs  = decode_reg(((word >> 5)  & 0x1F) as u8)?;
            Ok((ArmInstruction::CmpRegReg { lhs, rhs }, 4))
        }
        0x14 => Ok((ArmInstruction::B   { rel: sign_extend_24(word) }, 4)),
        0x15 => Ok((ArmInstruction::BL  { rel: sign_extend_24(word) }, 4)),
        0x16 => Ok((ArmInstruction::BEQ { rel: sign_extend_24(word) }, 4)),
        0x17 => Ok((ArmInstruction::BNE { rel: sign_extend_24(word) }, 4)),
        0x18 => Ok((ArmInstruction::Ret, 4)),
        _    => Err(DecodeError::InvalidOrUnsupported { opcode_byte: opcode }),
    }
}

/// The mock ARM [`Disassembler`] implementation.
#[derive(Debug, Default, Clone, Copy)]
pub struct ArmDisassembler;

impl ArmDisassembler {
    pub fn new() -> Self { ArmDisassembler }
}

impl Disassembler for ArmDisassembler {
    type Instruction = ArmInstruction;

    fn disassemble_one(
        &self,
        bytes: &[u8],
        address: u64,
    ) -> Result<DisassemblyResult<Self::Instruction>, DecodeError> {
        let (instruction, length) = decode_one(bytes)?;
        Ok(DisassemblyResult { instruction, length, address })
    }
}
