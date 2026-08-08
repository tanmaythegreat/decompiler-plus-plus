//! `Varnode` is the atomic unit of data in the IR: a generalized "value location".
//!
//! Following Ghidra's P-code model, a Varnode does not care *what kind* of
//! storage it names -- a CPU register, a byte range in RAM, an immediate
//! constant, or a compiler-generated temporary all look the same to the rest
//! of the pipeline: an `(address_space, offset, size)` triple. This is what
//! lets every later analysis pass (CFG building, dataflow, type inference)
//! stay architecture-agnostic.

use std::fmt;

/// The class of storage a [`Varnode`] lives in.
///
/// This is intentionally architecture-agnostic. An `arch-*` crate decides
/// what `offset` means within `Register` space (e.g. which integer identifies
/// `EAX` vs `RSP`); `decompiler-core` and later analysis passes never need to
/// know that mapping.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum AddressSpace {
    /// A CPU register. `offset` is an arch-defined register id.
    Register,
    /// Main memory / RAM. `offset` is a byte address.
    Memory,
    /// An immediate / literal value embedded in an instruction. `offset`
    /// holds the constant's bit pattern (sign/zero-extended as needed).
    Constant,
    /// A compiler/lifter-generated temporary with no real storage, used to
    /// hold intermediate results when a single machine instruction is
    /// decomposed into several IR operations (e.g. flag computations).
    Unique,
}

impl fmt::Display for AddressSpace {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            AddressSpace::Register => "register",
            AddressSpace::Memory => "ram",
            AddressSpace::Constant => "const",
            AddressSpace::Unique => "unique",
        };
        write!(f, "{s}")
    }
}

/// A single generalized value location: *n* bytes at some offset within some
/// address space, with an optional human-readable name for pretty-printing
/// (e.g. `"EAX"`) that carries no semantic weight -- two Varnodes are equal
/// iff their `(space, offset, size)` triples match, regardless of `name`.
#[derive(Debug, Clone)]
pub struct Varnode {
    pub space: AddressSpace,
    pub offset: u64,
    /// Size in bytes.
    pub size: u8,
    /// Optional debug name (e.g. register mnemonic). Not used for equality.
    pub name: Option<String>,
}

impl Varnode {
    pub fn new(space: AddressSpace, offset: u64, size: u8) -> Self {
        Varnode { space, offset, size, name: None }
    }

    pub fn named(space: AddressSpace, offset: u64, size: u8, name: impl Into<String>) -> Self {
        Varnode { space, offset, size, name: Some(name.into()) }
    }

    /// Convenience constructor for an immediate/constant Varnode.
    pub fn constant(value: u64, size: u8) -> Self {
        Varnode::new(AddressSpace::Constant, value, size)
    }

    /// Convenience constructor for a register Varnode with a display name.
    pub fn register(id: u64, size: u8, name: impl Into<String>) -> Self {
        Varnode::named(AddressSpace::Register, id, size, name)
    }
}

impl PartialEq for Varnode {
    fn eq(&self, other: &Self) -> bool {
        self.space == other.space && self.offset == other.offset && self.size == other.size
    }
}
impl Eq for Varnode {}

impl PartialOrd for Varnode {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Varnode {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.space.cmp(&other.space)
            .then(self.offset.cmp(&other.offset))
            .then(self.size.cmp(&other.size))
    }
}

// Manual `Hash` impl (rather than `#[derive]`) is required here for the same
// reason `PartialEq` is manual: hashing must agree with equality, which
// ignores `name`. This is what lets Phase 2's dataflow analyses use
// `Varnode` directly as a `HashMap`/`HashSet` key (e.g. "what's the current
// lattice value of this Varnode?") without a wrapper type.
impl std::hash::Hash for Varnode {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.space.hash(state);
        self.offset.hash(state);
        self.size.hash(state);
    }
}

impl fmt::Display for Varnode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(name) = &self.name {
            write!(f, "{name}:{}", self.size)
        } else {
            match self.space {
                AddressSpace::Constant => write!(f, "0x{:x}:{}", self.offset, self.size),
                _ => write!(f, "{}[0x{:x}]:{}", self.space, self.offset, self.size),
            }
        }
    }
}

/// A monotonically increasing source of fresh `Unique`-space Varnodes, used
/// by lifters whenever a machine instruction's semantics require a
/// scratch/intermediate value that has no architectural storage of its own.
///
/// # Contract
/// Each `TempAllocator` instance is **scoped to exactly one function**. Two
/// `Unique`-space Varnodes with the same `offset` produced by *different*
/// allocators would compare as equal (same `(space, offset, size)` triple)
/// even though they refer to different scratch values. Callers must create
/// one fresh allocator per function and not share allocators across functions.
#[derive(Debug, Default)]
pub struct TempAllocator {
    next: u64,
}

impl TempAllocator {
    pub fn new() -> Self {
        TempAllocator { next: 0 }
    }

    pub fn fresh(&mut self, size: u8) -> Varnode {
        let v = Varnode::new(AddressSpace::Unique, self.next, size);
        self.next += 1;
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn equality_ignores_debug_name() {
        let a = Varnode::register(0, 4, "EAX");
        let b = Varnode::new(AddressSpace::Register, 0, 4);
        assert_eq!(a, b, "Varnodes with the same (space, offset, size) must be equal regardless of name");
    }

    #[test]
    fn different_offsets_are_not_equal() {
        let eax = Varnode::register(0, 4, "EAX");
        let ecx = Varnode::register(1, 4, "ECX");
        assert_ne!(eax, ecx);
    }

    #[test]
    fn hash_ignores_debug_name_like_eq_does() {
        use std::collections::HashSet;
        let a = Varnode::register(0, 4, "EAX");
        let b = Varnode::new(AddressSpace::Register, 0, 4);
        let mut set = HashSet::new();
        set.insert(a);
        assert!(set.contains(&b), "equal Varnodes must hash the same regardless of name");
    }

    #[test]
    fn temp_allocator_never_repeats() {
        let mut temps = TempAllocator::new();
        let a = temps.fresh(4);
        let b = temps.fresh(4);
        assert_ne!(a, b);
        assert_eq!(a.space, AddressSpace::Unique);
    }
}
