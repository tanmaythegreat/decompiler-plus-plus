// lifter.rs — x86-64 -> IR.
//
// Changes vs the original beyond the obvious widening of mnemonic
// coverage:
//
//  * `lea` no longer aliases `mov`. The old code lifted both through the
//    same arm, so `lea rax, [rip+0x2004]` became `rax = *[rip+0x402004]`
//    — a load of the string instead of its address. Every `printf`
//    format-string argument in the sample output was wrong because of it.
//  * RIP-relative operands are resolved to their absolute target.
//    `memory_displacement64()` already folds RIP in for these, so the old
//    code printed `[rip+0x402004]`, i.e. base *plus* an address that had
//    RIP added in twice over.
//  * `cmp` and `test` are no longer the same thing. Flags are modelled by
//    what set them, so `test eax,eax; je` becomes `eax == 0` instead of
//    the old `eax == eax`.
//  * sub-register writes carry their width, so `movzx eax, al` is a cast
//    rather than a spurious `eax = al` between unrelated names.

use crate::ir::*;
use iced_x86::{
    Decoder, DecoderOptions, Formatter, Instruction, Mnemonic, NasmFormatter, OpKind, Register,
};
use std::collections::HashMap;

pub const SYSV_INT_ARGS: [&str; 6] = ["rdi", "rsi", "rdx", "rcx", "r8", "r9"];
#[allow(dead_code)]
pub const SYSV_FLOAT_ARGS: [&str; 8] =
    ["xmm0", "xmm1", "xmm2", "xmm3", "xmm4", "xmm5", "xmm6", "xmm7"];

/// Registers a callee may destroy under SysV — a value in one of these
/// cannot be assumed to survive a call.
pub const CALLER_SAVED: [&str; 9] =
    ["rax", "rcx", "rdx", "rsi", "rdi", "r8", "r9", "r10", "r11"];

/// Canonicalise any width-alias of a GPR to its 64-bit family, and
/// report the width actually accessed.
///
/// The original hand-wrote this as a ~20-arm string match, which was
/// both incomplete (`r10b`, `r11b`... are the names iced prints for the
/// byte forms of r8-r15, and only `r8l`-style spellings were listed) and
/// silently wrong for `ah`/`bh`/`ch`/`dh`, which alias bits 8..16 rather
/// than the low byte. iced already knows all of this.
pub fn reg_ref(r: Register) -> RegRef {
    if r == Register::None {
        return RegRef::new("none", 8);
    }
    let size = r.size() as u8;
    let full = if r.is_gpr() {
        format!("{:?}", r.full_register()).to_lowercase()
    } else if r.is_xmm() || r.is_ymm() || r.is_zmm() {
        // model all vector widths as the xmm name; we only ever touch
        // the low lane for scalar float code
        let n = r.number();
        format!("xmm{}", n)
    } else {
        format!("{:?}", r).to_lowercase()
    };
    let high8 = matches!(r, Register::AH | Register::BH | Register::CH | Register::DH);
    RegRef { full, size, high8 }
}

pub fn sysv_arg_index(full: &str) -> Option<usize> {
    SYSV_INT_ARGS.iter().position(|&r| r == full)
}

#[allow(dead_code)]
pub fn sysv_float_arg_index(full: &str) -> Option<usize> {
    SYSV_FLOAT_ARGS.iter().position(|&r| r == full)
}

fn mem_op(insn: &Instruction) -> MemOp {
    let msize = insn.memory_size();
    let size = msize.size() as u8;
    let signed = msize.is_signed();

    if insn.is_ip_rel_memory_operand() {
        // RIP-relative: iced hands back the fully-resolved target, so
        // the base register must be dropped rather than printed as well.
        return MemOp {
            base: None,
            index: None,
            scale: 1,
            disp: insn.ip_rel_memory_address() as i64,
            size: if size == 0 { 8 } else { size },
            signed,
            rip_abs: Some(insn.ip_rel_memory_address()),
        };
    }

    let base = insn.memory_base();
    let index = insn.memory_index();
    // memory_displacement64 is unsigned; sign-extend it according to how
    // many displacement bytes the encoding actually carried, otherwise
    // `[rbp-4]` reads back as `[rbp+0xfffffffffffffffc]`.
    let raw = insn.memory_displacement64();
    let disp = match insn.memory_displ_size() {
        1 => raw as u8 as i8 as i64,
        2 => raw as u16 as i16 as i64,
        4 => raw as u32 as i32 as i64,
        _ => raw as i64,
    };

    MemOp {
        base: if base == Register::None { None } else { Some(reg_ref(base)) },
        index: if index == Register::None { None } else { Some(reg_ref(index)) },
        scale: insn.memory_index_scale() as u8,
        disp,
        size: if size == 0 { 8 } else { size },
        signed,
        rip_abs: None,
    }
}

/// Width in bytes of operand `idx`, used to type immediates and to
/// decide how wide a sign-extension is.
fn op_width(insn: &Instruction, idx: u32) -> u8 {
    match insn.op_kind(idx) {
        OpKind::Register => insn.op_register(idx).size() as u8,
        OpKind::Memory => {
            let s = insn.memory_size().size() as u8;
            if s == 0 {
                8
            } else {
                s
            }
        }
        _ => 8,
    }
}

fn operand(insn: &Instruction, idx: u32) -> Expr {
    match insn.op_kind(idx) {
        OpKind::Register => Expr::Reg(reg_ref(insn.op_register(idx))),
        OpKind::Memory => {
            // fs:/gs: relative operands are thread-local storage, not a
            // normal address — the stack canary lives at fs:0x28. Rendering
            // them as `*(long *)40` is actively misleading, so name the
            // access instead.
            match insn.segment_prefix() {
                Register::FS | Register::GS => {
                    let seg = if insn.segment_prefix() == Register::FS { "fs" } else { "gs" };
                    let sz = insn.memory_size().size();
                    let suffix = match sz {
                        1 => "byte",
                        2 => "word",
                        4 => "dword",
                        _ => "qword",
                    };
                    Expr::Call {
                        name: format!("__read{}{}", seg, suffix),
                        args: vec![Expr::Const(insn.memory_displacement64() as i64)],
                        indirect: None,
                    }
                }
                _ => Expr::Mem(mem_op(insn)),
            }
        }
        OpKind::NearBranch16 | OpKind::NearBranch32 | OpKind::NearBranch64 => {
            Expr::Const(insn.near_branch_target() as i64)
        }
        OpKind::Immediate8 => Expr::Const(insn.immediate8() as i8 as i64),
        OpKind::Immediate16 => Expr::Const(insn.immediate16() as i16 as i64),
        OpKind::Immediate32 => Expr::Const(insn.immediate32() as i32 as i64),
        OpKind::Immediate64 => Expr::Const(insn.immediate64() as i64),
        OpKind::Immediate8to16 => Expr::Const(insn.immediate8to16() as i64),
        OpKind::Immediate8to32 => Expr::Const(insn.immediate8to32() as i64),
        OpKind::Immediate8to64 => Expr::Const(insn.immediate8to64()),
        OpKind::Immediate32to64 => Expr::Const(insn.immediate32to64()),
        _ => Expr::Unknown(format!("op{}", idx)),
    }
}

/// Map the conditional suffix shared by `jcc`, `setcc` and `cmovcc`.
/// One table instead of three, keyed off iced's canonical spelling.
fn cond_of(m: Mnemonic) -> Option<CondCode> {
    let name = format!("{:?}", m);
    let suffix = if let Some(s) = name.strip_prefix("Cmov") {
        s
    } else if let Some(s) = name.strip_prefix("Set") {
        s
    } else if name.len() > 1 && name.starts_with('J') {
        &name[1..]
    } else {
        return None;
    };
    use CondCode::*;
    Some(match suffix.to_lowercase().as_str() {
        "e" | "z" => E,
        "ne" | "nz" => Ne,
        "l" | "nge" => L,
        "le" | "ng" => Le,
        "g" | "nle" => G,
        "ge" | "nl" => Ge,
        "b" | "c" | "nae" => B,
        "be" | "na" => Be,
        "a" | "nbe" => A,
        "ae" | "nb" | "nc" => Ae,
        "s" => S,
        "ns" => Ns,
        "p" | "pe" => P,
        "np" | "po" => Np,
        "o" => O,
        "no" => No,
        _ => return None,
    })
}

pub fn sanitize_name(n: &str) -> String {
    let s: String = n
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '_' { c } else { '_' })
        .collect();
    if s.is_empty() || s.chars().next().map(|c| c.is_ascii_digit()).unwrap_or(false) {
        format!("_{}", s)
    } else {
        s
    }
}

pub struct LiftCtx<'a> {
    /// address -> function name (symbols, PLT stubs)
    pub symbols: &'a HashMap<u64, String>,
    /// call-site displacement field address -> callee name (unlinked .o)
    pub reloc_symbols: &'a HashMap<u64, String>,
}

/// Lift one decoded instruction.
pub fn lift(insn: &Instruction, fmt_out: &mut String, ctx: &LiftCtx) -> LiftedInsn {
    let mut formatter = NasmFormatter::new();
    fmt_out.clear();
    formatter.format(insn, fmt_out);

    let mut stmts: Vec<Stmt> = Vec::new();
    let mut targets: Vec<u64> = Vec::new();
    let mut flags: Option<FlagSrc> = None;
    let mut cond: Option<CondCode> = None;
    let mut is_block_end = false;
    let mut falls_through = true;
    let mut is_call = false;
    let mut is_frame_setup = false;

    let m = insn.mnemonic();
    let rsp = || Expr::Reg(RegRef::new("rsp", 8));

    match m {
        // ---- data movement -------------------------------------------
        Mnemonic::Mov => {
            stmts.push(Stmt::Assign { dst: operand(insn, 0), src: operand(insn, 1) });
        }
        Mnemonic::Movzx | Mnemonic::Movsx | Mnemonic::Movsxd => {
            let signed = !matches!(m, Mnemonic::Movzx);
            let src_w = op_width(insn, 1);
            let dst_w = op_width(insn, 0);
            // A widening move from a 32-bit source needs one cast, not
            // two: `(long)(int)x` is just `(long)x`. Narrower sources keep
            // the inner cast because it carries the signedness of the
            // 8/16-bit value being extended.
            let inner = operand(insn, 1);
            let src = if src_w >= 4 {
                inner
            } else {
                Expr::cast(Type::from_width(src_w, signed), inner)
            };
            stmts.push(Stmt::Assign {
                dst: operand(insn, 0),
                src: Expr::cast(Type::from_width(dst_w, signed), src),
            });
        }
        // Sign-extend accumulator: cbw/cwde/cdqe widen rax in place;
        // cwd/cdq/cqo fill rdx with the sign of rax. The original lifted
        // none of these, so every `long` accumulation printed
        // `/* unhandled: cdqe */` and silently lost the conversion.
        Mnemonic::Cbw => stmts.push(sign_extend_acc("rax", 1, 2)),
        Mnemonic::Cwde => stmts.push(sign_extend_acc("rax", 2, 4)),
        Mnemonic::Cdqe => stmts.push(sign_extend_acc("rax", 4, 8)),
        Mnemonic::Cwd | Mnemonic::Cdq | Mnemonic::Cqo => {
            let w: u8 = match m {
                Mnemonic::Cwd => 2,
                Mnemonic::Cdq => 4,
                _ => 8,
            };
            stmts.push(Stmt::Assign {
                dst: Expr::Reg(RegRef::new("rdx", w)),
                src: Expr::bin(
                    BinOp::Sar,
                    Expr::Reg(RegRef::new("rax", w)),
                    Expr::Const((w as i64 * 8) - 1),
                ),
            });
        }
        Mnemonic::Lea => {
            // The address, not the contents. This is the single biggest
            // correctness fix in the file.
            let addr = match operand(insn, 1) {
                Expr::Mem(mo) => Expr::AddrOf(Box::new(Expr::Mem(mo))),
                other => other,
            };
            stmts.push(Stmt::Assign { dst: operand(insn, 0), src: addr });
        }
        Mnemonic::Xchg => {
            let a = operand(insn, 0);
            let b = operand(insn, 1);
            let tmp = Expr::Reg(RegRef::new("__tmp", 8));
            stmts.push(Stmt::Assign { dst: tmp.clone(), src: a.clone() });
            stmts.push(Stmt::Assign { dst: a, src: b.clone() });
            stmts.push(Stmt::Assign { dst: b, src: tmp });
        }

        // ---- stack ----------------------------------------------------
        Mnemonic::Push => {
            let src = operand(insn, 0);
            // `push rbp` / `push rbx` etc. at function entry is frame
            // bookkeeping, not user code. Marked here, filtered later.
            if let Expr::Reg(r) = &src {
                if matches!(r.full.as_str(), "rbp" | "rbx" | "r12" | "r13" | "r14" | "r15") {
                    is_frame_setup = true;
                }
            }
            stmts.push(Stmt::Assign {
                dst: rsp(),
                src: Expr::bin(BinOp::Sub, rsp(), Expr::Const(8)),
            });
            stmts.push(Stmt::Assign {
                dst: Expr::Mem(MemOp {
                    base: Some(RegRef::new("rsp", 8)),
                    index: None,
                    scale: 1,
                    disp: 0,
                    size: 8,
                    signed: false,
                    rip_abs: None,
                }),
                src,
            });
        }
        Mnemonic::Pop => {
            let dst = operand(insn, 0);
            if let Expr::Reg(r) = &dst {
                if matches!(r.full.as_str(), "rbp" | "rbx" | "r12" | "r13" | "r14" | "r15") {
                    is_frame_setup = true;
                }
            }
            stmts.push(Stmt::Assign {
                dst,
                src: Expr::Mem(MemOp {
                    base: Some(RegRef::new("rsp", 8)),
                    index: None,
                    scale: 1,
                    disp: 0,
                    size: 8,
                    signed: false,
                    rip_abs: None,
                }),
            });
            stmts.push(Stmt::Assign {
                dst: rsp(),
                src: Expr::bin(BinOp::Add, rsp(), Expr::Const(8)),
            });
        }
        Mnemonic::Leave => {
            is_frame_setup = true;
            stmts.push(Stmt::Assign { dst: rsp(), src: Expr::Reg(RegRef::new("rbp", 8)) });
            stmts.push(Stmt::Assign {
                dst: Expr::Reg(RegRef::new("rbp", 8)),
                src: Expr::Mem(MemOp {
                    base: Some(RegRef::new("rsp", 8)),
                    index: None,
                    scale: 1,
                    disp: 0,
                    size: 8,
                    signed: false,
                    rip_abs: None,
                }),
            });
        }

        // ---- two-operand arithmetic / logic ---------------------------
        Mnemonic::Add
        | Mnemonic::Sub
        | Mnemonic::And
        | Mnemonic::Or
        | Mnemonic::Xor
        | Mnemonic::Shl
        | Mnemonic::Sal
        | Mnemonic::Shr
        | Mnemonic::Sar
        | Mnemonic::Rol
        | Mnemonic::Ror => {
            let dst = operand(insn, 0);
            let rhs = if insn.op_count() >= 2 { operand(insn, 1) } else { Expr::Const(1) };
            let op = match m {
                Mnemonic::Add => BinOp::Add,
                Mnemonic::Sub => BinOp::Sub,
                Mnemonic::And => BinOp::And,
                Mnemonic::Or => BinOp::Or,
                Mnemonic::Xor => BinOp::Xor,
                Mnemonic::Shl | Mnemonic::Sal => BinOp::Shl,
                Mnemonic::Shr => BinOp::Shr,
                Mnemonic::Sar => BinOp::Sar,
                Mnemonic::Rol | Mnemonic::Ror => BinOp::Or, // approximated
                _ => unreachable!(),
            };
            // `xor r, r` and `sub r, r` are the canonical zero idioms.
            let src = if matches!(m, Mnemonic::Xor | Mnemonic::Sub) && dst == rhs {
                Expr::Const(0)
            } else {
                Expr::bin(op, dst.clone(), rhs)
            };
            stmts.push(Stmt::Assign { dst: dst.clone(), src });
            flags = Some(FlagSrc::Logic(dst));
        }
        Mnemonic::Adc | Mnemonic::Sbb => {
            let dst = operand(insn, 0);
            let rhs = operand(insn, 1);
            let op = if matches!(m, Mnemonic::Adc) { BinOp::Add } else { BinOp::Sub };
            stmts.push(Stmt::Assign {
                dst: dst.clone(),
                src: Expr::bin(op, Expr::bin(op, dst.clone(), rhs), Expr::Lit("CF".into())),
            });
            flags = Some(FlagSrc::Logic(dst));
        }
        Mnemonic::Inc | Mnemonic::Dec => {
            let dst = operand(insn, 0);
            let op = if matches!(m, Mnemonic::Inc) { BinOp::Add } else { BinOp::Sub };
            stmts.push(Stmt::Assign {
                dst: dst.clone(),
                src: Expr::bin(op, dst.clone(), Expr::Const(1)),
            });
            flags = Some(FlagSrc::Logic(dst));
        }
        Mnemonic::Neg | Mnemonic::Not => {
            let dst = operand(insn, 0);
            let op = if matches!(m, Mnemonic::Neg) { UnOp::Neg } else { UnOp::Not };
            stmts.push(Stmt::Assign { dst: dst.clone(), src: Expr::un(op, dst.clone()) });
            if matches!(m, Mnemonic::Neg) {
                flags = Some(FlagSrc::Logic(dst));
            }
        }

        // ---- multiply / divide ----------------------------------------
        Mnemonic::Imul => {
            // three encodings: imul r/m (rdx:rax), imul r, r/m,
            // imul r, r/m, imm. The original assumed two operands with
            // dst also being lhs, which mangles the three-operand form.
            match insn.op_count() {
                1 => {
                    let src = operand(insn, 0);
                    stmts.push(Stmt::Assign {
                        dst: Expr::Reg(RegRef::new("rax", 8)),
                        src: Expr::bin(BinOp::Mul, Expr::Reg(RegRef::new("rax", 8)), src),
                    });
                }
                2 => {
                    let dst = operand(insn, 0);
                    let rhs = operand(insn, 1);
                    stmts.push(Stmt::Assign {
                        dst: dst.clone(),
                        src: Expr::bin(BinOp::Mul, dst, rhs),
                    });
                }
                _ => {
                    let dst = operand(insn, 0);
                    stmts.push(Stmt::Assign {
                        dst,
                        src: Expr::bin(BinOp::Mul, operand(insn, 1), operand(insn, 2)),
                    });
                }
            }
        }
        Mnemonic::Mul => {
            let src = operand(insn, 0);
            stmts.push(Stmt::Assign {
                dst: Expr::Reg(RegRef::new("rax", 8)),
                src: Expr::bin(
                    BinOp::Mul,
                    Expr::cast(Type::Int { bits: 64, signed: false }, Expr::Reg(RegRef::new("rax", 8))),
                    src,
                ),
            });
        }
        Mnemonic::Idiv | Mnemonic::Div => {
            let src = operand(insn, 0);
            let w = op_width(insn, 0);
            let signed = matches!(m, Mnemonic::Idiv);
            let acc = Expr::Reg(RegRef::new("rax", w));
            let (dop, rop) = if signed { (BinOp::Div, BinOp::Rem) } else { (BinOp::UDiv, BinOp::URem) };
            stmts.push(Stmt::Assign {
                dst: Expr::Reg(RegRef::new("__q", w)),
                src: Expr::bin(dop, acc.clone(), src.clone()),
            });
            stmts.push(Stmt::Assign {
                dst: Expr::Reg(RegRef::new("rdx", w)),
                src: Expr::bin(rop, acc, src),
            });
            stmts.push(Stmt::Assign {
                dst: Expr::Reg(RegRef::new("rax", w)),
                src: Expr::Reg(RegRef::new("__q", w)),
            });
        }

        // ---- flags ------------------------------------------------------
        Mnemonic::Cmp => {
            flags = Some(FlagSrc::Cmp(operand(insn, 0), operand(insn, 1)));
        }
        Mnemonic::Test => {
            flags = Some(FlagSrc::Test(operand(insn, 0), operand(insn, 1)));
        }

        // ---- conditional materialisation --------------------------------
        m2 if format!("{:?}", m2).starts_with("Set") && cond_of(m2).is_some() => {
            cond = cond_of(m2);
            // src filled in by the flag-resolution pass, which is the
            // only place that knows what set the flags.
            stmts.push(Stmt::Assign {
                dst: operand(insn, 0),
                src: Expr::Unknown("__setcc__".into()),
            });
        }
        m2 if format!("{:?}", m2).starts_with("Cmov") && cond_of(m2).is_some() => {
            cond = cond_of(m2);
            let dst = operand(insn, 0);
            let src = operand(insn, 1);
            stmts.push(Stmt::Assign {
                dst: dst.clone(),
                src: Expr::Ternary {
                    c: Box::new(Expr::Unknown("__cmov__".into())),
                    t: Box::new(src),
                    f: Box::new(dst),
                },
            });
        }

        // ---- control flow ------------------------------------------------
        Mnemonic::Jmp => {
            if insn.op0_kind() == OpKind::NearBranch64 || insn.op0_kind() == OpKind::NearBranch32 {
                let t = insn.near_branch_target();
                targets.push(t);
                stmts.push(Stmt::Goto(t));
            } else {
                // indirect jmp: jump table or a tail call through the PLT
                stmts.push(Stmt::Asm(format!("indirect jump: {}", fmt_out)));
            }
            is_block_end = true;
            falls_through = false;
        }
        m2 if cond_of(m2).is_some() && format!("{:?}", m2).starts_with('J') => {
            let t = insn.near_branch_target();
            cond = cond_of(m2);
            targets.push(t);
            // cond expression filled in by resolve_flags
            stmts.push(Stmt::If { cond: Expr::Unknown("__cc__".into()), target: t });
            is_block_end = true;
            falls_through = true;
        }
        Mnemonic::Call => {
            is_call = true;
            let (name, indirect) =
                if insn.op0_kind() == OpKind::NearBranch64 || insn.op0_kind() == OpKind::NearBranch32
                {
                    // For an unlinked .o the displacement is a placeholder
                    // patched later by the linker, so a relocation on that
                    // field beats the decoded target.
                    let field_addr = insn.ip() + insn.len() as u64 - 4;
                    let addr = insn.near_branch_target();
                    let n = ctx
                        .reloc_symbols
                        .get(&field_addr)
                        .or_else(|| ctx.symbols.get(&addr))
                        .map(|s| sanitize_name(s))
                        .unwrap_or_else(|| format!("sub_{:x}", addr));
                    (n, None)
                } else {
                    let tgt = operand(insn, 0);
                    // `call [rel X]` where X is a known GOT/PLT slot
                    if let Expr::Mem(mo) = &tgt {
                        if let Some(abs) = mo.rip_abs {
                            if let Some(n) = ctx.symbols.get(&abs) {
                                (sanitize_name(n), None)
                            } else {
                                ("(*fp)".to_string(), Some(Box::new(tgt.clone())))
                            }
                        } else {
                            ("(*fp)".to_string(), Some(Box::new(tgt.clone())))
                        }
                    } else {
                        ("(*fp)".to_string(), Some(Box::new(tgt.clone())))
                    }
                };
            // Every call is modelled as defining rax. If nothing reads
            // rax afterwards, dead-store elimination turns this back into
            // a bare `f(...);` statement. That is what makes
            // `v1 = abs_val(-7);` come out instead of the old
            // `abs_val(-7); *[rbp-4] = eax;` pair.
            stmts.push(Stmt::Assign {
                dst: Expr::Reg(RegRef::new("rax", 8)),
                src: Expr::Call { name, args: Vec::new(), indirect },
            });
        }
        Mnemonic::Ret | Mnemonic::Retf => {
            stmts.push(Stmt::Return(None));
            is_block_end = true;
            falls_through = false;
        }
        Mnemonic::Hlt | Mnemonic::Ud2 => {
            stmts.push(Stmt::Asm(fmt_out.clone()));
            is_block_end = true;
            falls_through = false;
        }
        Mnemonic::Nop | Mnemonic::Endbr64 | Mnemonic::Endbr32 => {
            stmts.push(Stmt::Nop);
        }

        // ---- scalar SSE (enough not to print /* unhandled */) ----------
        Mnemonic::Movss | Mnemonic::Movsd | Mnemonic::Movaps | Mnemonic::Movapd
        | Mnemonic::Movups | Mnemonic::Movupd | Mnemonic::Movq | Mnemonic::Movd => {
            stmts.push(Stmt::Assign { dst: operand(insn, 0), src: operand(insn, 1) });
        }
        Mnemonic::Addss | Mnemonic::Addsd | Mnemonic::Subss | Mnemonic::Subsd
        | Mnemonic::Mulss | Mnemonic::Mulsd | Mnemonic::Divss | Mnemonic::Divsd => {
            let dst = operand(insn, 0);
            let rhs = operand(insn, 1);
            let name = format!("{:?}", m).to_lowercase();
            let op = if name.starts_with("add") {
                BinOp::Add
            } else if name.starts_with("sub") {
                BinOp::Sub
            } else if name.starts_with("mul") {
                BinOp::Mul
            } else {
                BinOp::Div
            };
            stmts.push(Stmt::Assign { dst: dst.clone(), src: Expr::bin(op, dst, rhs) });
        }
        Mnemonic::Cvtsi2sd | Mnemonic::Cvtsi2ss => {
            let bits = if matches!(m, Mnemonic::Cvtsi2sd) { 64 } else { 32 };
            stmts.push(Stmt::Assign {
                dst: operand(insn, 0),
                src: Expr::cast(Type::Float { bits }, operand(insn, 1)),
            });
        }
        Mnemonic::Cvttsd2si | Mnemonic::Cvttss2si | Mnemonic::Cvtsd2si | Mnemonic::Cvtss2si => {
            let w = op_width(insn, 0);
            stmts.push(Stmt::Assign {
                dst: operand(insn, 0),
                src: Expr::cast(Type::from_width(w, true), operand(insn, 1)),
            });
        }
        Mnemonic::Ucomiss | Mnemonic::Ucomisd | Mnemonic::Comiss | Mnemonic::Comisd => {
            flags = Some(FlagSrc::Cmp(operand(insn, 0), operand(insn, 1)));
        }
        Mnemonic::Pxor | Mnemonic::Xorps | Mnemonic::Xorpd => {
            let dst = operand(insn, 0);
            let rhs = operand(insn, 1);
            let src =
                if dst == rhs { Expr::Const(0) } else { Expr::bin(BinOp::Xor, dst.clone(), rhs) };
            stmts.push(Stmt::Assign { dst, src });
        }

        _ => {
            stmts.push(Stmt::Asm(fmt_out.clone()));
        }
    }

    LiftedInsn {
        addr: insn.ip(),
        len: insn.len() as u32,
        asm_text: fmt_out.clone(),
        stmts,
        flags,
        cond,
        is_block_end,
        targets,
        falls_through,
        is_call,
        is_frame_setup,
    }
}

fn sign_extend_acc(reg: &str, from: u8, to: u8) -> Stmt {
    Stmt::Assign {
        dst: Expr::Reg(RegRef::new(reg, to)),
        src: Expr::cast(Type::from_width(to, true), Expr::Reg(RegRef::new(reg, from))),
    }
}

/// Decode + lift a byte range.
///
/// The original stopped at the first undecodable byte, truncating the
/// rest of the function. Compilers routinely drop alignment padding and
/// jump-table data inside a symbol's extent, so this resynchronises
/// instead: emit the bad byte as inline asm and continue one byte later.
pub fn lift_region(code: &[u8], base: u64, ctx: &LiftCtx) -> Vec<LiftedInsn> {
    let mut out = Vec::new();
    let mut scratch = String::new();
    let mut pos = 0usize;

    while pos < code.len() {
        let mut decoder =
            Decoder::with_ip(64, &code[pos..], base + pos as u64, DecoderOptions::NONE);
        let mut insn = Instruction::default();
        let mut advanced = false;
        while decoder.can_decode() {
            decoder.decode_out(&mut insn);
            if insn.is_invalid() {
                break;
            }
            out.push(lift(&insn, &mut scratch, ctx));
            pos += insn.len();
            advanced = true;
        }
        if !advanced {
            out.push(LiftedInsn {
                addr: base + pos as u64,
                len: 1,
                asm_text: format!("db 0x{:02x}", code[pos]),
                stmts: vec![Stmt::Asm(format!("db 0x{:02x}", code[pos]))],
                flags: None,
                cond: None,
                is_block_end: false,
                targets: vec![],
                falls_through: true,
                is_call: false,
                is_frame_setup: false,
            });
            pos += 1;
        }
    }
    out
}
