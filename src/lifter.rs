// lifter.rs — the "arch-x86" piece: turns decoded x86-64 instructions into
// our small IR. In the full design this would implement a `Lifter` trait
// from `decompiler-core`; here it's a plain module to keep scope small.

use crate::ir::*;
use iced_x86::{Decoder, DecoderOptions, Formatter, Instruction, Mnemonic, NasmFormatter, OpKind, Register};
use std::collections::HashMap;

fn reg_name(r: Register) -> String {
    format!("{:?}", r).to_lowercase()
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
            ops.push(Instr::Call { target, args: vec![
                Value::Reg("rdi".into()), Value::Reg("rsi".into()), Value::Reg("rdx".into()),
            ]});
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
    out
}