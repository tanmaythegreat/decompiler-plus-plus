use crate::analysis::{slice_of, FuncRegion};
use lancelot_flirt::FlirtSignatureSet;
use object::{Object, ObjectSection, ObjectSymbol};

#[derive(Clone)]
pub struct CustomSig {
    pub name: String,
    pub pattern: Vec<Option<u8>>,
}

impl CustomSig {
    pub fn matches(&self, buf: &[u8]) -> bool {
        if buf.len() < self.pattern.len() { return false; }
        for (b, p) in buf.iter().zip(self.pattern.iter()) {
            if let Some(pv) = p {
                if *b != *pv { return false; }
            }
        }
        true
    }
}

pub fn apply_custom_sigs(funcs: &mut [FuncRegion], data: &[u8], base: u64, sigs: &[CustomSig]) -> usize {
    let mut matched_count = 0;
    for f in funcs.iter_mut() {
        if f.name.starts_with("sub_") || f.name == "?" {
            if let Some(buf) = slice_of(data, base, f) {
                for sig in sigs {
                    if sig.matches(buf) {
                        f.name = sig.name.clone();
                        f.is_lib = true;
                        matched_count += 1;
                        break;
                    }
                }
            }
        }
    }
    matched_count
}

/// Generate FLIRT-style signatures for every global function found in a static archive (.a file).
/// This works by directly parsing each .o member in the archive, reading its .text section and
/// relocation table. Relocatable bytes are wildcarded so the pattern matches across different
/// link-time layouts.
pub fn generate_sigs_from_archive(archive_path: &str) -> Result<Vec<CustomSig>, String> {
    let bytes = std::fs::read(archive_path)
        .map_err(|e| format!("failed to read archive {}: {}", archive_path, e))?;

    let archive = object::read::archive::ArchiveFile::parse(&*bytes)
        .map_err(|e| format!("failed to parse archive: {}", e))?;

    let mut sigs = Vec::new();

    for member in archive.members() {
        let member = member.map_err(|e| format!("bad archive member: {}", e))?;
        let member_bytes = match member.data(&*bytes) {
            Ok(b) => b,
            Err(_) => continue,
        };

        let obj = match object::File::parse(member_bytes) {
            Ok(o) => o,
            Err(_) => continue,
        };

        let text = match obj.section_by_name(".text") {
            Some(s) => s,
            None => continue,
        };

        let text_data = match text.data() {
            Ok(d) => d,
            Err(_) => continue,
        };

        // Collect relocations in the .text section.
        // In a .o file, relocation offsets are SECTION-RELATIVE (not absolute).
        let mut relocs: Vec<(u64, u64)> = Vec::new(); // (section_offset, byte_size)
        for (offset, reloc) in text.relocations() {
            let byte_size = (reloc.size() / 8).max(1) as u64;
            relocs.push((offset, byte_size));
        }

        for sym in obj.symbols() {
            if sym.section() != object::SymbolSection::Section(text.index()) { continue; }
            if sym.kind() != object::SymbolKind::Text { continue; }
            if !sym.is_global() { continue; }
            let name = match sym.name() {
                Ok(n) if !n.is_empty() => n,
                _ => continue,
            };
            let sym_offset = sym.address() as usize; // section-relative offset
            let sym_size = sym.size() as usize;
            if sym_size == 0 { continue; }

            let sig_len = 32.max(sym_size).min(256); // use at least 32 bytes, up to 256
            let end = (sym_offset + sig_len).min(text_data.len());
            if sym_offset >= end { continue; }

            let mut pattern = Vec::with_capacity(end - sym_offset);
            for i in sym_offset..end {
                let off = i as u64;
                // Check if this byte falls inside a relocation field
                let is_reloc = relocs.iter().any(|&(r_off, r_size)| {
                    off >= r_off && off < r_off + r_size
                });
                if is_reloc {
                    pattern.push(None); // wildcard
                } else {
                    pattern.push(Some(text_data[i]));
                }
            }

            sigs.push(CustomSig { name: name.to_string(), pattern });
        }
    }

    Ok(sigs)
}

/// Find the system's static libc.a by asking gcc, then generate signatures for all functions.
pub fn auto_generate_libc_signatures() -> Result<Vec<CustomSig>, String> {
    let output = std::process::Command::new("gcc")
        .args(&["-print-file-name=libc.a"])
        .output()
        .map_err(|e| format!("failed to run gcc: {}", e))?;

    let libc_path = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if libc_path == "libc.a" || libc_path.is_empty() {
        return Err("gcc could not find libc.a on this system".to_string());
    }

    generate_sigs_from_archive(&libc_path)
}

pub fn match_signatures(funcs: &mut [FuncRegion], data: &[u8], base: u64, sig_path: &str) {
    let sig_data = match std::fs::read(sig_path) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("failed to read flirt sig file {}: {}", sig_path, e);
            return;
        }
    };

    let is_pat = sig_path.ends_with(".pat");
    match_signature_bytes(funcs, data, base, &sig_data, is_pat, sig_path);
}

pub fn match_signature_bytes(
    funcs: &mut [FuncRegion],
    data: &[u8],
    base: u64,
    sig_data: &[u8],
    is_pat: bool,
    sig_name_for_log: &str,
) {
    let sigs = if is_pat {
        let pat_str = String::from_utf8_lossy(sig_data);
        match lancelot_flirt::pat::parse(&pat_str) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("failed to parse flirt .pat: {:?}", e);
                return;
            }
        }
    } else {
        match lancelot_flirt::sig::parse(&sig_data) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("failed to parse flirt .sig: {:?}", e);
                return;
            }
        }
    };

    let sigset = FlirtSignatureSet::with_signatures(sigs);
    let mut matched_count = 0;

    for f in funcs.iter_mut() {
        if f.name.starts_with("sub_") || f.name == "?" {
            if let Some(buf) = slice_of(data, base, f) {
                let matches = sigset.r#match(buf);
                if let Some(m) = matches.first() {
                    if let Some(name) = m.get_name() {
                        f.name = name.to_string();
                        f.is_lib = true;
                        matched_count += 1;
                    }
                }
            }
        }
    }

    if matched_count > 0 {
        println!("FLIRT: matched {} functions using {}", matched_count, sig_name_for_log);
    }
}
