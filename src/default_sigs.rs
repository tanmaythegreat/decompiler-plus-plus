// Embedded Windows MSVC signatures (from maktm/flirtdb)
pub const LIBCMT_MSVC_X64: &[u8] = include_bytes!("default_sigs/libcmt_15_msvc_x64.sig");
pub const LIBVCRUNTIME_MSVC_X64: &[u8] = include_bytes!("default_sigs/libvcruntime_15_msvc_x64.sig");

// Embedded Linux libc signatures — generated from system libc.a AT COMPILE TIME by build.rs.
// Zero runtime cost: no file I/O, no gcc subprocess, no libc.a parsing at startup.
static LIBC_SIGS_BIN: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/libc_sigs.bin"));

/// A precomputed signature pattern: None entries are wildcards.
#[derive(Clone)]
pub struct EmbeddedSig {
    pub name: &'static str,
    pub pattern: Vec<Option<u8>>,
}

impl EmbeddedSig {
    pub fn matches(&self, buf: &[u8]) -> bool {
        if buf.len() < self.pattern.len() {
            return false;
        }
        for (b, p) in buf.iter().zip(self.pattern.iter()) {
            if let Some(pv) = p {
                if *b != *pv {
                    return false;
                }
            }
        }
        true
    }
}

/// Deserialize the compile-time-generated libc signatures from the embedded blob.
/// The blob format written by build.rs:
///   [u32 le] count
///   for each sig:
///     [u16 le] name_len, [u8 * name_len] name (UTF-8)
///     [u16 le] pattern_len
///     for each byte: 0xff => concrete (next byte is value), 0x00 => wildcard
pub fn load_libc_sigs() -> Vec<EmbeddedSig> {
    let data = LIBC_SIGS_BIN;
    if data.len() < 4 {
        return Vec::new();
    }
    let count = u32::from_le_bytes([data[0], data[1], data[2], data[3]]) as usize;
    let mut sigs = Vec::with_capacity(count);
    let mut pos = 4usize;

    // We need owned strings to hand out &'static str — use Box::leak for this.
    // Since we call this once at startup and keep them for the program lifetime, this is fine.
    for _ in 0..count {
        if pos + 2 > data.len() { break; }
        let name_len = u16::from_le_bytes([data[pos], data[pos + 1]]) as usize;
        pos += 2;
        if pos + name_len > data.len() { break; }
        let name: &'static str = Box::leak(
            String::from_utf8_lossy(&data[pos..pos + name_len]).into_owned().into_boxed_str()
        );
        pos += name_len;

        if pos + 2 > data.len() { break; }
        let pat_len = u16::from_le_bytes([data[pos], data[pos + 1]]) as usize;
        pos += 2;

        let mut pattern = Vec::with_capacity(pat_len);
        for _ in 0..pat_len {
            if pos >= data.len() { break; }
            let tag = data[pos]; pos += 1;
            if tag == 0xff {
                if pos >= data.len() { break; }
                pattern.push(Some(data[pos])); pos += 1;
            } else {
                pattern.push(None);
            }
        }

        sigs.push(EmbeddedSig { name, pattern });
    }

    sigs
}
