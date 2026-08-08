// lifter.rs — the "arch-x86" piece: turns decoded x86-64 instructions into
// our small IR. In the full design this would implement a `Lifter` trait
// from `decompiler-core`; here it's a plain module to keep scope small.

use crate::ir::*;
use iced_x86::{Decoder, DecoderOptions, Formatter, Instruction, Mnemonic, NasmFormatter, OpKind, Register};
use std::collections::HashMap;

fn reg_name(r: Register) -> String {
    format!("{:?}", r).to_lowercase()
}

// --- calling convention (System V AMD64, the only one relevant here since
// this project only reads ELF -- Linux's ABI, not Windows x64) ---------

/// Integer/pointer argument registers in order, per the SysV AMD64 ABI.
/// (Floating-point args go in xmm0-xmm7, which this IR doesn't model at
/// all -- see README limitations -- so calls/functions that are actually
/// variadic-with-floats or float-only params aren't recovered correctly.)
pub const SYSV_INT_ARGS: [&str; 6] = ["rdi", "rsi", "rdx", "rcx", "r8", "r9"];

/// Canonicalize any width-alias of an x86-64 GPR to its 64-bit "family"
/// name (e.g. "edi"/"di"/"dil" -> "rdi"). A `mov edi, ...` still writes
/// the same architectural register `rdi` occupies (zero-extended), so
/// argument-register bookkeeping needs to match on the family, not the
/// literal string the disassembler happened to print for that access
/// width.
pub fn reg_family(name: &str) -> String {
    let fam = match name {
        "rax" | "eax" | "ax" | "al" | "ah" => "rax",
        "rbx" | "ebx" | "bx" | "bl" | "bh" => "rbx",
        "rcx" | "ecx" | "cx" | "cl" | "ch" => "rcx",
        "rdx" | "edx" | "dx" | "dl" | "dh" => "rdx",
        "rsi" | "esi" | "si" | "sil" => "rsi",
        "rdi" | "edi" | "di" | "dil" => "rdi",
        "rbp" | "ebp" | "bp" | "bpl" => "rbp",
        "rsp" | "esp" | "sp" | "spl" => "rsp",
        "r8" | "r8d" | "r8w" | "r8l" => "r8",
        "r9" | "r9d" | "r9w" | "r9l" => "r9",
        "r10" | "r10d" | "r10w" | "r10l" => "r10",
        "r11" | "r11d" | "r11w" | "r11l" => "r11",
        "r12" | "r12d" | "r12w" | "r12l" => "r12",
        "r13" | "r13d" | "r13w" | "r13l" => "r13",
        "r14" | "r14d" | "r14w" | "r14l" => "r14",
        "r15" | "r15d" | "r15w" | "r15l" => "r15",
        other => other,
    };
    fam.to_string()
}

/// If `name` is (some width of) a SysV integer argument register, its
/// position in the calling-convention order (rdi=0, rsi=1, ...).
pub fn sysv_arg_index(name: &str) -> Option<usize> {
    let fam = reg_family(name);
    SYSV_INT_ARGS.iter().position(|&r| r == fam)
}

fn val_of_op(insn: &Instruction, idx: u32) -> Value {
    match insn.op_kind(idx) {
        OpKind::Register => Value::Reg(reg_name(insn.op_register(idx))),
        OpKind::Immediate8
        | OpKind::Immediate16
        | OpKind::Immediate32
        | OpKind::Immediate64
        | OpKind::Immediate8to16
        | OpKind::Immediate8to32
        | OpKind::Immediate8to64
        | OpKind::Immediate32to64 => Value::Imm(insn.immediate(idx) as i64),
        OpKind::Memory => {
            let base = insn.memory_base();
            let index = insn.memory_index();
            let scale = insn.memory_index_scale();
            let disp = insn.memory_displacement64() as i64;
            let mut s = String::from("[");
            let mut wrote = false;
            if base != Register::None {
                s.push_str(&reg_name(base));
                wrote = true;
            }
            if index != Register::None {
                if wrote {
                    s.push('+');
                }
                s.push_str(&format!("{}*{}", reg_name(index), scale));
                wrote = true;
            }
            if disp != 0 || !wrote {
                if disp < 0 {
                    s.push_str(&format!("-0x{:x}", -disp));
                } else if wrote {
                    s.push_str(&format!("+0x{:x}", disp));
                } else {
                    s.push_str(&format!("0x{:x}", disp));
                }
            }
            s.push(']');
            Value::Mem(s)
        }
        _ => Value::Imm(0),
    }
}

fn cond_for_mnemonic(m: Mnemonic) -> Option<Cond> {
    use Mnemonic::*;
    Some(match m {
        Je => Cond::Eq,
        Jne => Cond::Ne,
        Jl => Cond::Lt,
        Jle => Cond::Le,
        Jg => Cond::Gt,
        Jge => Cond::Ge,
        Jb => Cond::Below,
        Jbe => Cond::BelowEq,
        Ja => Cond::Above,
        Jae => Cond::AboveEq,
        // Sign-flag jumps: no dedicated Cond variant, but for the
        // overwhelmingly common case (a preceding `cmp reg/mem, 0` or
        // `test reg, reg`), SF==0 / SF==1 line up with signed >=0 / <0.
        Jns => Cond::Ge,
        Js => Cond::Lt,
        _ => return None,
    })
}

/// Turn a symbol name into something safe to print as a C-style call
/// target (same rule main.rs uses for function names).
fn sanitize_call_name(n: &str) -> String {
    n.chars().map(|c| if c.is_ascii_alphanumeric() || c == '_' { c } else { '_' }).collect()
}

/// Lift one decoded instruction into our IR, mirroring how a real lifter
/// would break a single machine instruction into several p-code ops.
/// `symbols` maps known function addresses to their names (from the
/// object file's symbol table) so `call` targets can be rendered by name
/// instead of a bare `sub_<addr>` when we know it. `reloc_symbols` maps
/// call-site addresses (address of the call's 4-byte displacement field)
/// to names, for unlinked object files where the displacement itself
/// isn't a real address yet -- see `resolve_reloc_call_targets` in
/// main.rs. When both apply, the relocation-based name wins, since it's
/// authoritative for `.o` files where `near_branch_target` is bogus.
pub fn lift(insn: &Instruction, fmt_out: &mut String, symbols: &HashMap<u64, String>, reloc_symbols: &HashMap<u64, String>) -> LiftedInsn {
    let mut formatter = NasmFormatter::new();
    fmt_out.clear();
    formatter.format(insn, fmt_out);

    let mut ops: Vec<Instr> = Vec::new();
    let mut targets: Vec<u64> = Vec::new();
    let mut is_block_end = false;
    let mut falls_through = true;

    match insn.mnemonic() {
        Mnemonic::Mov | Mnemonic::Movzx | Mnemonic::Movsxd | Mnemonic::Movsx | Mnemonic::Lea => {
            let dst = val_of_op(insn, 0);
            let src = val_of_op(insn, 1);
            ops.push(Instr::Copy { dst, src });
        }
        Mnemonic::Add | Mnemonic::Sub | Mnemonic::And | Mnemonic::Or | Mnemonic::Xor
        | Mnemonic::Imul | Mnemonic::Shl | Mnemonic::Shr => {
            let dst = val_of_op(insn, 0);
            let lhs = dst.clone();
            let rhs = if insn.op_count() >= 2 {
                val_of_op(insn, 1)
            } else {
                Value::Imm(0)
            };
            let op = match insn.mnemonic() {
                Mnemonic::Add => BinOp::Add,
                Mnemonic::Sub => BinOp::Sub,
                Mnemonic::And => BinOp::And,
                Mnemonic::Or => BinOp::Or,
                Mnemonic::Xor => BinOp::Xor,
                Mnemonic::Imul => BinOp::Mul,
                Mnemonic::Shl => BinOp::Shl,
                Mnemonic::Shr => BinOp::Shr,
                _ => unreachable!(),
            };
            ops.push(Instr::Bin { dst, op, lhs, rhs });
        }
        Mnemonic::Inc | Mnemonic::Dec => {
            let dst = val_of_op(insn, 0);
            let lhs = dst.clone();
            let op = if matches!(insn.mnemonic(), Mnemonic::Inc) { BinOp::Add } else { BinOp::Sub };
            ops.push(Instr::Bin { dst, op, lhs, rhs: Value::Imm(1) });
        }
        Mnemonic::Neg | Mnemonic::Not => {
            let dst = val_of_op(insn, 0);
            let src = dst.clone();
            let op = if matches!(insn.mnemonic(), Mnemonic::Neg) { UnOp::Neg } else { UnOp::Not };
            ops.push(Instr::Un { dst, op, src });
        }
        Mnemonic::Cmp | Mnemonic::Test => {
            let lhs = val_of_op(insn, 0);
            let rhs = val_of_op(insn, 1);
            // real cond filled in when the following Jcc is lifted; we
            // store a placeholder Eq here, corrected by the CBranch step
            // in cfg.rs which looks back at the preceding Cmp.
            ops.push(Instr::Cmp { lhs, rhs, cond: Cond::Eq });
        }
        Mnemonic::Jmp => {
            let t = insn.near_branch_target();
            targets.push(t);
            ops.push(Instr::Branch { target: t });
            is_block_end = true;
            falls_through = false;
        }
        m if cond_for_mnemonic(m).is_some() => {
            let t = insn.near_branch_target();
            let cond = cond_for_mnemonic(m).unwrap();
            targets.push(t);
            ops.push(Instr::CBranch { target: t });
            // stash the real condition as an Unknown marker consumed by cfg.rs
            ops.push(Instr::Unknown { text: format!("__cond__{:?}", cond) });
            is_block_end = true;
            falls_through = true; // conditional: also falls through
        }
        Mnemonic::Call => {
            let target = if insn.op0_kind() == OpKind::NearBranch64 || insn.op0_kind() == OpKind::NearBranch32 {
                // Displacement field is the last 4 bytes of the (5-byte)
                // near-call encoding; a relocation entry there (if any)
                // is authoritative over the raw, possibly-unpatched
                // near_branch_target -- see resolve_reloc_call_targets.
                let field_addr = insn.ip() + insn.len() as u64 - 4;
                if let Some(name) = reloc_symbols.get(&field_addr) {
                    sanitize_call_name(name)
                } else {
                    let addr = insn.near_branch_target();
                    match symbols.get(&addr) {
                        Some(name) => sanitize_call_name(name),
                        None => format!("sub_{:x}", addr),
                    }
                }
            } else {
                "indirect_call".to_string()
            };
            // Real arg count/values get filled in by `resolve_call_args`
            // once the whole function's instructions are lifted -- a
            // single instruction can't see what was moved into rdi/rsi/...
            // earlier in the block by itself, so start empty rather than
            // assuming every call passes the same fixed 3 args.
            ops.push(Instr::Call { target, args: Vec::new() });
        }
        Mnemonic::Ret | Mnemonic::Retf => {
            ops.push(Instr::Ret { val: Some(Value::Reg("rax".into())) });
            is_block_end = true;
            falls_through = false;
        }
        Mnemonic::Push | Mnemonic::Pop | Mnemonic::Nop | Mnemonic::Endbr64 | Mnemonic::Leave => {
            ops.push(Instr::Nop);
        }
        _ => {
            ops.push(Instr::Unknown { text: fmt_out.clone() });
        }
    }

    LiftedInsn {
        addr: insn.ip(),
        len: insn.len() as u32,
        asm_text: fmt_out.clone(),
        ops,
        is_block_end,
        targets,
        falls_through,
    }
}

/// Second pass over a function's flat, address-ordered instruction list:
/// works out how many SysV integer/pointer argument registers (rdi, rsi,
/// rdx, rcx, r8, r9) were actually set up before each `call`, and
/// substitutes in whatever expression was last written to each one --
/// instead of the fixed `(rdi, rsi, rdx)` triple every call used to get
/// regardless of how many args (if any) it actually passed.
///
/// This is a simple forward scan, not full dataflow: for each arg
/// register it remembers the last value copied/computed into it since
/// the previous `call` (or function start), and at each `call` credits
/// the *contiguous* prefix (rdi, then rdi+rsi, ...) that was actually
/// set -- mirroring how the ABI assigns args in order, and stopping at
/// the first untouched slot so e.g. a stale rdx from earlier in the
/// function doesn't get mistaken for a 3rd argument when only rdi/rsi
/// were actually (re)set for this call. State resets after every call,
/// since a callee is free to clobber all of rdi..r9 and each call site
/// re-establishes its own args from scratch.
///
/// Good enough for straight-line / simple-branching -O0 code (this
/// project's stated scope); a register set once and reused across a
/// loop back-edge, args built up in an unusual order, or args passed on
/// the stack (7th+ integer arg, or anything once a struct/float is
/// involved) can still be missed -- see README limitations.
fn resolve_call_args(instrs: &mut [LiftedInsn]) {
    let mut arg_val: HashMap<usize, Value> = HashMap::new();

    for ins in instrs.iter_mut() {
        for op in ins.ops.iter() {
            let dst = match op {
                Instr::Copy { dst, .. } | Instr::Bin { dst, .. } | Instr::Un { dst, .. } => Some(dst),
                _ => None,
            };
            if let Some(Value::Reg(r)) = dst {
                if let Some(idx) = sysv_arg_index(r) {
                    // Copy (mov/lea) hands us the real source expression;
                    // `xor reg, reg` is the standard zero-idiom (a very
                    // common way to pass a literal 0 arg, e.g. the flags
                    // arg in an `open(path, O_RDONLY, 0)`-style call) so
                    // resolve it to an actual 0 instead of just naming
                    // the register; any other arithmetic op means the
                    // register now holds a computed value we don't try
                    // to reconstruct, so fall back to naming it.
                    let src = match op {
                        Instr::Copy { src, .. } => src.clone(),
                        Instr::Bin { op: BinOp::Xor, lhs, rhs, .. } if lhs == rhs => Value::Imm(0),
                        _ => Value::Reg(r.clone()),
                    };
                    arg_val.insert(idx, src);
                }
            }
        }
        for op in ins.ops.iter_mut() {
            if let Instr::Call { args, .. } = op {
                let mut real_args = Vec::new();
                for i in 0..SYSV_INT_ARGS.len() {
                    match arg_val.get(&i) {
                        Some(v) => real_args.push(v.clone()),
                        None => break, // contiguous prefix only
                    }
                }
                *args = real_args;
                arg_val.clear();
            }
        }
    }
}

/// Decode + lift every instruction in `code` starting at virtual address `base`.
pub fn lift_region(code: &[u8], base: u64, symbols: &HashMap<u64, String>, reloc_symbols: &HashMap<u64, String>) -> Vec<LiftedInsn> {
    let mut decoder = Decoder::with_ip(64, code, base, DecoderOptions::NONE);
    let mut insn = Instruction::default();
    let mut out = Vec::new();
    let mut scratch = String::new();
    while decoder.can_decode() {
        decoder.decode_out(&mut insn);
        if insn.is_invalid() {
            break;
        }
        out.push(lift(&insn, &mut scratch, symbols, reloc_symbols));
    }
    resolve_call_args(&mut out);
    out
}