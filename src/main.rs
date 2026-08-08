// mini_decompiler — x86-64 ELF -> C.
//
// Pipeline:
//   object parsing -> per-function disassembly (iced-x86)
//   -> expression IR (ir.rs, lifter.rs)
//   -> flag resolution (simplify.rs)
//   -> stack frame / variable / array / struct recovery (frame.rs)
//   -> CFG, dominators, post-dominators, structuring (cfg.rs)
//   -> copy propagation, dead-store elimination, call-arg recovery
//   -> C emission (cgen.rs)
//
// Functions are analysed twice: the first pass learns each local
// function's parameter count and whether it returns a value, and the
// second pass uses that to give call sites the right argument lists.

mod cfg;
mod cgen;
mod frame;
mod ir;
mod lifter;
mod ptr;
mod simplify;

use cfg::{Cfg, Structurer};
use frame::{Frame, GlobalMap};
use ir::*;
use object::{Object, ObjectSection, ObjectSymbol, ObjectSymbolTable, RelocationTarget, SymbolKind};
use simplify::KnownFns;
use std::collections::{HashMap, HashSet};
use std::env;
use std::fs;

struct FuncRegion {
    name: String,
    start: u64,
    end: u64,
}

struct Options {
    path: String,
    max_funcs: usize,
    show_asm: bool,
    only: Option<String>,
}

fn parse_args() -> Options {
    let args: Vec<String> = env::args().collect();
    let mut o = Options { path: String::new(), max_funcs: 16, show_asm: false, only: None };
    let mut positional = Vec::new();
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "-a" | "--asm" => o.show_asm = true,
            "-A" | "--all" => o.max_funcs = usize::MAX,
            "-f" | "--func" => {
                i += 1;
                o.only = args.get(i).cloned();
            }
            "-n" => {
                i += 1;
                o.max_funcs = args.get(i).and_then(|s| s.parse().ok()).unwrap_or(16);
            }
            "-h" | "--help" => {
                usage(&args[0]);
                std::process::exit(0);
            }
            s => positional.push(s.to_string()),
        }
        i += 1;
    }
    if positional.is_empty() {
        usage(&args[0]);
        std::process::exit(1);
    }
    o.path = positional[0].clone();
    if let Some(n) = positional.get(1).and_then(|s| s.parse::<usize>().ok()) {
        o.max_funcs = n;
    }
    o
}

fn usage(prog: &str) {
    eprintln!("usage: {} <binary> [max-functions] [options]", prog);
    eprintln!("  -a, --asm         also print the disassembly");
    eprintln!("  -A, --all         decompile every function");
    eprintln!("  -f, --func NAME   decompile just this function");
    eprintln!("  -n N              limit to N functions (default 16)");
}

/// PLT stub address -> imported symbol name. Unchanged in spirit from the
/// original; it was one of the parts that already worked.
fn resolve_plt_targets(obj: &object::File) -> HashMap<u64, String> {
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
        return plt_to_name;
    }
    // Also let `call [rel GOT]` resolve directly.
    for (addr, name) in &got_to_name {
        plt_to_name.insert(*addr, name.clone());
    }

    for sec_name in [".plt", ".plt.sec", ".plt.got"] {
        let Some(sec) = obj.sections().find(|s| s.name() == Ok(sec_name)) else { continue };
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
                pending_new_stub = true;
            }
        }
    }
    plt_to_name
}

/// Call-site displacement field address -> callee name, for unlinked `.o`.
fn resolve_reloc_call_targets<'d, 'f>(
    obj: &'f object::File<'d>,
    text: &object::Section<'d, 'f>,
) -> HashMap<u64, String> {
    let mut out = HashMap::new();
    let Some(symtab) = obj.symbol_table() else { return out };
    for (field_addr, reloc) in text.relocations() {
        if let RelocationTarget::Symbol(idx) = reloc.target() {
            if let Ok(sym) = symtab.symbol_by_index(idx) {
                if let Ok(name) = sym.name() {
                    if !name.is_empty() && sym.kind() != SymbolKind::Section {
                        out.insert(field_addr, name.to_string());
                    }
                }
            }
        }
    }
    out
}

fn escape_c(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        match c {
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\x{:02x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

/// Named data symbols plus every C string in the read-only sections, so
/// `lea rdi, [rip+0x...]` can print the literal instead of an address.
fn build_globals(obj: &object::File) -> GlobalMap {
    let mut g = GlobalMap::default();
    for s in obj.symbols() {
        if matches!(s.kind(), SymbolKind::Data) && s.address() != 0 {
            if let Ok(n) = s.name() {
                if !n.is_empty() {
                    g.names.entry(s.address()).or_insert_with(|| lifter::sanitize_name(n));
                }
            }
        }
    }
    for sec in obj.sections() {
        let name = sec.name().unwrap_or("");
        if !matches!(
            name,
            ".rodata" | ".rodata.str1.1" | ".rodata.str1.8" | ".data" | ".data.rel.ro" | ".text"
        ) {
            continue;
        }
        let Ok(data) = sec.data() else { continue };
        let base = sec.address();
        let printable = |b: u8| (0x20..0x7f).contains(&b) || b == b'\n' || b == b'\t' || b == b'\r';
        let mut i = 0usize;
        while i < data.len() {
            if printable(data[i]) {
                let start = i;
                while i < data.len() && printable(data[i]) {
                    i += 1;
                }
                let ok_term = i < data.len() && data[i] == 0;
                let len = i - start;
                if ok_term && len >= 3 {
                    let s = String::from_utf8_lossy(&data[start..i]).to_string();
                    g.strings.insert(base + start as u64, escape_c(&s));
                }
                i += 1;
            } else {
                i += 1;
            }
        }
    }
    g
}

/// Whether the function produces a value in rax, and at what width.
///
/// The original decided this with "did anything ever write rax", which
/// counted the `xor eax, eax` zero idiom and every clobber, so almost
/// nothing came out `void`.
fn detect_return(instrs: &[LiftedInsn], name: &str) -> Option<u8> {
    if name == "main" {
        return Some(4);
    }
    // The interesting write is the one that *reaches* a `ret`, not the
    // widest one anywhere: `sum_array` accumulates in rax at 64 bits but
    // returns `eax`, and taking the max declared it `long`.
    let rax_def = |ins: &LiftedInsn| -> Option<(u8, bool)> {
        ins.stmts.iter().rev().find_map(|st| match st {
            Stmt::Assign { dst: Expr::Reg(r), src } if r.full == "rax" => {
                Some((r.size, matches!(src, Expr::Call { .. })))
            }
            _ => None,
        })
    };
    let mut width: Option<u8> = None;
    for (i, ins) in instrs.iter().enumerate() {
        if !ins.stmts.iter().any(|s| matches!(s, Stmt::Return(_))) {
            continue;
        }
        for prev in instrs[..i].iter().rev().take(24) {
            if let Some((w, from_call)) = rax_def(prev) {
                if from_call {
                    // tail call: the value is whatever the callee returned
                    width = Some(width.map_or(8, |x: u8| x.max(8)));
                } else {
                    width = Some(width.map_or(w, |x: u8| x.max(w)));
                }
                break;
            }
        }
    }
    if width.is_some() {
        return width;
    }
    // a tail position `rax = call(...)` also produces a value
    for (i, ins) in instrs.iter().enumerate() {
        if !ins.is_call {
            continue;
        }
        let ret_soon = instrs[i + 1..]
            .iter()
            .take(4)
            .any(|n| n.stmts.iter().any(|s| matches!(s, Stmt::Return(_))));
        if ret_soon {
            return Some(8);
        }
    }
    None
}

/// Backward liveness over the CFG, on 64-bit register families.
fn liveness(cfg: &Cfg, instrs: &[LiftedInsn], returns: bool) -> Vec<HashSet<String>> {
    let n = cfg.blocks.len();
    let mut gen: Vec<HashSet<String>> = vec![HashSet::new(); n];
    let mut kill: Vec<HashSet<String>> = vec![HashSet::new(); n];

    for (b, blk) in cfg.blocks.iter().enumerate() {
        for &ix in &blk.instrs {
            for st in &instrs[ix].stmts {
                let mut rs = HashSet::new();
                simplify::reads(st, &mut rs);
                for r in rs {
                    if !kill[b].contains(&r) {
                        gen[b].insert(r);
                    }
                }
                if let Some(w) = simplify::writes(st) {
                    kill[b].insert(w);
                }
            }
        }
    }

    let mut live_in: Vec<HashSet<String>> = vec![HashSet::new(); n];
    let mut live_out: Vec<HashSet<String>> = vec![HashSet::new(); n];
    let mut changed = true;
    let mut rounds = 0;
    while changed && rounds < 200 {
        changed = false;
        rounds += 1;
        for b in (0..n).rev() {
            let mut out: HashSet<String> = HashSet::new();
            for s in &cfg.blocks[b].succs {
                if let Some(&si) = cfg.index.get(s) {
                    out.extend(live_in[si].iter().cloned());
                }
            }
            // `Return` already carries the value it returns, so rax is
            // not implicitly live at the exit; keeping it live is what
            // left dead `eax = 0;` lines just above every `return 0;`.
            let _ = returns;
            let mut inn = gen[b].clone();
            for r in out.iter() {
                if !kill[b].contains(r) {
                    inn.insert(r.clone());
                }
            }
            if out != live_out[b] {
                live_out[b] = out;
                changed = true;
            }
            if inn != live_in[b] {
                live_in[b] = inn;
                changed = true;
            }
        }
    }
    live_out
}

struct Analyzed {
    name: String,
    addr: u64,
    size: usize,
    insns: Vec<LiftedInsn>,
    frame: Frame,
    ret_width: Option<u8>,
}

#[allow(clippy::too_many_arguments)]
fn analyze(
    name: &str,
    start: u64,
    slice: &[u8],
    symbols: &HashMap<u64, String>,
    reloc_symbols: &HashMap<u64, String>,
    globals: &GlobalMap,
    structs: &mut StructTable,
    known: &KnownFns,
) -> Analyzed {
    let ctx = lifter::LiftCtx { symbols, reloc_symbols };
    let mut insns = lifter::lift_region(slice, start, &ctx);
    simplify::resolve_flags(&mut insns);

    let mut frame = Frame::build(&insns, globals.clone(), structs, name);
    frame.rewrite(&mut insns, structs);
    simplify::strip_frame(&mut insns);
    frame.recover_register_params(&insns);

    let ret_width = detect_return(&insns, name);

    // Seed the entry block with "argument register holds parameter N", so
    // reads of edi before the prologue spill resolve to `a1` rather than
    // to a bare register name.
    let entry_env: Vec<(String, Expr, u8)> = frame
        .vars
        .iter()
        .enumerate()
        .filter(|(_, v)| v.is_param)
        .filter_map(|(i, v)| {
            lifter::SYSV_INT_ARGS
                .get(v.param_index)
                .map(|r| (r.to_string(), Expr::Var(i), v.ty.size(structs).max(1) as u8))
        })
        .collect();

    // A parameter register the function never writes still holds the
    // parameter in every block, so it can be seeded everywhere. One it does
    // write is only trustworthy on entry — propagation here is block-local.
    let clobbered: HashSet<String> = insns
        .iter()
        .flat_map(|i| i.stmts.iter())
        .filter_map(|st| match st {
            Stmt::Assign { dst: Expr::Reg(r), .. } => Some(r.full.clone()),
            _ => None,
        })
        .collect();
    let stable_env: Vec<(String, Expr, u8)> =
        entry_env.iter().filter(|(r, _, _)| !clobbered.contains(r)).cloned().collect();

    let cfg = Cfg::build(&insns);
    for (bi, blk) in cfg.blocks.iter().enumerate() {
        let (Some(&first), Some(&last)) = (blk.instrs.first(), blk.instrs.last()) else {
            continue;
        };
        let seed: &[(String, Expr, u8)] =
            if bi == cfg.entry { &entry_env } else { &stable_env };
        simplify::propagate_block(&mut insns[first..=last], known, ret_width, seed);
    }

    // Aggregates reached *through* a pointer only become visible once the
    // address arithmetic has been folded back into one expression.
    ptr::recover(&mut insns, &mut frame, structs, name);

    let cfg = Cfg::build(&insns);
    let live_out = liveness(&cfg, &insns, ret_width.is_some());
    for (b, blk) in cfg.blocks.iter().enumerate() {
        let (Some(&first), Some(&last)) = (blk.instrs.first(), blk.instrs.last()) else {
            continue;
        };
        simplify::dce_block(&mut insns[first..=last], &live_out[b]);
    }

    Analyzed { name: name.to_string(), addr: start, size: slice.len(), insns, frame, ret_width }
}

/// If the function returns a variable we typed as a pointer, the signature
/// should say `struct foo *`, not `long`.
fn return_type(a: &Analyzed, structs: &StructTable) -> Type {
    let Some(w) = a.ret_width else { return Type::Void };
    let w = w.max(4);
    if w == 8 {
        for ins in &a.insns {
            for st in &ins.stmts {
                if let Stmt::Return(Some(Expr::Var(v))) = st {
                    if let Some(info) = a.frame.vars.get(*v) {
                        if matches!(info.ty, Type::Ptr(_)) {
                            return info.ty.clone();
                        }
                    }
                }
            }
        }
    }
    let _ = structs;
    Type::from_width(w, true)
}

fn render(a: &Analyzed, structs: &StructTable, show_asm: bool) -> String {
    let mut out = String::new();
    if show_asm {
        out.push_str("/* disassembly:\n");
        for ins in &a.insns {
            out.push_str(&format!("     {:08x}: {}\n", ins.addr, ins.asm_text));
        }
        out.push_str("*/\n");
    }

    let cfg = Cfg::build(&a.insns);
    let mut s = Structurer::new(&cfg, &a.insns);
    let nodes = s.run();
    let labels = s.goto_targets.clone();

    let mut reg_width = HashMap::new();
    cgen::used_regs(&nodes, &mut reg_width);
    let em = cgen::Emitter { frame: &a.frame, st: structs, reg_width };

    let ret_ty = return_type(a, structs).base_name(structs);
    let params = if a.frame.param_count() == 0 {
        "void".to_string()
    } else if a.name == "main" {
        // main's signature is fixed by the standard; recovering it as
        // `(int, long)` from the spill widths is technically what the code
        // does but not what anyone wants to read.
        ["int argc", "char **argv", "char **envp"][..a.frame.param_count().min(3)].join(", ")
    } else {
        a.frame
            .param_types
            .iter()
            .enumerate()
            .map(|(i, t)| t.declare(&format!("a{}", i + 1), structs))
            .collect::<Vec<_>>()
            .join(", ")
    };

    out.push_str(&format!(
        "/* {:#x}  {} bytes  {} basic blocks */\n",
        a.addr,
        a.size,
        cfg.blocks.len()
    ));
    out.push_str(&format!("{} {}({})\n{{\n", ret_ty, a.name, params));

    let mut used = HashSet::new();
    cgen::used_vars(&nodes, &mut used);
    let decls = format!("{}{}", em.declarations(&used), em.reg_declarations());
    if !decls.is_empty() {
        out.push_str(&decls);
        out.push('\n');
    }
    em.nodes(&nodes, 1, &labels, &mut out);
    out.push_str("}\n");
    out
}

fn main() {
    let opts = parse_args();

    let bytes = fs::read(&opts.path).unwrap_or_else(|e| {
        eprintln!("failed to read {}: {}", opts.path, e);
        std::process::exit(1);
    });
    let obj = object::File::parse(&*bytes).unwrap_or_else(|e| {
        eprintln!("failed to parse object file: {}", e);
        std::process::exit(1);
    });

    let text = obj.sections().find(|s| s.name() == Ok(".text")).unwrap_or_else(|| {
        eprintln!("no .text section found");
        std::process::exit(1);
    });
    let text_addr = text.address();
    let text_data = text.data().unwrap_or(&[]);
    let text_end = text_addr + text_data.len() as u64;

    let mut funcs: Vec<FuncRegion> = obj
        .symbols()
        .filter(|s| s.kind() == SymbolKind::Text && s.size() > 0)
        .filter(|s| s.address() >= text_addr && s.address() < text_end)
        .map(|s| FuncRegion {
            name: lifter::sanitize_name(s.name().unwrap_or("?")),
            start: s.address(),
            end: (s.address() + s.size()).min(text_end),
        })
        .collect();
    funcs.sort_by_key(|f| f.start);
    funcs.dedup_by_key(|f| f.start);

    if funcs.is_empty() {
        // Stripped: recover function starts by scanning for call targets
        // and for the classic prologue, instead of treating all of .text
        // as one function the way the original did.
        funcs = scan_function_starts(&obj, text_addr, text_data);
    }

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
    let reloc_call_symbols = resolve_reloc_call_targets(&obj, &text);
    let globals = build_globals(&obj);

    let selected: Vec<&FuncRegion> = match &opts.only {
        Some(n) => funcs.iter().filter(|f| &f.name == n).collect(),
        None => funcs.iter().take(opts.max_funcs).collect(),
    };

    // ---- pass 1: learn signatures of local functions -------------------
    let mut known = KnownFns::default();
    {
        let mut throwaway = StructTable::default();
        for f in &funcs {
            let Some(slice) = slice_of(text_data, text_addr, f) else { continue };
            let a = analyze(
                &f.name,
                f.start,
                slice,
                &call_symbols,
                &reloc_call_symbols,
                &globals,
                &mut throwaway,
                &KnownFns::default(),
            );
            known.params.insert(f.name.clone(), a.frame.param_count());
            known.returns.insert(f.name.clone(), a.ret_width.is_some());
        }
    }

    // ---- pass 2: real analysis, now with call signatures ---------------
    let mut structs = StructTable::default();
    let mut results = Vec::new();
    for f in &selected {
        let Some(slice) = slice_of(text_data, text_addr, f) else { continue };
        results.push(analyze(
            &f.name,
            f.start,
            slice,
            &call_symbols,
            &reloc_call_symbols,
            &globals,
            &mut structs,
            &known,
        ));
    }

    println!("/* mini_decompiler");
    println!(" * input   : {}", opts.path);
    println!(" * .text   : {:#x} .. {:#x} ({} bytes)", text_addr, text_end, text_data.len());
    println!(" * symbols : {} functions", funcs.len());
    if !plt_symbols.is_empty() {
        println!(" * imports : {} PLT entries resolved", plt_symbols.len());
    }
    println!(" */\n");

    if !structs.defs.is_empty() {
        println!("/* recovered aggregate types */");
        print!("{}", structs.render());
    }

    for a in &results {
        println!("{}", render(a, &structs, opts.show_asm));
    }
}

fn slice_of<'a>(data: &'a [u8], base: u64, f: &FuncRegion) -> Option<&'a [u8]> {
    let s = f.start.checked_sub(base)? as usize;
    let e = f.end.checked_sub(base)? as usize;
    if s >= data.len() || e > data.len() || s >= e {
        return None;
    }
    Some(&data[s..e])
}

/// Best-effort function discovery for stripped binaries: the entry point,
/// every direct `call` target inside .text, and every `endbr64`/`push rbp`
/// prologue. Ends are the next start.
fn scan_function_starts(obj: &object::File, text_addr: u64, data: &[u8]) -> Vec<FuncRegion> {
    let mut starts: std::collections::BTreeSet<u64> = std::collections::BTreeSet::new();
    let entry = obj.entry();
    if entry >= text_addr && entry < text_addr + data.len() as u64 {
        starts.insert(entry);
    }
    let mut decoder = iced_x86::Decoder::with_ip(64, data, text_addr, iced_x86::DecoderOptions::NONE);
    let mut insn = iced_x86::Instruction::default();
    let mut prev_endbr = None;
    while decoder.can_decode() {
        decoder.decode_out(&mut insn);
        if insn.is_invalid() {
            continue;
        }
        match insn.mnemonic() {
            iced_x86::Mnemonic::Call
                if insn.op0_kind() == iced_x86::OpKind::NearBranch64 =>
            {
                let t = insn.near_branch_target();
                if t >= text_addr && t < text_addr + data.len() as u64 {
                    starts.insert(t);
                }
            }
            iced_x86::Mnemonic::Endbr64 => prev_endbr = Some(insn.ip()),
            iced_x86::Mnemonic::Push
                if insn.op0_register() == iced_x86::Register::RBP =>
            {
                starts.insert(prev_endbr.take().unwrap_or_else(|| insn.ip()));
            }
            _ => {}
        }
    }
    let v: Vec<u64> = starts.into_iter().collect();
    let text_end = text_addr + data.len() as u64;
    v.iter()
        .enumerate()
        .map(|(i, &s)| FuncRegion {
            name: format!("sub_{:x}", s),
            start: s,
            end: v.get(i + 1).copied().unwrap_or(text_end),
        })
        .collect()
}
