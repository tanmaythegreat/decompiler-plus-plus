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

use object::{Object, ObjectSection, ObjectSymbol, ObjectSymbolTable, RelocationTarget, SymbolKind};
use std::collections::HashMap;
use std::env;
use std::fs;

struct FuncRegion {
    name: String,
    start: u64,
    end: u64,
}

/// Resolve PLT stub addresses to imported symbol names (`printf`, `malloc`,
/// ...) for dynamically-linked binaries.
///
/// This does NOT need an external libc.so / ld.so on disk: the binary's
/// own dynamic symbol table already carries the *names* of imported
/// (undefined) symbols -- that's what dynamic linking uses to look them
/// up at load time. What's missing is just the address correlation:
///   .rela.plt / .rela.dyn  maps  GOT slot address -> dynsym index
///   .plt / .plt.sec / .plt.got   each hold a stub per import that does
///                                 `jmp *[GOT slot]` (optionally preceded
///                                 by `endbr64` on CET-enabled builds)
/// so decoding those stubs and matching the jumped-through GOT address
/// against the relocation table gives us `plt_stub_addr -> symbol_name`.
fn resolve_plt_targets(obj: &object::File) -> HashMap<u64, String> {
    // Step 1: GOT slot address -> imported symbol name, straight from the
    // binary's own dynamic relocations + dynamic symbol table.
    let mut got_to_name: HashMap<u64, String> = HashMap::new();
    if let (Some(relocs), Some(dynsyms)) = (obj.dynamic_relocations(), obj.dynamic_symbol_table()) {
        for (addr, reloc) in relocs {
            if let RelocationTarget::Symbol(idx) = reloc.target() {
                if let Ok(sym) = dynsyms.symbol_by_index(idx) {
                    if let Ok(name) = sym.name() {
                        if !name.is_empty() {
                            got_to_name.insert(addr, name.to_string());
                        }
                    }
                }
            }
        }
    }

    let mut plt_to_name: HashMap<u64, String> = HashMap::new();
    if got_to_name.is_empty() {
        return plt_to_name; // statically linked / no dynamic imports
    }

    // Step 2: walk every PLT-flavored section, decode it as x86-64, and
    // match each `jmp [GOT slot]` against the table above. A stub starts
    // at the first instruction after the previous stub's terminating
    // `jmp` (or at section start); this holds for legacy .plt (stub IS
    // `jmp *GOT[n]`), .plt.sec / .plt.got (stub is `endbr64; jmp
    // *GOT[n]`), and the .plt resolver header (harmless miss: its GOT
    // slot isn't in the relocation table so it's just never inserted).
    for sec_name in [".plt", ".plt.sec", ".plt.got"] {
        let Some(sec) = obj.sections().find(|s| s.name() == Ok(sec_name)) else {
            continue;
        };
        let addr = sec.address();
        let Ok(data) = sec.data() else { continue };
        if data.is_empty() {
            continue;
        }

        let mut decoder = iced_x86::Decoder::with_ip(64, data, addr, iced_x86::DecoderOptions::NONE);
        let mut insn = iced_x86::Instruction::default();
        let mut stub_start = addr;
        let mut pending_new_stub = true;
        while decoder.can_decode() {
            decoder.decode_out(&mut insn);
            if insn.is_invalid() {
                break;
            }
            // Entries are padded to a fixed stride with `nop`s after the
            // terminating `jmp`; skip them so they don't get mistaken for
            // the start of the next stub.
            if insn.mnemonic() == iced_x86::Mnemonic::Nop {
                continue;
            }
            if pending_new_stub {
                stub_start = insn.ip();
                pending_new_stub = false;
            }
            if insn.mnemonic() == iced_x86::Mnemonic::Jmp {
                if insn.op0_kind() == iced_x86::OpKind::Memory {
                    let target = if insn.is_ip_rel_memory_operand() {
                        Some(insn.ip_rel_memory_address())
                    } else if insn.memory_base() == iced_x86::Register::None
                        && insn.memory_index() == iced_x86::Register::None
                    {
                        Some(insn.memory_displacement64())
                    } else {
                        None
                    };
                    if let Some(got_addr) = target {
                        if let Some(name) = got_to_name.get(&got_addr) {
                            plt_to_name.entry(stub_start).or_insert_with(|| name.clone());
                        }
                    }
                }
                pending_new_stub = true; // jmp always ends a PLT stub
            }
        }
    }
    plt_to_name
}

/// Resolve `call` targets inside *unlinked* object files (`.o`).
///
/// In a relocatable object file, a direct `call` instruction's 4-byte
/// displacement is not filled in with a real value yet -- it's a
/// placeholder (usually 0), to be patched by the linker using a
/// relocation entry (`R_X86_64_PLT32` / `R_X86_64_PC32`) recorded
/// against that exact byte offset. Decoding the placeholder as if it
/// were a real relative displacement (what a normal disassembler does
/// for *linked* code) produces a bogus target address for every
/// same-object call in a `.o`, which then prints as `sub_<bogus-addr>`.
///
/// This builds a map from "address of the call's displacement field" to
/// the real callee name, read directly from the section's relocation
/// table, so `.o` files resolve calls exactly like linked binaries do.
fn resolve_reloc_call_targets<'d, 'f>(obj: &'f object::File<'d>, text: &object::Section<'d, 'f>) -> HashMap<u64, String> {
    let mut out = HashMap::new();
    let Some(symtab) = obj.symbol_table() else {
        return out;
    };
    for (field_addr, reloc) in text.relocations() {
        match reloc.target() {
            RelocationTarget::Symbol(idx) => {
                if let Ok(sym) = symtab.symbol_by_index(idx) {
                    if let Ok(name) = sym.name() {
                        if !name.is_empty() {
                            out.insert(field_addr, name.to_string());
                        }
                    }
                }
            }
            // Some toolchains emit section-relative relocations (symbol
            // + addend pointing at an STT_SECTION symbol) instead of a
            // direct function symbol, e.g. for local/static callees.
            // The addend is then the callee's address within that
            // section, which -- since we only handle .text-to-.text
            // calls here -- lines up with our own function addresses,
            // so it can be resolved the same way a linked binary's call
            // target would be (left to the existing near_branch_target
            // lookup in lifter.rs; nothing to do here).
            RelocationTarget::Section(_) | RelocationTarget::Absolute => {}
            _ => {}
        }
    }
    out
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
    // through PLT stubs rather than a Text symbol at the call target.
    // Undefined dynamic symbols have no address of their own (address 0),
    // so they can't be resolved this way -- but they DO still carry a
    // name, which combined with .plt stub decoding lets us map the real
    // call target (the PLT stub address) to that name. See
    // `resolve_plt_targets` for how (no external libc/ld file needed).
    for s in obj.dynamic_symbols() {
        if let Ok(name) = s.name() {
            if !name.is_empty() && s.address() != 0 {
                call_symbols.entry(s.address()).or_insert_with(|| name.to_string());
            }
        }
    }
    let plt_symbols = resolve_plt_targets(&obj);
    for (addr, name) in &plt_symbols {
        call_symbols.entry(*addr).or_insert_with(|| name.clone());
    }
    // Unlinked object files (.o) haven't had `call` displacements patched
    // in yet -- see resolve_reloc_call_targets for why -- so build a
    // second map, keyed by call-site address, to override the (bogus)
    // near_branch_target-based lookup for those.
    let reloc_call_symbols = resolve_reloc_call_targets(&obj, &text);

    println!("== mini_decompiler ==");
    println!("input: {}", path);
    println!(".text @ 0x{:x}, {} bytes", text_addr, text_data.len());
    println!("functions found: {}", funcs.len());
    if !plt_symbols.is_empty() {
        println!("PLT imports resolved: {}", plt_symbols.len());
    }
    println!();

    for (n, f) in funcs.iter().take(max_funcs).enumerate() {
        let start_off = (f.start - text_addr) as usize;
        let end_off = ((f.end.min(text_end)) - text_addr) as usize;
        if start_off >= text_data.len() || end_off > text_data.len() || start_off >= end_off {
            continue;
        }
        let slice = &text_data[start_off..end_off];
        let insns = lifter::lift_region(slice, f.start, &call_symbols, &reloc_call_symbols);

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