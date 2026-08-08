// mini_decompiler — toy pipeline:
//   binary file -> object-file parsing -> per-function x86-64 disassembly
//   (iced-x86) -> lift to small IR (src/lifter.rs, src/ir.rs) -> CFG +
//   dominators + if/while structuring (src/cfg.rs) -> C-like pseudocode.
//
// Scope is intentionally small (see the accompanying design doc): a
// representative x86-64 subset, ELF input, no type inference / SSA /
// inlining handling. Anything it can't structure falls back to goto.

mod cfg;
mod ir;
mod lifter;

use object::{Object, ObjectSection, ObjectSymbol, SymbolKind};
use std::collections::HashMap;
use std::env;
use std::fs;

struct FuncRegion {
    name: String,
    start: u64,
    end: u64,
}

fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() < 2 {
        eprintln!("usage: {} <binary-file> [max-functions]", args[0]);
        std::process::exit(1);
    }
    let path = &args[1];
    let max_funcs: usize = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(8);

    let bytes = fs::read(path).unwrap_or_else(|e| {
        eprintln!("failed to read {}: {}", path, e);
        std::process::exit(1);
    });

    let obj = object::File::parse(&*bytes).unwrap_or_else(|e| {
        eprintln!("failed to parse object file: {}", e);
        std::process::exit(1);
    });

    let text = obj
        .sections()
        .find(|s| s.name() == Ok(".text"))
        .unwrap_or_else(|| {
            eprintln!("no .text section found");
            std::process::exit(1);
        });
    let text_addr = text.address();
    let text_data = text.data().unwrap_or(&[]);
    let text_end = text_addr + text_data.len() as u64;

    // gather FUNC symbols that live inside .text
    let mut funcs: Vec<FuncRegion> = obj
        .symbols()
        .filter(|s| s.kind() == SymbolKind::Text && s.size() > 0)
        .filter(|s| s.address() >= text_addr && s.address() < text_end)
        .map(|s| FuncRegion {
            name: s.name().unwrap_or("?").to_string(),
            start: s.address(),
            end: s.address() + s.size(),
        })
        .collect();
    funcs.sort_by_key(|f| f.start);
    funcs.dedup_by_key(|f| f.start);

    if funcs.is_empty() {
        // stripped binary: fall back to disassembling from the entry point
        // up to the first `ret`.
        let entry = obj.entry();
        let off = (entry.saturating_sub(text_addr)) as usize;
        if off < text_data.len() {
            funcs.push(FuncRegion {
                name: "entry".to_string(),
                start: entry,
                end: text_end,
            });
        }
    }

    // Build an address -> name map covering every named function symbol in
    // the object file (not just the ones we're about to print), so `call`
    // targets can be resolved to real names instead of `sub_<addr>` even
    // when the callee itself is outside our `max_funcs` window.
    let mut call_symbols: HashMap<u64, String> = HashMap::new();
    for s in obj.symbols() {
        if s.kind() == SymbolKind::Text {
            if let Ok(name) = s.name() {
                if !name.is_empty() {
                    call_symbols.entry(s.address()).or_insert_with(|| name.to_string());
                }
            }
        }
    }
    // Dynamically-linked binaries route external calls (printf, malloc, ...)
    // through PLT stubs rather than a Text symbol at the call target, so
    // pull those names from the dynamic symbol table too.
    for s in obj.dynamic_symbols() {
        if let Ok(name) = s.name() {
            if !name.is_empty() {
                call_symbols.entry(s.address()).or_insert_with(|| name.to_string());
            }
        }
    }

    println!("== mini_decompiler ==");
    println!("input: {}", path);
    println!(".text @ 0x{:x}, {} bytes", text_addr, text_data.len());
    println!("functions found: {}\n", funcs.len());

    for (n, f) in funcs.iter().take(max_funcs).enumerate() {
        let start_off = (f.start - text_addr) as usize;
        let end_off = ((f.end.min(text_end)) - text_addr) as usize;
        if start_off >= text_data.len() || end_off > text_data.len() || start_off >= end_off {
            continue;
        }
        let slice = &text_data[start_off..end_off];
        let insns = lifter::lift_region(slice, f.start, &call_symbols);

        println!("---------------------------------------------------------");
        println!("[{}] function {} @ 0x{:x} ({} bytes, {} instrs)", n, f.name, f.start, slice.len(), insns.len());
        println!("---------------------------------------------------------");
        println!("-- disassembly --");
        for ins in &insns {
            println!("  0x{:08x}: {}", ins.addr, ins.asm_text);
        }

        let func = cfg::Function::build(sanitize_name(&f.name), insns);
        println!("\n-- pseudocode ({} basic blocks) --", func.blocks.len());
        print!("{}", func.render());
        println!();
    }
}

fn sanitize_name(n: &str) -> String {
    if n.is_empty() || n == "?" {
        "func".to_string()
    } else {
        n.chars().map(|c| if c.is_ascii_alphanumeric() || c == '_' { c } else { '_' }).collect()
    }
}