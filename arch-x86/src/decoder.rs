//! Decodes raw x86 bytes into [`X86Instruction`], a small architecture-
//! specific enum. This is intentionally *not* a general-purpose x86
//! decoder -- it covers the representative subset the roadmap's Phase 1
//! calls for (MOV, ADD, CMP, JMP, CALL), plus RET and the two conditional
//! jumps (JE/JNE) needed to make a CMP actually useful in a demo.
//!
//! Everything here is private to `arch-x86`; `decompiler-core` only ever
//! sees `X86Instruction` through the `Disassembler` trait's associated
//! type, never these opcode bytes directly.

use crate::registers::{parse_modrm, Reg32};
use decompiler_core::{DecodeError, Disassembler, DisassemblyResult};
use std::fmt;

/// One decoded x86 instruction, in the Phase 1 subset.
///
/// Immediates/displacements are kept as signed values (`i32`) since that's
/// how x86 encodes them (sign-extended `imm8`/`imm32`/`rel8`/`rel32`); the
/// lifter is responsible for turning a `rel` into an absolute target
/// address using the instruction's own address and length.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum X86Instruction {
    MovRegReg { dst: Reg32, src: Reg32 },
    MovRegImm { dst: Reg32, imm: i32 },
    AddRegReg { dst: Reg32, src: Reg32 },
    AddRegImm { dst: Reg32, imm: i32 },
    CmpRegReg { lhs: Reg32, rhs: Reg32 },
    CmpRegImm { lhs: Reg32, imm: i32 },
    Jmp { rel: i32 },
    Je { rel: i32 },
    Jne { rel: i32 },
    Call { rel: i32 },
    Ret,
}

impl fmt::Display for X86Instruction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            X86Instruction::MovRegReg { dst, src } => write!(f, "mov {dst}, {src}"),
            X86Instruction::MovRegImm { dst, imm } => write!(f, "mov {dst}, {imm}"),
            X86Instruction::AddRegReg { dst, src } => write!(f, "add {dst}, {src}"),
            X86Instruction::AddRegImm { dst, imm } => write!(f, "add {dst}, {imm}"),
            X86Instruction::CmpRegReg { lhs, rhs } => write!(f, "cmp {lhs}, {rhs}"),
            X86Instruction::CmpRegImm { lhs, imm } => write!(f, "cmp {lhs}, {imm}"),
            X86Instruction::Jmp { rel } => write!(f, "jmp {rel:+}"),
            X86Instruction::Je { rel } => write!(f, "je {rel:+}"),
            X86Instruction::Jne { rel } => write!(f, "jne {rel:+}"),
            X86Instruction::Call { rel } => write!(f, "call {rel:+}"),
            X86Instruction::Ret => write!(f, "ret"),
        }
    }
}

fn read_u8(bytes: &[u8], i: usize) -> Result<u8, DecodeError> {
    bytes.get(i).copied().ok_or(DecodeError::UnexpectedEnd)
}

fn read_i8(bytes: &[u8], i: usize) -> Result<i32, DecodeError> {
    Ok(read_u8(bytes, i)? as i8 as i32)
}

fn read_i32(bytes: &[u8], i: usize) -> Result<i32, DecodeError> {
    if bytes.len() < i + 4 {
        return Err(DecodeError::UnexpectedEnd);
    }
    Ok(i32::from_le_bytes([bytes[i], bytes[i + 1], bytes[i + 2], bytes[i + 3]]))
}

/// Decode exactly one instruction from the start of `bytes`.
fn decode_one(bytes: &[u8]) -> Result<(X86Instruction, usize), DecodeError> {
    let opcode = read_u8(bytes, 0)?;

    match opcode {
        // MOV r32, imm32  (B8+rd id)
        0xB8..=0xBF => {
            let dst = Reg32::from_bits(opcode - 0xB8);
            let imm = read_i32(bytes, 1)?;
            Ok((X86Instruction::MovRegImm { dst, imm }, 5))
        }

        // MOV r/m32, r32  (89 /r)  -- register-direct only
        0x89 => {
            let modrm = read_u8(bytes, 1)?;
            let m = parse_modrm(modrm).ok_or(DecodeError::UnsupportedAddressingMode { modrm })?;
            // In the 0x89 encoding, `reg` is the source and `rm` is the destination.
            Ok((X86Instruction::MovRegReg { dst: m.rm, src: m.reg }, 2))
        }

        // MOV r32, r/m32  (8B /r)  -- register-direct only
        0x8B => {
            let modrm = read_u8(bytes, 1)?;
            let m = parse_modrm(modrm).ok_or(DecodeError::UnsupportedAddressingMode { modrm })?;
            Ok((X86Instruction::MovRegReg { dst: m.reg, src: m.rm }, 2))
        }

        // ADD r/m32, r32  (01 /r)  -- register-direct only
        0x01 => {
            let modrm = read_u8(bytes, 1)?;
            let m = parse_modrm(modrm).ok_or(DecodeError::UnsupportedAddressingMode { modrm })?;
            Ok((X86Instruction::AddRegReg { dst: m.rm, src: m.reg }, 2))
        }

        // ADD EAX, imm32  (05 id)
        0x05 => {
            let imm = read_i32(bytes, 1)?;
            Ok((X86Instruction::AddRegImm { dst: Reg32::Eax, imm }, 5))
        }

        // CMP r/m32, r32  (39 /r)  -- register-direct only
        0x39 => {
            let modrm = read_u8(bytes, 1)?;
            let m = parse_modrm(modrm).ok_or(DecodeError::UnsupportedAddressingMode { modrm })?;
            Ok((X86Instruction::CmpRegReg { lhs: m.rm, rhs: m.reg }, 2))
        }

        // CMP EAX, imm32  (3D id)
        0x3D => {
            let imm = read_i32(bytes, 1)?;
            Ok((X86Instruction::CmpRegImm { lhs: Reg32::Eax, imm }, 5))
        }

        // Group 1 (81 /r id): only the ADD (/0) and CMP (/7) sub-opcodes are
        // implemented for now; other reg-field values (OR/ADC/SBB/AND/SUB/XOR)
        // are left as future work.
        0x81 => {
            let modrm = read_u8(bytes, 1)?;
            let m = parse_modrm(modrm).ok_or(DecodeError::UnsupportedAddressingMode { modrm })?;
            let reg_field = (modrm >> 3) & 0b111;
            let imm = read_i32(bytes, 2)?;
            match reg_field {
                0 => Ok((X86Instruction::AddRegImm { dst: m.rm, imm }, 6)),
                7 => Ok((X86Instruction::CmpRegImm { lhs: m.rm, imm }, 6)),
                _ => Err(DecodeError::InvalidOrUnsupported { opcode_byte: opcode }),
            }
        }

        // Group 1 (83 /r ib): ADD (/0) and CMP (/7) with a sign-extended
        // imm8. Real compilers prefer this form over 81's imm32 whenever the
        // constant fits in a byte, so it's far more common in practice than
        // 0x81 -- without it, ordinary code like `cmp eax, 1` or
        // `add esp, 4` would fail to decode.
        0x83 => {
            let modrm = read_u8(bytes, 1)?;
            let m = parse_modrm(modrm).ok_or(DecodeError::UnsupportedAddressingMode { modrm })?;
            let reg_field = (modrm >> 3) & 0b111;
            let imm = read_i8(bytes, 2)?;
            match reg_field {
                0 => Ok((X86Instruction::AddRegImm { dst: m.rm, imm }, 3)),
                7 => Ok((X86Instruction::CmpRegImm { lhs: m.rm, imm }, 3)),
                _ => Err(DecodeError::InvalidOrUnsupported { opcode_byte: opcode }),
            }
        }

        // JMP rel8  (EB cb)
        0xEB => {
            let rel = read_i8(bytes, 1)?;
            Ok((X86Instruction::Jmp { rel }, 2))
        }

        // JMP rel32  (E9 cd)
        0xE9 => {
            let rel = read_i32(bytes, 1)?;
            Ok((X86Instruction::Jmp { rel }, 5))
        }

        // JE/JZ rel8  (74 cb)
        0x74 => {
            let rel = read_i8(bytes, 1)?;
            Ok((X86Instruction::Je { rel }, 2))
        }

        // JNE/JNZ rel8  (75 cb)
        0x75 => {
            let rel = read_i8(bytes, 1)?;
            Ok((X86Instruction::Jne { rel }, 2))
        }

        // CALL rel32  (E8 cd)
        0xE8 => {
            let rel = read_i32(bytes, 1)?;
            Ok((X86Instruction::Call { rel }, 5))
        }

        // RET  (C3)
        0xC3 => Ok((X86Instruction::Ret, 1)),

        other => Err(DecodeError::InvalidOrUnsupported { opcode_byte: other }),
    }
}

/// The x86 [`Disassembler`] implementation.
///
/// Holds no state today -- a real x86-64 decoder would track a default
/// operand-size mode here (16/32/64-bit) -- but exists as a concrete type so
/// the `Disassembler` trait has something to be implemented on, matching the
/// architecture the roadmap describes.
#[derive(Debug, Default, Clone, Copy)]
pub struct X86Disassembler;

impl X86Disassembler {
    pub fn new() -> Self {
        X86Disassembler
    }
}

impl Disassembler for X86Disassembler {
    type Instruction = X86Instruction;

    fn disassemble_one(
        &self,
        bytes: &[u8],
        address: u64,
    ) -> Result<DisassemblyResult<Self::Instruction>, DecodeError> {
        let (instruction, length) = decode_one(bytes)?;
        Ok(DisassemblyResult { instruction, length, address })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode(bytes: &[u8]) -> (X86Instruction, usize) {
        decode_one(bytes).expect("decode should succeed")
    }

    #[test]
    fn mov_reg_imm() {
        let (instr, len) = decode(&[0xB8, 0x05, 0x00, 0x00, 0x00]);
        assert_eq!(instr, X86Instruction::MovRegImm { dst: Reg32::Eax, imm: 5 });
        assert_eq!(len, 5);

        // B8+rd covers all 8 registers, e.g. BB -> EBX
        let (instr, _) = decode(&[0xBB, 0x01, 0x00, 0x00, 0x00]);
        assert_eq!(instr, X86Instruction::MovRegImm { dst: Reg32::Ebx, imm: 1 });
    }

    #[test]
    fn mov_reg_reg_directions() {
        // 89 /r: reg is the *source*, rm is the destination -> mov rm, reg
        let (instr, len) = decode(&[0x89, 0xD8]); // modrm 11 011 000 = reg EBX, rm EAX
        assert_eq!(instr, X86Instruction::MovRegReg { dst: Reg32::Eax, src: Reg32::Ebx });
        assert_eq!(len, 2);

        // 8B /r: reg is the destination, rm is the source -> mov reg, rm
        let (instr, _) = decode(&[0x8B, 0xD8]); // same modrm
        assert_eq!(instr, X86Instruction::MovRegReg { dst: Reg32::Ebx, src: Reg32::Eax });
    }

    #[test]
    fn add_forms() {
        let (instr, len) = decode(&[0x05, 0x0A, 0x00, 0x00, 0x00]); // add eax, 10
        assert_eq!(instr, X86Instruction::AddRegImm { dst: Reg32::Eax, imm: 10 });
        assert_eq!(len, 5);

        // 81 /0 id: add r/m32, imm32 (general register, not just eax)
        let (instr, len) = decode(&[0x81, 0xC3, 0x14, 0x00, 0x00, 0x00]); // add ebx, 20
        assert_eq!(instr, X86Instruction::AddRegImm { dst: Reg32::Ebx, imm: 20 });
        assert_eq!(len, 6);

        let (instr, _) = decode(&[0x01, 0xD8]); // add eax, ebx
        assert_eq!(instr, X86Instruction::AddRegReg { dst: Reg32::Eax, src: Reg32::Ebx });
    }

    #[test]
    fn group1_imm8_forms() {
        // 83 /0 ib: add r/m32, imm8 (sign-extended) -- the common
        // small-constant form real compilers actually emit.
        let (instr, len) = decode(&[0x83, 0xC0, 0x05]); // add eax, 5
        assert_eq!(instr, X86Instruction::AddRegImm { dst: Reg32::Eax, imm: 5 });
        assert_eq!(len, 3);

        // 83 /7 ib: cmp r/m32, imm8, with a negative (sign-extended) byte.
        let (instr, len) = decode(&[0x83, 0xF8, 0xFF]); // cmp eax, -1
        assert_eq!(instr, X86Instruction::CmpRegImm { lhs: Reg32::Eax, imm: -1 });
        assert_eq!(len, 3);
    }

    #[test]
    fn cmp_forms() {
        let (instr, len) = decode(&[0x3D, 0x08, 0x00, 0x00, 0x00]); // cmp eax, 8
        assert_eq!(instr, X86Instruction::CmpRegImm { lhs: Reg32::Eax, imm: 8 });
        assert_eq!(len, 5);

        // 81 /7 id: cmp r/m32, imm32 (general register)
        let (instr, len) = decode(&[0x81, 0xFB, 0x03, 0x00, 0x00, 0x00]); // cmp ebx, 3
        assert_eq!(instr, X86Instruction::CmpRegImm { lhs: Reg32::Ebx, imm: 3 });
        assert_eq!(len, 6);

        let (instr, _) = decode(&[0x39, 0xD8]); // cmp eax, ebx
        assert_eq!(instr, X86Instruction::CmpRegReg { lhs: Reg32::Eax, rhs: Reg32::Ebx });
    }

    #[test]
    fn jumps_and_call_and_ret() {
        assert_eq!(decode(&[0xEB, 0x05]).0, X86Instruction::Jmp { rel: 5 });
        assert_eq!(decode(&[0xE9, 0x10, 0x00, 0x00, 0x00]).0, X86Instruction::Jmp { rel: 16 });
        assert_eq!(decode(&[0x74, 0x03]).0, X86Instruction::Je { rel: 3 });
        assert_eq!(decode(&[0x75, 0x03]).0, X86Instruction::Jne { rel: 3 });
        assert_eq!(
            decode(&[0xE8, 0x00, 0x01, 0x00, 0x00]).0,
            X86Instruction::Call { rel: 256 }
        );
        assert_eq!(decode(&[0xC3]).0, X86Instruction::Ret);
    }

    #[test]
    fn negative_rel8_is_sign_extended() {
        // 0xFB as i8 is -5: a short backward jump.
        let (instr, _) = decode(&[0xEB, 0xFB]);
        assert_eq!(instr, X86Instruction::Jmp { rel: -5 });
    }

    #[test]
    fn memory_operand_is_unsupported_not_a_crash() {
        // modrm 00 000 000 = mod=00 (memory, [eax]) -- not yet supported.
        let err = decode_one(&[0x89, 0x00]).unwrap_err();
        assert_eq!(err, DecodeError::UnsupportedAddressingMode { modrm: 0x00 });
    }

    #[test]
    fn truncated_instruction_reports_unexpected_end() {
        let err = decode_one(&[0xB8, 0x01, 0x00]).unwrap_err();
        assert_eq!(err, DecodeError::UnexpectedEnd);
    }

    #[test]
    fn unknown_opcode_is_reported() {
        let err = decode_one(&[0x0F]).unwrap_err();
        assert_eq!(err, DecodeError::InvalidOrUnsupported { opcode_byte: 0x0F });
    }
}
