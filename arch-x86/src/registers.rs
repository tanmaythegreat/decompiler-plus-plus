//! The 8 legacy 32-bit general-purpose registers and the register-id
//! encoding shared by ModRM `reg`/`rm` fields and the `B8+rd` opcode form.
//!
//! Phase 1 targets 32-bit operand size only (no REX prefixes, no 64-bit
//! extended registers) to keep the decoder small while still covering
//! everything needed to lift MOV/ADD/CMP/JMP/CALL. Widening to x86-64 and a
//! real ModRM/SIB/displacement addressing-mode decoder is future work noted
//! in the roadmap's later phases.

use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reg32 {
    Eax,
    Ecx,
    Edx,
    Ebx,
    Esp,
    Ebp,
    Esi,
    Edi,
}

impl Reg32 {
    /// Decode the 3-bit register field used by both ModRM sub-fields and the
    /// `B8+rd` opcode encoding.
    pub fn from_bits(bits: u8) -> Reg32 {
        match bits & 0b111 {
            0 => Reg32::Eax,
            1 => Reg32::Ecx,
            2 => Reg32::Edx,
            3 => Reg32::Ebx,
            4 => Reg32::Esp,
            5 => Reg32::Ebp,
            6 => Reg32::Esi,
            _ => Reg32::Edi,
        }
    }

    /// The register id used as a Varnode offset within `AddressSpace::Register`.
    /// This matches the ModRM encoding directly (0..=7); an arch-x86-internal
    /// detail that the rest of the pipeline never needs to interpret.
    pub fn id(self) -> u64 {
        match self {
            Reg32::Eax => 0,
            Reg32::Ecx => 1,
            Reg32::Edx => 2,
            Reg32::Ebx => 3,
            Reg32::Esp => 4,
            Reg32::Ebp => 5,
            Reg32::Esi => 6,
            Reg32::Edi => 7,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Reg32::Eax => "EAX",
            Reg32::Ecx => "ECX",
            Reg32::Edx => "EDX",
            Reg32::Ebx => "EBX",
            Reg32::Esp => "ESP",
            Reg32::Ebp => "EBP",
            Reg32::Esi => "ESI",
            Reg32::Edi => "EDI",
        }
    }
}

impl fmt::Display for Reg32 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.name())
    }
}

/// A decoded ModRM byte, restricted (for now) to the register-direct
/// addressing mode (`mod == 0b11`), which is all Phase 1's instruction
/// subset needs.
pub struct ModRm {
    pub reg: Reg32,
    pub rm: Reg32,
}

/// Parse a ModRM byte, requiring register-direct addressing mode.
///
/// Returns `None` if `mod != 0b11` (i.e. a memory operand is being
/// addressed) -- support for memory addressing modes (SIB bytes,
/// displacements) is left for a later pass, so callers should treat `None`
/// as "unsupported addressing mode" rather than "invalid byte".
pub fn parse_modrm(byte: u8) -> Option<ModRm> {
    let md = (byte >> 6) & 0b11;
    let reg = (byte >> 3) & 0b111;
    let rm = byte & 0b111;
    if md != 0b11 {
        return None;
    }
    Some(ModRm { reg: Reg32::from_bits(reg), rm: Reg32::from_bits(rm) })
}
