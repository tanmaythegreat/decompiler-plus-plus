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

use mini_decompiler::analysis::*;
use mini_decompiler::ir::StructTable;
use mini_decompiler::{lifter, simplify::KnownFns};
use object::{Object, ObjectSection, ObjectSymbol, SymbolKind};
use std::collections::HashMap;
use std::env;
use std::fs;

struct Options {
    path: String,
    max_funcs: usize,
    show_asm: bool,
    json_out: Option<String>,
    only: Option<String>,
    flirt: Option<String>,
    auto_libc: bool,
    ai_rename: bool,
}

fn parse_args() -> Options {
    let args: Vec<String> = env::args().collect();
    let mut o = Options {
        path: String::new(),
        max_funcs: 16,
        show_asm: false,
        only: None,
        json_out: None,
        flirt: None,
        auto_libc: false,
        ai_rename: false,
    };
    let mut positional = Vec::new();
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "-a" | "--asm" => o.show_asm = true,
            "-A" | "--all" => o.max_funcs = usize::MAX,
            "-j" | "--json" => {
                i += 1;
                o.json_out = args.get(i).cloned();
                o.max_funcs = usize::MAX;
            }
            "-f" | "--func" => {
                i += 1;
                o.only = args.get(i).cloned();
            }
            "--flirt" => {
                i += 1;
                o.flirt = args.get(i).cloned();
            }
            "--auto-libc" => o.auto_libc = true,
            "--ai-rename" => o.ai_rename = true,
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
    eprintln!("  -j, --json FILE   write the analysis as JSON for the viewer (- for stdout)");
    eprintln!("  --auto-libc       generate libc/libgcc FLIRT signatures from the local toolchain");
    eprintln!("  --ai-rename       ask the configured AI provider to name sub_XXXXXX functions and their a<N>/v<N> locals");
    eprintln!("                    (Stage 3 fallback; needs an API key + provider, see File > AI Settings in the GUI or src/ai.rs)");
}

/// PLT stub address -> imported symbol name. Unchanged in spirit from the
/// original; it was one of the parts that already worked.
fn main() {
    let opts = parse_args();

    let bytes = fs::read(&opts.path).unwrap_or_else(|e| {
        eprintln!("failed to read {}: {}", opts.path, e);
        std::process::exit(1);
    });
    let obj = mini_decompiler::analysis::open_object(&opts.path, &bytes).unwrap_or_else(|e| {
        eprintln!("{}", e);
        std::process::exit(1);
    });

    let text = mini_decompiler::analysis::code_section(&obj).unwrap_or_else(|| {
        eprintln!("the file has no executable section, so there is no code to decompile");
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

    for f in &mut funcs {
        if f.name.starts_with("sub_") || f.name == "?" {
            if let Some(name) = call_symbols.get(&f.start) {
                f.name = lifter::sanitize_name(name);
            }
        }
    }

    if let Some(sig_path) = &opts.flirt {
        mini_decompiler::flirt::match_signatures(&mut funcs, text_data, text_addr, sig_path);
    }
    
    if opts.auto_libc {
        match mini_decompiler::flirt::auto_generate_libc_signatures() {
            Ok(sigs) => {
                let count = mini_decompiler::flirt::apply_custom_sigs(&mut funcs, text_data, text_addr, &sigs);
                println!("Auto-libc: matched {} functions", count);
            },
            Err(e) => eprintln!("Auto-libc failed: {}", e),
        }
    }

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

    if let Some(dest) = &opts.json_out {
        let text = emit_json(&results, &structs, &opts.path, &obj, &call_symbols);
        if dest == "-" {
            print!("{}", text);
        } else if let Err(e) = fs::write(dest, text) {
            eprintln!("failed to write {}: {}", dest, e);
            std::process::exit(1);
        }
        return;
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

    let rendered: Vec<String> = results.iter().map(|a| render(a, &structs, opts.show_asm)).collect();

    if opts.ai_rename {
        print_ai_renamed(&results, &rendered);
    } else {
        for text in &rendered {
            println!("{}", text);
        }
    }
}

/// Runs the Stage-3 AI naming pass (see `rename.rs`) over every function in
/// `results` that still has its deterministic `sub_XXXXXX` name, then
/// prints `rendered` with those names and their locals substituted in.
/// Falls back to printing the unrenamed output — with a stderr note —
/// rather than failing the whole run if the API key isn't set or a
/// request errors out.
fn print_ai_renamed(results: &[Analyzed], rendered: &[String]) {
    use mini_decompiler::ai::AiClient;
    use mini_decompiler::rename;

    let client = match AiClient::from_env() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("--ai-rename: {e}; printing without AI-suggested names");
            for text in rendered {
                println!("{}", text);
            }
            return;
        }
    };

    let by_name: HashMap<&str, &String> =
        results.iter().map(|a| a.name.as_str()).zip(rendered.iter()).collect();

    let targets: Vec<rename::AiTarget> = results
        .iter()
        .filter(|a| rename::needs_naming(a))
        .map(|a| {
            let code = by_name.get(a.name.as_str()).map(|s| (*s).clone()).unwrap_or_default();
            rename::make_target(a, code)
        })
        .collect();
    let total = targets.len();
    if total == 0 {
        for text in rendered {
            println!("{}", text);
        }
        return;
    }
    eprintln!("--ai-rename: asking {} to name {} unresolved function(s)...", client.provider().display_name(), total);

    let plan = rename::build_plan(
        &client,
        &targets,
        |_, _, _| {},
        |t, index, total, res| match res {
            Ok(_) => eprintln!("  [{index}/{total}] {} named", t.name),
            Err(e) => eprintln!("  [{index}/{total}] {}: skipped ({e})", t.name),
        },
    );

    for a in results {
        let text = by_name.get(a.name.as_str()).map(|s| s.as_str()).unwrap_or("");
        println!("{}", rename::apply_plan(&a.name, text, &plan));
    }
}

