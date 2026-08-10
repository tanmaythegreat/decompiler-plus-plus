// Embedded Windows MSVC signatures (from maktm/flirtdb)
pub const LIBCMT_MSVC_X64: &[u8] = include_bytes!("default_sigs/libcmt_15_msvc_x64.sig");
pub const LIBVCRUNTIME_MSVC_X64: &[u8] = include_bytes!("default_sigs/libvcruntime_15_msvc_x64.sig");

// Embedded Linux libc/libgcc/CRT signatures — generated AT COMPILE TIME by build.rs.
// Covers every global AND local symbol in libc.a, libgcc.a, and all CRT .o files.
// Zero runtime cost: no file I/O, no gcc subprocess, no archive parsing at startup.
static LIBC_SIGS_BIN: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/libc_sigs.bin"));

/// Deserialize the compile-time-generated signatures from the embedded blob and return
/// them as `CustomSig` values that `apply_custom_sigs` can directly consume.
///
/// Blob format written by build.rs:
///   [u32 le] count
///   for each sig:
///     [u16 le] name_len  •  [u8 × name_len] name (UTF-8)
///     [u16 le] pattern_len
///     per byte: 0xff → concrete (followed by value byte),  0x00 → wildcard
pub fn load_libc_sigs() -> Vec<crate::flirt::CustomSig> {
    let data = LIBC_SIGS_BIN;
    if data.len() < 4 {
        return Vec::new();
    }
    let count = u32::from_le_bytes([data[0], data[1], data[2], data[3]]) as usize;
    let mut sigs = Vec::with_capacity(count);
    let mut pos = 4usize;

    for _ in 0..count {
        if pos + 2 > data.len() { break; }
        let name_len = u16::from_le_bytes([data[pos], data[pos + 1]]) as usize;
        pos += 2;
        if pos + name_len > data.len() { break; }
        let name = String::from_utf8_lossy(&data[pos..pos + name_len]).into_owned();
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

        sigs.push(crate::flirt::CustomSig { name, pattern });
    }

    sigs
}
