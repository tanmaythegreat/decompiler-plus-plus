use std::fmt;

/// AArch64 general-purpose registers X0–X31.
///
/// `#[repr(u8)]` is required so that `*self as u8` in the `Display` impl is
/// well-defined. Without it the compiler assigns arbitrary discriminants and
/// the cast would be undefined behaviour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Reg64 {
    X0 = 0, X1, X2, X3, X4, X5, X6, X7,
    X8, X9, X10, X11, X12, X13, X14, X15,
    X16, X17, X18, X19, X20, X21, X22, X23,
    X24, X25, X26, X27, X28, X29, X30, X31,
}

impl Reg64 {
    /// Decode a 5-bit register field from an AArch64 instruction word.
    ///
    /// Returns `Err(bits)` for any value outside 0–31 so the caller
    /// (the decoder) can emit a proper [`DecodeError`] instead of panicking.
    pub fn from_bits(bits: u8) -> Result<Self, u8> {
        match bits {
            0  => Ok(Reg64::X0),  1  => Ok(Reg64::X1),
            2  => Ok(Reg64::X2),  3  => Ok(Reg64::X3),
            4  => Ok(Reg64::X4),  5  => Ok(Reg64::X5),
            6  => Ok(Reg64::X6),  7  => Ok(Reg64::X7),
            8  => Ok(Reg64::X8),  9  => Ok(Reg64::X9),
            10 => Ok(Reg64::X10), 11 => Ok(Reg64::X11),
            12 => Ok(Reg64::X12), 13 => Ok(Reg64::X13),
            14 => Ok(Reg64::X14), 15 => Ok(Reg64::X15),
            16 => Ok(Reg64::X16), 17 => Ok(Reg64::X17),
            18 => Ok(Reg64::X18), 19 => Ok(Reg64::X19),
            20 => Ok(Reg64::X20), 21 => Ok(Reg64::X21),
            22 => Ok(Reg64::X22), 23 => Ok(Reg64::X23),
            24 => Ok(Reg64::X24), 25 => Ok(Reg64::X25),
            26 => Ok(Reg64::X26), 27 => Ok(Reg64::X27),
            28 => Ok(Reg64::X28), 29 => Ok(Reg64::X29),
            30 => Ok(Reg64::X30), 31 => Ok(Reg64::X31),
            n  => Err(n),
        }
    }
}

impl fmt::Display for Reg64 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Reg64::X30 => write!(f, "LR"),
            Reg64::X31 => write!(f, "SP"), // or XZR depending on context
            // SAFE: `#[repr(u8)]` guarantees the discriminant equals the
            // declared integer value, so `*self as u8` is well-defined.
            _ => write!(f, "X{}", *self as u8),
        }
    }
}
