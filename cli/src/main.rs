//! Phase 1 + Phase 2 deliverable CLI.
//!
//! Wires the whole pipeline built so far together: `arch_x86::X86Disassembler`
//! decodes raw bytes, `arch_x86::X86Lifter` lifts each decoded instruction
//! into `ir_pcode::Instruction`s, and the results are accumulated into a
//! `decompiler_core::Function` / `BasicBlock` and printed (Phase 1) --
//! then `analysis_passes::CfgBuilder` turns that flat block into a real CFG,
//! `analysis_passes::ConstantPropagation` runs a dataflow analysis over it
//! and simplifies the IR in place, and `output_c::generate` renders the
//! result as C-like pseudocode (Phase 2).
//!
//! Usage:
//!   decompiler-cli                      # run the built-in demo function
//!   decompiler-cli --hex "B8 05 00 00 00 C3" [--base 0x1000]
//!   decompiler-cli --file path/to/raw.bin [--base 0x1000]

use analysis_passes::{CfgBuilder, ConstantPropagation, PassManager, Structurer, TypeInference};
use arch_x86::{X86Disassembler, X86Lifter};
use arch_arm::{ArmDisassembler, ArmLifter};
use decompiler_core::{BasicBlock, DecodeError, Disassembler, Function, Lifter, TempAllocator};
use std::env;
use std::fs;
use std::process::ExitCode;

/// A small hand-assembled x86 function used when no input is given:
///
/// ```text
/// 0x1000:  mov  eax, 5
/// 0x1005:  add  eax, 3
/// 0x100a:  cmp  eax, 8
/// 0x100f:  jne  0x1018          ; not-equal path
/// 0x1011:  mov  ebx, 1          ; equal path
/// 0x1016:  jmp  0x101d
/// 0x1018:  mov  ebx, 0          ; not-equal path
/// 0x101d:  ret
/// ```
///
/// This exercises every instruction in Phase 1's target subset (MOV, ADD,
/// CMP, JMP, CALL is exercised separately below) plus the conditional-jump
/// pair that makes CMP's lifted flag Varnode actually meaningful.
const DEMO_BYTES: &[u8] = &[
    0xB8, 0x05, 0x00, 0x00, 0x00, // mov eax, 5
    0x05, 0x03, 0x00, 0x00, 0x00, // add eax, 3
    0x3D, 0x08, 0x00, 0x00, 0x00, // cmp eax, 8
    0x75, 0x07, // jne +7  -> 0x1018
    0xBB, 0x01, 0x00, 0x00, 0x00, // mov ebx, 1
    0xEB, 0x05, // jmp +5  -> 0x101d
    0xBB, 0x00, 0x00, 0x00, 0x00, // mov ebx, 0
    0xC3, // ret
];

/// A second tiny snippet demonstrating CALL specifically, since the demo
/// function above doesn't include one.
const CALL_DEMO_BYTES: &[u8] = &[
    0xE8, 0x0A, 0x00, 0x00, 0x00, // call +0xa
];

fn parse_hex(input: &str) -> Result<Vec<u8>, String> {
    let cleaned: String = input.chars().filter(|c| !c.is_whitespace()).collect();
    let cleaned = cleaned.strip_prefix("0x").unwrap_or(&cleaned);
    if cleaned.len() % 2 != 0 {
        return Err("hex string must have an even number of digits".to_string());
    }
    (0..cleaned.len())
        .step_by(2)
        .map(|i| {
            u8::from_str_radix(&cleaned[i..i + 2], 16)
                .map_err(|e| format!("invalid hex byte '{}': {e}", &cleaned[i..i + 2]))
        })
        .collect()
}

fn parse_base(input: &str) -> Result<u64, String> {
    let s = input.trim();
    if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        u64::from_str_radix(hex, 16).map_err(|e| e.to_string())
    } else {
        s.parse::<u64>().map_err(|e| e.to_string())
    }
}

/// Disassemble + lift every instruction in `bytes` (starting at `base`) into
/// a single `Function`, printing each step as it goes. Decoding stops at the
/// first `ret`, at an error, or when the buffer is exhausted.
fn process_function<D: Disassembler, L: Lifter<Instruction = D::Instruction>>(
    name: &str,
    bytes: &[u8],
    base: u64,
    disassembler: &D,
    lifter: &L,
) -> Function 
where <D as Disassembler>::Instruction: std::fmt::Display {
    println!("\n=== {name} (base 0x{base:x}, {} bytes) ===", bytes.len());

    let mut temps = TempAllocator::new();

    let mut function = Function::new(name, base);
    let mut block = BasicBlock::new(base);

    let mut offset = 0usize;
    loop {
        if offset >= bytes.len() {
            break;
        }
        let address = base + offset as u64;
        let remaining = &bytes[offset..];

        match disassembler.disassemble_one(remaining, address) {
            Ok(result) => {
                let raw = &remaining[..result.length];
                let hex: Vec<String> = raw.iter().map(|b| format!("{b:02x}")).collect();
                println!(
                    "  0x{address:04x}:  {:<24}  {}",
                    hex.join(" "),
                    result.instruction
                );

                let ops = lifter.lift(&result.instruction, address, result.length, &mut temps);
                for op in &ops {
                    println!("             \u{2514}\u{2500} {op}");
                }

                // Detect function end by checking for a Return op in the *lifted IR*,
                // not the decoded instruction's debug string. This is architecture-
                // agnostic: any `arch-*` backend that emits `Instruction::Return`
                // (as both x86 `RET` and ARM `RET` do) will stop the decode loop.
                let is_ret = ops.iter().any(|op| matches!(op, ir_pcode::Instruction::Return));
                block.push(address, ops);
                offset += result.length;

                if is_ret {
                    break;
                }
            }
            Err(DecodeError::UnexpectedEnd) => {
                println!("  0x{address:04x}:  <truncated instruction, stopping>");
                break;
            }
            Err(e) => {
                println!("  0x{address:04x}:  decode error: {e}");
                break;
            }
        }
    }

    function.blocks.push(block);
    function
}

fn print_summary(function: &Function) {
    let total_ops: usize =
        function.blocks.iter().flat_map(|b| &b.instructions).map(|li| li.ops.len()).sum();
    let total_machine_instrs: usize =
        function.blocks.iter().map(|b| b.instructions.len()).sum();
    println!(
        "  -- {total_machine_instrs} machine instruction(s) lifted to {total_ops} IR operation(s)"
    );
}

/// Phase 2: run the CFG-construction + constant-propagation pipeline over
/// `function` (mutating it in place), then print the resulting graph and a
/// pseudocode rendering. This is what turns the flat, single-block output
/// `process_function` produces into the real multi-block CFG the roadmap's
/// Phase 2 calls for.
fn run_phase2(function: &mut Function) {
    println!("\n  --- Phase 2-4: CFG, Dataflow, Type Inference, Structuring ---");
    let pipeline = PassManager::new()
        .with(CfgBuilder)
        .with(ConstantPropagation)
        .with(TypeInference)
        .with(Structurer);
    pipeline.run_verbose(function);

    for block in &function.blocks {
        println!(
            "  block 0x{:x}  (preds: {:?}, succs: {:?})",
            block.start_address, block.predecessors, block.successors
        );
        for li in &block.instructions {
            for op in &li.ops {
                println!("    0x{:x}:  {op}", li.address);
            }
        }
    }

    println!("\n  --- Phase 4: pseudocode (structured C) ---");
    for line in output_c::generate(&*function).lines() {
        println!("  {line}");
    }
}

fn run() -> Result<(), String> {
    let args: Vec<String> = env::args().skip(1).collect();

    let mut hex_arg: Option<String> = None;
    let mut file_arg: Option<String> = None;
    let mut base_arg: u64 = 0x1000;
    let mut arch_arg: String = "x86".to_string();

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--hex" => {
                i += 1;
                hex_arg = Some(args.get(i).ok_or("--hex requires a value")?.clone());
            }
            "--file" => {
                i += 1;
                file_arg = Some(args.get(i).ok_or("--file requires a value")?.clone());
            }
            "--base" => {
                i += 1;
                base_arg = parse_base(args.get(i).ok_or("--base requires a value")?)?;
            }
            "--arch" => {
                i += 1;
                arch_arg = args.get(i).ok_or("--arch requires a value (x86 or arm)")?.clone();
            }
            "--help" | "-h" => {
                println!(
                    "decompiler-cli - disassemble + lift bytes to IR, then run full pipeline\n\n\
                     Usage:\n  \
                     decompiler-cli                              run the built-in demo\n  \
                     decompiler-cli --hex \"B8 05 00 00 00 C3\"     lift a hex byte string\n  \
                     decompiler-cli --file bytes.bin              lift a raw binary file\n  \
                     decompiler-cli [...] --base 0x401000         set the load address (default 0x1000)\n  \
                     decompiler-cli [...] --arch arm              set architecture (x86 or arm, default x86)"
                );
                return Ok(());
            }
            other => return Err(format!("unrecognized argument: {other}")),
        }
        i += 1;
    }

    if hex_arg.is_some() && file_arg.is_some() {
        return Err("--hex and --file are mutually exclusive; pass only one".to_string());
    }

    if hex_arg.is_none() && file_arg.is_none() {
        if arch_arg == "arm" {
            // ARM mock demo bytes
            // MOV X0, #5
            // ADD X0, X0, #3
            // CMP X0, #8
            // B.NE +8
            // MOV X1, #1
            // B +8
            // MOV X1, #0
            // RET
            let arm_demo: &[u8] = &[
                0xa0, 0x00, 0x00, 0x10, // mov x0, 5
                0xa0, 0x08, 0x00, 0x12, // add x0, x0, 3
                0x00, 0x20, 0x00, 0x13, // cmp x0, 8 (wait, our mock: lhs=0, imm=8) -> 0x13 << 24 | (8 << 5) | 0
                0x08, 0x00, 0x00, 0x17, // b.ne +8 -> 0x17 << 24 | 8
                0x21, 0x00, 0x00, 0x10, // mov x1, 1 -> 0x10 << 24 | (1 << 5) | 1
                0x08, 0x00, 0x00, 0x14, // b +8 -> 0x14 << 24 | 8
                0x01, 0x00, 0x00, 0x10, // mov x1, 0 -> 0x10 << 24 | (0 << 5) | 1
                0x00, 0x00, 0x00, 0x18, // ret
            ];
            let mut f1 = process_function("demo_arm", arm_demo, 0x1000, &ArmDisassembler::new(), &ArmLifter::new());
            print_summary(&f1);
            run_phase2(&mut f1);
            return Ok(());
        }
        
        let mut f1 = process_function("demo_function (mov/add/cmp/jne/jmp/ret)", DEMO_BYTES, 0x1000, &X86Disassembler::new(), &X86Lifter::new());
        print_summary(&f1);
        run_phase2(&mut f1);
        let mut f2 = process_function("demo_call (call)", CALL_DEMO_BYTES, 0x2000, &X86Disassembler::new(), &X86Lifter::new());
        print_summary(&f2);
        run_phase2(&mut f2);
        return Ok(());
    }

    let bytes = if let Some(hex) = hex_arg {
        parse_hex(&hex)?
    } else {
        let path = file_arg.unwrap();
        fs::read(&path).map_err(|e| format!("failed to read {path}: {e}"))?
    };

    if arch_arg == "arm" {
        let mut function = process_function("input", &bytes, base_arg, &ArmDisassembler::new(), &ArmLifter::new());
        print_summary(&function);
        run_phase2(&mut function);
    } else {
        let mut function = process_function("input", &bytes, base_arg, &X86Disassembler::new(), &X86Lifter::new());
        print_summary(&function);
        run_phase2(&mut function);
    }
    Ok(())
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}
