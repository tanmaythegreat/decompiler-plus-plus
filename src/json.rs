// json.rs — export everything the viewer needs, including enough operational
// semantics for it to single-step the code itself.
//
// Written by hand rather than with serde: the crate has two dependencies and
// adding a third for one output format is not worth it.

use crate::ir::*;
use std::fmt::Write;

pub fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out
}

pub fn q(s: &str) -> String {
    format!("\"{}\"", esc(s))
}

/// An expression as a JSON node the viewer's interpreter can evaluate.
///
/// The shape mirrors the IR rather than the printed C, because the viewer
/// needs to *run* this, not display it: a cast has to actually truncate,
/// and a load has to actually read memory.
pub fn expr(e: &Expr) -> String {
    match e {
        // as a string: a 64-bit immediate does not survive a round trip
        // through a JSON number, and `movabs rdx, 0x64202c6f6c6c6568` is
        // exactly the case that matters
        Expr::Const(v) => format!("{{\"k\":\"const\",\"v\":\"{}\"}}", v),
        Expr::Lit(s) => format!("{{\"k\":\"lit\",\"s\":{}}}", q(s)),
        Expr::Reg(r) => format!(
            "{{\"k\":\"reg\",\"r\":{},\"sz\":{},\"hi8\":{}}}",
            q(&r.full),
            r.size,
            r.high8
        ),
        Expr::Var(id) => format!("{{\"k\":\"var\",\"id\":{}}}", id),
        Expr::Mem(m) => format!(
            "{{\"k\":\"mem\",\"base\":{},\"index\":{},\"scale\":{},\"disp\":{},\"sz\":{},\"signed\":{}}}",
            m.base.as_ref().map(|b| q(&b.full)).unwrap_or_else(|| "null".into()),
            m.index.as_ref().map(|b| q(&b.full)).unwrap_or_else(|| "null".into()),
            m.scale,
            m.disp,
            m.size,
            m.signed
        ),
        Expr::Load { addr, ty } => {
            format!("{{\"k\":\"load\",\"a\":{},\"sz\":{},\"signed\":{}}}", expr(addr), ty_size(ty), ty_signed(ty))
        }
        Expr::AddrOf(x) => format!("{{\"k\":\"addr\",\"e\":{}}}", expr(x)),
        Expr::Bin { op, l, r } => format!(
            "{{\"k\":\"bin\",\"op\":{},\"l\":{},\"r\":{}}}",
            q(&format!("{:?}", op)),
            expr(l),
            expr(r)
        ),
        Expr::Un { op, e } => {
            format!("{{\"k\":\"un\",\"op\":{},\"e\":{}}}", q(&format!("{:?}", op)), expr(e))
        }
        Expr::Cast { ty, e } => format!(
            "{{\"k\":\"cast\",\"sz\":{},\"signed\":{},\"e\":{}}}",
            ty_size(ty),
            ty_signed(ty),
            expr(e)
        ),
        Expr::Call { name, args, .. } => format!(
            "{{\"k\":\"call\",\"name\":{},\"args\":[{}]}}",
            q(name),
            args.iter().map(expr).collect::<Vec<_>>().join(",")
        ),
        Expr::Index { base, idx } => {
            format!("{{\"k\":\"index\",\"b\":{},\"i\":{}}}", expr(base), expr(idx))
        }
        Expr::Arrow { base, sid, fid } | Expr::Dot { base, sid, fid } => format!(
            "{{\"k\":\"field\",\"b\":{},\"sid\":{},\"fid\":{}}}",
            expr(base),
            sid,
            fid
        ),
        Expr::Ternary { c, t, f } => format!(
            "{{\"k\":\"tern\",\"c\":{},\"t\":{},\"f\":{}}}",
            expr(c),
            expr(t),
            expr(f)
        ),
        Expr::Unknown(s) => format!("{{\"k\":\"unknown\",\"s\":{}}}", q(s)),
    }
}

fn ty_size(t: &Type) -> u8 {
    match t {
        Type::Int { bits, .. } => (bits / 8) as u8,
        Type::Float { bits } => (bits / 8) as u8,
        Type::Ptr(_) => 8,
        _ => 8,
    }
}

fn ty_signed(t: &Type) -> bool {
    matches!(t, Type::Int { signed: true, .. })
}

pub fn stmt(s: &Stmt) -> String {
    match s {
        Stmt::Nop => "{\"k\":\"nop\"}".to_string(),
        Stmt::Assign { dst, src } => {
            format!("{{\"k\":\"assign\",\"dst\":{},\"src\":{}}}", expr(dst), expr(src))
        }
        Stmt::Do(e) => format!("{{\"k\":\"do\",\"e\":{}}}", expr(e)),
        Stmt::Return(v) => format!(
            "{{\"k\":\"ret\",\"e\":{}}}",
            v.as_ref().map(expr).unwrap_or_else(|| "null".into())
        ),
        Stmt::If { cond, target } => {
            format!("{{\"k\":\"if\",\"c\":{},\"t\":{}}}", expr(cond), target)
        }
        Stmt::Goto(t) => format!("{{\"k\":\"goto\",\"t\":{}}}", t),
        Stmt::Asm(t) => format!("{{\"k\":\"asm\",\"s\":{}}}", q(t)),
    }
}

pub fn structs(st: &StructTable) -> String {
    let defs: Vec<String> = st
        .defs
        .iter()
        .enumerate()
        .map(|(i, d)| {
            let fields: Vec<String> = d
                .fields
                .iter()
                .map(|f| {
                    format!(
                        "{{\"name\":{},\"off\":{},\"ty\":{},\"size\":{}}}",
                        q(&f.name),
                        f.off,
                        q(&f.ty.base_name(st)),
                        f.ty.size(st)
                    )
                })
                .collect();
            format!(
                "{{\"id\":{},\"name\":{},\"size\":{},\"fields\":[{}]}}",
                i,
                q(&d.name),
                d.size,
                fields.join(",")
            )
        })
        .collect();
    format!("[{}]", defs.join(","))
}
