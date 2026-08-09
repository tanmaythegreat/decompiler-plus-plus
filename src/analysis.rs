// analysis.rs — the whole pipeline from an ELF file to analysed functions.
//
// This lives in the library rather than in the CLI so that both front ends
// (the command line and the native viewer) drive exactly the same analysis
// and cannot drift apart.

use crate::cfg::{Cfg, Structurer};
use crate::frame::{Frame, GlobalMap};
use crate::ir::*;
use crate::simplify::KnownFns;
use crate::{cgen, idiom, json, lifter, ptr, simplify};
use object::{Object, ObjectSection, ObjectSymbol, ObjectSymbolTable, RelocationTarget, SymbolKind};
use std::collections::{HashMap, HashSet};

pub struct FuncRegion {
    pub name: String,
    pub start: u64,
    pub end: u64,
    /// true when this function was renamed by FLIRT signature matching
    pub is_lib: bool,
}

pub fn resolve_plt_targets(obj: &object::File) -> HashMap<u64, String> {
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
pub fn resolve_reloc_call_targets<'d, 'f>(
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

pub fn escape_c(s: &str) -> String {
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
pub fn build_globals(obj: &object::File) -> GlobalMap {
    let mut g = GlobalMap::default();
    for s in obj.symbols() {
        if matches!(s.kind(), SymbolKind::Text) && s.address() != 0 {
            if let Ok(n) = s.name() {
                if !n.is_empty() {
                    g.funcs.entry(s.address()).or_insert_with(|| lifter::sanitize_name(n));
                }
            }
        }
    }
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
            ".rodata" | ".rodata.str1.1" | ".rodata.str1.8" | ".data" | ".data.rel.ro"
            | ".text" | "__cstring" | "__const" | "__text" | ".rdata"
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
/// A constant that is the entry address of a known function is a function
/// pointer, not an integer. gcc hands `main` to `__libc_start_main` this
/// way, and printing it as `4682` hides the single most useful fact in
/// `_start`.
pub fn name_code_pointers(instrs: &mut [LiftedInsn], g: &GlobalMap) {
    fn go(e: Expr, g: &GlobalMap) -> Expr {
        if let Expr::Const(v) = &e {
            if *v > 0 {
                if let Some(n) = g.funcs.get(&(*v as u64)) {
                    return Expr::Lit(n.clone());
                }
            }
        }
        match e {
            Expr::Bin { op, l, r } => {
                Expr::Bin { op, l: Box::new(go(*l, g)), r: Box::new(go(*r, g)) }
            }
            Expr::Un { op, e } => Expr::Un { op, e: Box::new(go(*e, g)) },
            Expr::Cast { ty, e } => Expr::Cast { ty, e: Box::new(go(*e, g)) },
            Expr::Call { name, args, indirect } => Expr::Call {
                name,
                args: args.into_iter().map(|a| go(a, g)).collect(),
                indirect: indirect.map(|i| Box::new(go(*i, g))),
            },
            other => other,
        }
    }
    for ins in instrs {
        for st in ins.stmts.iter_mut() {
            let taken = std::mem::replace(st, Stmt::Nop);
            *st = match taken {
                Stmt::Assign { dst, src } => Stmt::Assign { dst, src: go(src, g) },
                Stmt::Do(e) => Stmt::Do(go(e, g)),
                Stmt::Return(Some(e)) => Stmt::Return(Some(go(e, g))),
                other => other,
            };
        }
    }
}

pub fn detect_return(instrs: &[LiftedInsn], name: &str) -> Option<u8> {
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
pub fn liveness(cfg: &Cfg, instrs: &[LiftedInsn], returns: bool) -> Vec<HashSet<String>> {
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

pub struct Analyzed {
    pub name: String,
    pub addr: u64,
    pub size: usize,
    pub insns: Vec<LiftedInsn>,
    pub frame: Frame,
    pub ret_width: Option<u8>,
    /// the instruction stream before any rewriting, which is what the
    /// viewer's stepper executes -- the analysed form has had its frame
    /// folded away and is no longer a faithful model of the machine
    pub raw: Vec<LiftedInsn>,
    /// true when this function was renamed by FLIRT — stays true even
    /// after the user gives it a custom name in the GUI
    pub is_lib: bool,
}

#[allow(clippy::too_many_arguments)]
pub fn analyze(
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
    let raw = insns.clone();

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

    idiom::run(&mut insns);
    name_code_pointers(&mut insns, &globals);

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

    Analyzed { name: name.to_string(), addr: start, size: slice.len(), insns, frame, ret_width, raw, is_lib: false }
}

/// If the function returns a variable we typed as a pointer, the signature
/// should say `struct foo *`, not `long`.
pub fn return_type(a: &Analyzed, structs: &StructTable) -> Type {
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

pub fn render(a: &Analyzed, structs: &StructTable, show_asm: bool) -> String {
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
    let em = cgen::Emitter {
        frame: &a.frame,
        st: structs,
        reg_width,
        no_cast: false,
        trace: Default::default(),
    };

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

/// The body of one function, rendered twice -- with and without the width
/// casts -- together with the address each line came from.
/// The full C spelling of a type, declarator and all: `base_name` drops the
/// pointer stars, which is right for a declaration but wrong for a label.
pub fn spell(t: &Type, st: &StructTable) -> String {
    t.declare("", st).trim().to_string()
}

pub fn body_lines(a: &Analyzed, structs: &StructTable, no_cast: bool) -> Vec<(Option<u64>, String)> {
    let cfg = Cfg::build(&a.insns);
    let mut s = Structurer::new(&cfg, &a.insns);
    let nodes = s.run();
    let labels = s.goto_targets.clone();
    let mut reg_width = HashMap::new();
    cgen::used_regs(&nodes, &mut reg_width);
    let em = cgen::Emitter {
        frame: &a.frame,
        st: structs,
        reg_width,
        no_cast,
        trace: Default::default(),
    };
    let mut text = String::new();
    em.nodes(&nodes, 1, &labels, &mut text);
    let trace = em.trace.borrow().clone();
    text.lines()
        .enumerate()
        .map(|(i, l)| (trace.get(i).copied().flatten(), l.to_string()))
        .collect()
}

/// The complete rendered function -- banner, signature, declarations, body --
/// with the instruction address each line came from. The viewer needs the
/// mapping; the CLI just concatenates the text.
pub fn render_lines(
    a: &Analyzed,
    structs: &StructTable,
    no_cast: bool,
) -> Vec<(Option<u64>, String)> {
    let cfg = Cfg::build(&a.insns);
    let mut out: Vec<(Option<u64>, String)> = Vec::new();
    out.push((
        None,
        format!("/* {:#x}  {} bytes  {} basic blocks */", a.addr, a.size, cfg.blocks.len()),
    ));
    out.push((None, format!("{} {}({})", signature_ret(a, structs), a.name, signature_params(a, structs))));
    out.push((None, "{".to_string()));

    let mut s = Structurer::new(&cfg, &a.insns);
    let nodes = s.run();
    let mut used = HashSet::new();
    cgen::used_vars(&nodes, &mut used);
    let mut reg_width = HashMap::new();
    cgen::used_regs(&nodes, &mut reg_width);
    let em = cgen::Emitter {
        frame: &a.frame,
        st: structs,
        reg_width,
        no_cast,
        trace: Default::default(),
    };
    let decls = format!("{}{}", em.declarations(&used), em.reg_declarations());
    if !decls.is_empty() {
        for l in decls.lines() {
            out.push((None, l.to_string()));
        }
        out.push((None, String::new()));
    }
    for l in body_lines(a, structs, no_cast) {
        out.push(l);
    }
    out.push((None, "}".to_string()));
    out
}

pub fn signature_ret(a: &Analyzed, structs: &StructTable) -> String {
    return_type(a, structs).base_name(structs)
}

pub fn signature_params(a: &Analyzed, structs: &StructTable) -> String {
    if a.frame.param_count() == 0 {
        "void".to_string()
    } else if a.name == "main" {
        ["int argc", "char **argv", "char **envp"][..a.frame.param_count().min(3)].join(", ")
    } else {
        a.frame
            .param_types
            .iter()
            .enumerate()
            .map(|(i, t)| t.declare(&format!("a{}", i + 1), structs))
            .collect::<Vec<_>>()
            .join(", ")
    }
}

pub fn emit_json(
    analyzed: &[Analyzed],
    structs: &StructTable,
    path: &str,
    obj: &object::File,
    imports: &HashMap<u64, String>,
) -> String {
    let mut out = String::new();

    // The stepper needs the actual bytes of the loaded image to read string
    // literals and globals; without them every `printf` argument is a
    // dangling pointer.
    let mut mem = Vec::new();
    for sec in obj.sections() {
        let name = sec.name().unwrap_or("");
        if !matches!(
            name,
            ".text" | ".rodata" | ".data" | ".data.rel.ro" | ".got" | ".got.plt" | ".rdata"
                | "__text" | "__cstring" | "__const" | "__data"
        ) {
            continue;
        }
        let Ok(data) = sec.data() else { continue };
        if data.is_empty() {
            continue;
        }
        let hex: String = data.iter().map(|b| format!("{:02x}", b)).collect();
        mem.push(format!(
            "{{\"name\":{},\"addr\":{},\"data\":{}}}",
            json::q(name),
            sec.address(),
            json::q(&hex)
        ));
    }

    let imp: Vec<String> = imports
        .iter()
        .map(|(a, n)| format!("{{\"addr\":{},\"name\":{}}}", a, json::q(n)))
        .collect();

    out.push_str(&format!(
        "{{\n\"file\":{},\n\"structs\":{},\n\"memory\":[{}],\n\"imports\":[{}],\n\"functions\":[\n",
        json::q(path),
        json::structs(structs),
        mem.join(","),
        imp.join(",")
    ));

    let bodies: Vec<String> = analyzed
        .iter()
        .map(|a| {
            let cfg = Cfg::build(&a.insns);

            let asm: Vec<String> = a
                .raw
                .iter()
                .map(|i| {
                    format!(
                        "{{\"addr\":{},\"len\":{},\"text\":{},\"stmts\":[{}],\"call\":{},\"end\":{},\"targets\":[{}]}}",
                        i.addr,
                        i.len,
                        json::q(&i.asm_text),
                        i.stmts.iter().map(json::stmt).collect::<Vec<_>>().join(","),
                        i.is_call,
                        i.is_block_end,
                        i.targets.iter().map(|t| t.to_string()).collect::<Vec<_>>().join(",")
                    )
                })
                .collect();

            let blocks: Vec<String> = cfg
                .blocks
                .iter()
                .map(|b| {
                    format!(
                        "{{\"addr\":{},\"succs\":[{}],\"ret\":{},\"insns\":[{}]}}",
                        b.addr,
                        b.succs.iter().map(|s| s.to_string()).collect::<Vec<_>>().join(","),
                        b.ends_return,
                        b.instrs
                            .iter()
                            .map(|&ix| a.insns[ix].addr.to_string())
                            .collect::<Vec<_>>()
                            .join(",")
                    )
                })
                .collect();

            let with = body_lines(a, structs, false);
            let without = body_lines(a, structs, true);
            let lines: Vec<String> = with
                .iter()
                .enumerate()
                .map(|(i, (addr, text))| {
                    format!(
                        "{{\"addr\":{},\"c\":{},\"nc\":{}}}",
                        addr.map(|a| a.to_string()).unwrap_or_else(|| "null".into()),
                        json::q(text),
                        json::q(without.get(i).map(|(_, t)| t.as_str()).unwrap_or(text))
                    )
                })
                .collect();

            let vars: Vec<String> = a
                .frame
                .vars
                .iter()
                .enumerate()
                .map(|(i, v)| {
                    format!(
                        "{{\"id\":{},\"name\":{},\"ty\":{},\"off\":{},\"param\":{},\"size\":{}}}",
                        i,
                        json::q(&v.name),
                        json::q(&spell(&v.ty, structs)),
                        v.off,
                        v.is_param,
                        v.size
                    )
                })
                .collect();

            format!(
                "{{\"name\":{},\"addr\":{},\"size\":{},\"ret\":{},\"params\":[{}],\"vars\":[{}],\"blocks\":[{}],\"asm\":[{}],\"lines\":[{}]}}",
                json::q(&a.name),
                a.addr,
                a.size,
                json::q(&spell(&return_type(a, structs), structs)),
                a.frame
                    .param_types
                    .iter()
                    .enumerate()
                    .map(|(i, t)| format!(
                        "{{\"name\":\"a{}\",\"ty\":{}}}",
                        i + 1,
                        json::q(&spell(t, structs))
                    ))
                    .collect::<Vec<_>>()
                    .join(","),
                vars.join(","),
                blocks.join(","),
                asm.join(","),
                lines.join(",")
            )
        })
        .collect();

    out.push_str(&bodies.join(",\n"));
    out.push_str("\n]}\n");
    out
}


/// One binary, fully analysed. Both front ends go through this so they can
/// never disagree about what the program contains.
pub struct Program {
    pub path: String,
    pub funcs: Vec<Analyzed>,
    pub structs: StructTable,
    /// loaded sections the stepper can read: name, base address, bytes
    pub memory: Vec<(String, u64, Vec<u8>)>,
    /// call target -> symbol name, including resolved PLT stubs
    pub symbols: HashMap<u64, String>,
    pub text_range: (u64, u64),
    pub plt_count: usize,
    /// printable strings found in the image, for the strings view
    pub strings: Vec<(u64, String)>,
}

/// The section holding the code. ELF and PE call it `.text`; Mach-O calls it
/// `__text`; a linker script can call it anything. Falling back to "the
/// executable section with the most bytes in it" means a file with an unusual
/// layout still opens instead of being rejected outright.
pub fn code_section<'a>(obj: &'a object::File) -> Option<object::Section<'a, 'a>> {
    if let Some(s) = obj
        .sections()
        .find(|s| matches!(s.name(), Ok(".text") | Ok("__text") | Ok("CODE")))
    {
        return Some(s);
    }
    obj.sections()
        .filter(|s| {
            matches!(s.kind(), object::SectionKind::Text) && s.size() > 0
        })
        .max_by_key(|s| s.size())
}

/// Sections worth handing to the stepper: anything with real bytes that the
/// program could read. Listing them by name misses every format but ELF.
fn is_loadable(s: &object::Section) -> bool {
    use object::SectionKind::*;
    matches!(
        s.kind(),
        Text | Data | ReadOnlyData | ReadOnlyString | ReadOnlyDataWithRel | UninitializedData
    ) && s.size() > 0
        && s.address() != 0
}

/// Parse an object file, with an error a person can act on.
pub fn open_object<'a>(path: &str, bytes: &'a [u8]) -> Result<object::File<'a>, String> {
    let obj = object::File::parse(bytes).map_err(|e| {
        format!(
            "could not parse {} as an object file: {}\n\n\
             Supported: ELF, PE/COFF and Mach-O executables, shared libraries \
             and .o files. A script, an archive, a core dump or a packed \
             binary will not open.",
            path.rsplit('/').next().unwrap_or(path),
            e
        )
    })?;
    if obj.architecture() != object::Architecture::X86_64 {
        return Err(format!(
            "{:?} binaries are not supported — this decompiler lifts x86-64 only.",
            obj.architecture()
        ));
    }
    Ok(obj)
}

pub fn analyze_bytes(path: &str, bytes: &[u8], sig_path: Option<&str>, custom_sigs: Option<&[crate::flirt::CustomSig]>) -> Result<Program, String> {
    let obj = open_object(path, bytes)?;
    let text = code_section(&obj).ok_or(
        "the file has no executable section, so there is no code to decompile",
    )?;
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
            is_lib: false,
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

    let entry_addr = obj.entry();
    for f in &mut funcs {
        if f.name.starts_with("sub_") || f.name == "?" {
            if f.start == entry_addr {
                f.name = "_start".to_string();
            } else if let Some(name) = call_symbols.get(&f.start) {
                f.name = lifter::sanitize_name(name);
            }
        }
    }

    if let Some(sig_path) = sig_path {
        crate::flirt::match_signatures(&mut funcs, text_data, text_addr, sig_path);
    }
    
    // Auto-apply default Windows MSVC signatures for PE files (x64)
    if obj.format() == object::BinaryFormat::Pe {
        crate::flirt::match_signature_bytes(
            &mut funcs, text_data, text_addr, 
            crate::default_sigs::LIBCMT_MSVC_X64, 
            false, 
            "default_libcmt_msvc_x64"
        );
        crate::flirt::match_signature_bytes(
            &mut funcs, text_data, text_addr, 
            crate::default_sigs::LIBVCRUNTIME_MSVC_X64, 
            false, 
            "default_libvcruntime_msvc_x64"
        );
    } else if obj.format() == object::BinaryFormat::Elf {
        // For ELF static binaries: generate signatures directly from the system's libc.a.
        // This parses every .o file in the archive and wildcards relocation bytes so the
        // pattern matches regardless of link-time address layout.
        match crate::flirt::auto_generate_libc_signatures() {
            Ok(libc_sigs) => {
                let matched = crate::flirt::apply_custom_sigs(
                    &mut funcs, text_data, text_addr, &libc_sigs
                );
                if matched > 0 {
                    println!("Auto-libc: matched {} functions", matched);
                }
            }
            Err(e) => eprintln!("Auto-libc: failed to generate signatures: {}", e),
        }
    }
    
    if let Some(sigs) = custom_sigs {
        crate::flirt::apply_custom_sigs(&mut funcs, text_data, text_addr, sigs);
    }

    let selected: Vec<&FuncRegion> = funcs.iter().collect();

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
        let mut a = analyze(
            &f.name,
            f.start,
            slice,
            &call_symbols,
            &reloc_call_symbols,
            &globals,
            &mut structs,
            &known,
        );
        a.is_lib = f.is_lib;  // propagate FLIRT tag from FuncRegion
        results.push(a);
    }


    let mut memory = Vec::new();
    for sec in obj.sections() {
        if !is_loadable(&sec) {
            continue;
        }
        let name = sec.name().unwrap_or("<unnamed>").to_string();
        let data = sec.data().unwrap_or(&[]).to_vec();
        if !data.is_empty() {
            memory.push((name, sec.address(), data));
        }
    }

    let mut strings: Vec<(u64, String)> = globals
        .strings
        .iter()
        .map(|(a, s)| (*a, s.clone()))
        .collect();
    strings.sort_by_key(|(a, _)| *a);
    strings.dedup_by_key(|(a, _)| *a);

    Ok(Program {
        strings,
        path: path.to_string(),
        funcs: results,
        structs,
        memory,
        symbols: call_symbols,
        text_range: (text_addr, text_end),
        plt_count: plt_symbols.len(),
    })
}

/// Every place in the binary that refers to `target` -- as a call, as a
/// branch, or as a constant handed around as a value.
pub fn references_to(prog: &Program, target: u64, name: &str) -> Vec<(String, u64, String)> {
    let mut out = Vec::new();
    for f in &prog.funcs {
        for ins in &f.raw {
            let mut hit = ins.targets.contains(&target);
            if !hit {
                for st in &ins.stmts {
                    let mut found = false;
                    let mut probe = |e: &Expr| {
                        e.walk(&mut |x| match x {
                            Expr::Const(v) if *v as u64 == target => found = true,
                            Expr::Lit(s) if s == name => found = true,
                            Expr::Call { name: n, .. } if n == name => found = true,
                            Expr::Mem(m) if m.rip_abs == Some(target) => found = true,
                            _ => {}
                        });
                    };
                    match st {
                        Stmt::Assign { dst, src } => {
                            probe(dst);
                            probe(src);
                        }
                        Stmt::Do(e) | Stmt::Return(Some(e)) | Stmt::If { cond: e, .. } => probe(e),
                        _ => {}
                    }
                    if found {
                        hit = true;
                        break;
                    }
                }
            }
            if hit {
                out.push((f.name.clone(), ins.addr, ins.asm_text.clone()));
            }
        }
    }
    out
}

pub fn analyze_file(path: &str, sig_path: Option<&str>, custom_sigs: Option<&[crate::flirt::CustomSig]>) -> Result<Program, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("cannot read {}: {}", path, e))?;
    analyze_bytes(path, &bytes, sig_path, custom_sigs)
}

pub fn slice_of<'a>(data: &'a [u8], base: u64, f: &FuncRegion) -> Option<&'a [u8]> {
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
pub fn scan_function_starts(obj: &object::File, text_addr: u64, data: &[u8]) -> Vec<FuncRegion> {
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
            is_lib: false,
        })
        .collect()
}
