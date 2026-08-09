// build.rs — runs at COMPILE TIME on the developer's machine.
//
// Generates FLIRT-style byte-pattern signatures for ALL functions found in:
//   1. libc.a        — the full C standard library (both global AND local symbols)
//   2. libgcc.a      — GCC compiler runtime (integer division, float conversion, etc.)
//   3. crt1.o        — C runtime entry point (_start)
//   4. crti.o/crtn.o — .init/.fini section setup and teardown
//   5. crtbeginT.o / crtend.o — GCC C++ constructor/destructor registration
//
// All signatures are serialised into $OUT_DIR/libc_sigs.bin, which is then
// embedded into the binary via include_bytes! in default_sigs.rs. End users
// do NOT need gcc, libc.a, or any toolchain installed — the patterns are
// baked in at compile time and cost nothing at runtime.
//
// A "collision" occurs when two different source symbols produce the same byte
// pattern. Colliding entries are dropped rather than emitting an ambiguous name.
//
// Binary format of libc_sigs.bin:
//   [u32 le] number of signatures
//   for each signature:
//     [u16 le] name_len  •  [u8 × name_len] name (UTF-8, no NUL)
//     [u16 le] pat_len
//     for each pattern byte:
//       0xff followed by the concrete byte value
//       0x00 alone means wildcard

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Command;
use object::{Object, ObjectSection, ObjectSymbol};

fn main() {
    println!("cargo:rerun-if-changed=build.rs");

    let out_dir  = std::env::var("OUT_DIR").expect("OUT_DIR not set");
    let out_path = PathBuf::from(&out_dir).join("libc_sigs.bin");

    let mut all_sigs: Vec<Sig> = Vec::new();

    // ── 1. libc.a — full standard C library ──────────────────────────────────
    if let Some(path) = gcc_find("libc.a") {
        println!("cargo:rerun-if-changed={}", path);
        match sigs_from_archive(&path, true) {
            Ok(s)  => { eprintln!("build.rs: libc.a     → {} sigs", s.len()); all_sigs.extend(s); }
            Err(e) => eprintln!("build.rs: WARNING: libc.a failed: {}", e),
        }
    } else {
        eprintln!("build.rs: WARNING: libc.a not found — skipping");
    }

    // ── 2. libgcc.a — compiler-runtime helpers ───────────────────────────────
    if let Some(path) = gcc_find("libgcc.a") {
        println!("cargo:rerun-if-changed={}", path);
        match sigs_from_archive(&path, true) {
            Ok(s)  => { eprintln!("build.rs: libgcc.a   → {} sigs", s.len()); all_sigs.extend(s); }
            Err(e) => eprintln!("build.rs: WARNING: libgcc.a failed: {}", e),
        }
    }

    // ── 3. CRT object files ───────────────────────────────────────────────────
    for crt in &["crt1.o", "crti.o", "crtn.o", "crtbeginT.o", "crtend.o"] {
        if let Some(path) = gcc_find(crt) {
            println!("cargo:rerun-if-changed={}", path);
            match sigs_from_object_file(&path) {
                Ok(s)  => { eprintln!("build.rs: {:12} → {} sigs", crt, s.len()); all_sigs.extend(s); }
                Err(e) => eprintln!("build.rs: WARNING: {} failed: {}", crt, e),
            }
        }
    }

    // ── Collision removal ─────────────────────────────────────────────────────
    // If two *different* names produce the same first-32-byte pattern the
    // signature is ambiguous and we can't safely rename with it.
    let unique = deduplicate(all_sigs);
    eprintln!("build.rs: total after dedup → {} sigs", unique.len());

    let blob = serialize_sigs(&unique);
    std::fs::write(&out_path, &blob).expect("failed to write libc_sigs.bin");
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Ask gcc where a file lives. Returns None if not found.
fn gcc_find(name: &str) -> Option<String> {
    let out = Command::new("gcc")
        .arg(format!("-print-file-name={}", name))
        .output().ok()?;
    let path = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if path == name || path.is_empty() { return None; }
    if std::fs::metadata(&path).is_ok() { Some(path) } else { None }
}

struct Sig {
    name:    String,
    pattern: Vec<Option<u8>>,
}

/// Generate signatures from every .text symbol in a static archive (.a file).
/// `include_local`: when true, local (static) symbols are also included.
fn sigs_from_archive(path: &str, include_local: bool) -> Result<Vec<Sig>, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("read: {}", e))?;
    let archive = object::read::archive::ArchiveFile::parse(&*bytes)
        .map_err(|e| format!("archive parse: {}", e))?;

    let mut sigs = Vec::new();
    for member in archive.members() {
        let member = match member { Ok(m) => m, Err(_) => continue };
        let mdata  = match member.data(&*bytes) { Ok(b) => b, Err(_) => continue };
        sigs.extend(sigs_from_obj_bytes(mdata, include_local));
    }
    Ok(sigs)
}

/// Generate signatures from a single standalone .o file.
fn sigs_from_object_file(path: &str) -> Result<Vec<Sig>, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("read: {}", e))?;
    Ok(sigs_from_obj_bytes(&bytes, true))
}

/// Core: extract signatures from an in-memory ELF .o.
fn sigs_from_obj_bytes(bytes: &[u8], include_local: bool) -> Vec<Sig> {
    let obj = match object::File::parse(bytes) { Ok(o) => o, Err(_) => return vec![] };
    let text = match obj.section_by_name(".text") { Some(s) => s, None => return vec![] };
    let text_data = match text.data() { Ok(d) => d, Err(_) => return vec![] };

    // Section-relative relocation offsets in .o files
    let mut relocs: Vec<(u64, u64)> = Vec::new();
    for (off, rel) in text.relocations() {
        let sz = (rel.size() / 8).max(1) as u64;
        relocs.push((off, sz));
    }

    let mut sigs = Vec::new();
    for sym in obj.symbols() {
        if sym.section() != object::SymbolSection::Section(text.index()) { continue; }
        if sym.kind()    != object::SymbolKind::Text { continue; }
        if !include_local && !sym.is_global() { continue; }

        let name = match sym.name() { Ok(n) if !n.is_empty() => n, _ => continue };
        // Skip anonymous compiler-generated thunks (names like ".L123")
        if name.starts_with(".L") || name.starts_with("$") { continue; }

        let sym_off  = sym.address() as usize;
        let sym_size = sym.size()    as usize;
        if sym_size == 0 { continue; }

        let sig_len = 32.max(sym_size).min(256);
        let end = (sym_off + sig_len).min(text_data.len());
        if sym_off >= end { continue; }

        let pattern: Vec<Option<u8>> = (sym_off..end).map(|i| {
            let off = i as u64;
            let is_reloc = relocs.iter().any(|&(r, sz)| off >= r && off < r + sz);
            if is_reloc { None } else { Some(text_data[i]) }
        }).collect();

        sigs.push(Sig { name: name.to_string(), pattern });
    }
    sigs
}

/// Remove signatures whose concrete-byte prefix matches more than one name.
fn deduplicate(sigs: Vec<Sig>) -> Vec<Sig> {
    // Key = first 32 concrete bytes (wildcards → 0xff sentinel + 0x01 disambiguator)
    fn key(pat: &[Option<u8>]) -> Vec<u8> {
        pat.iter().take(32).flat_map(|b| match b {
            Some(v) => vec![0x00, *v],
            None    => vec![0x01, 0x00],
        }).collect()
    }

    let mut seen: HashMap<Vec<u8>, Option<String>> = HashMap::new();
    for sig in &sigs {
        let k = key(&sig.pattern);
        seen.entry(k).and_modify(|v| { *v = None; /* collision */ }).or_insert(Some(sig.name.clone()));
    }

    sigs.into_iter().filter(|s| {
        let k = key(&s.pattern);
        seen.get(&k).and_then(|v| v.as_ref()).map(|n| n == &s.name).unwrap_or(false)
    }).collect()
}

fn serialize_sigs(sigs: &[Sig]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&(sigs.len() as u32).to_le_bytes());
    for sig in sigs {
        let nb = sig.name.as_bytes();
        out.extend_from_slice(&(nb.len()           as u16).to_le_bytes());
        out.extend_from_slice(nb);
        out.extend_from_slice(&(sig.pattern.len()  as u16).to_le_bytes());
        for p in &sig.pattern {
            match p {
                Some(b) => { out.push(0xff); out.push(*b); }
                None    =>   out.push(0x00),
            }
        }
    }
    out
}
