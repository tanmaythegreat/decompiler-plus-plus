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
                        matched_count += 1;
                        break;
                    }
                }
            }
        }
    }
    matched_count
}

pub fn auto_generate_libc_signatures() -> Result<Vec<CustomSig>, String> {
    let targets = vec![
        "printf", "malloc", "free", "strlen", "strcmp", "strcpy", "memcpy", "memset",
        "puts", "fopen", "fclose", "fputs", "fgetc", "putchar", "snprintf", "sprintf",
        "sscanf", "scanf", "calloc", "realloc", "exit", "abort", "strncpy", "strncmp",
        "strchr", "strrchr", "strstr", "atoi", "atol", "strtol", "strtoul"
    ];
    
    let mut c_code = String::from("#include <stdio.h>\n#include <stdlib.h>\n#include <string.h>\nint main() {\n    return ");
    for (i, t) in targets.iter().enumerate() {
        if i > 0 { c_code.push_str(" + "); }
        c_code.push_str(&format!("(long){}", t));
    }
    c_code.push_str(";\n}\n");
    
    let temp_dir = std::env::temp_dir();
    let c_path = temp_dir.join("dummy.c");
    let out_path = temp_dir.join("dummy");
    
    std::fs::write(&c_path, c_code).map_err(|e| format!("failed to write dummy.c: {}", e))?;
    
    let status = std::process::Command::new("gcc")
        .args(&["-static", "-Wl,--emit-relocs", "-fno-stack-protector", "-fno-pie", "-O0"])
        .arg(&c_path)
        .arg("-o")
        .arg(&out_path)
        .status()
        .map_err(|e| format!("failed to run gcc: {}", e))?;
        
    if !status.success() {
        return Err("gcc failed to compile dummy static binary".to_string());
    }
    
    let bytes = std::fs::read(&out_path).map_err(|e| format!("failed to read dummy: {}", e))?;
    let obj = object::File::parse(&*bytes).map_err(|e| format!("failed to parse dummy: {}", e))?;
    let text = obj.section_by_name(".text").ok_or("no .text in dummy")?;
    let text_data = text.data().map_err(|e| format!("failed to get .text data: {}", e))?;
    let text_addr = text.address();
    
    let mut relocs = Vec::new();
    for (offset, reloc) in text.relocations() {
        relocs.push((offset, reloc.size() / 8));
    }
    
    let mut sigs = Vec::new();
    for sym in obj.symbols() {
        if sym.section() == object::SymbolSection::Section(text.index()) {
            let name = sym.name().unwrap_or("?");
            if targets.contains(&name) {
                let start_addr = sym.address();
                let start = (start_addr - text_addr) as usize;
                let end = (start + 256).min(start + sym.size() as usize).min(text_data.len());
                if start >= end { continue; }
                
                let mut pattern = Vec::new();
                for i in start..end {
                    let addr = text_addr + i as u64;
                    let mut is_reloc = false;
                    for &(r_off, r_size) in &relocs {
                        let r_off_addr = r_off;
                        if addr >= r_off_addr && addr < r_off_addr + r_size as u64 {
                            is_reloc = true;
                            break;
                        }
                    }
                    if is_reloc {
                        pattern.push(None);
                    } else {
                        pattern.push(Some(text_data[i]));
                    }
                }
                sigs.push(CustomSig { name: name.to_string(), pattern });
            }
        }
    }
    
    Ok(sigs)
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
