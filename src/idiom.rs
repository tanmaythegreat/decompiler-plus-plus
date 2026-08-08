// idiom.rs — undo the arithmetic rewrites the compiler applied on the way
// down, so the output reads like the source rather than like the codegen.
//
// A compiler never emits `idiv` for a constant divisor: division is ~25
// cycles and a multiply-high is ~3, so it multiplies by a magic reciprocal
// and shifts. Read literally, `(int)(((long)x * 0x55555556) >> 32)` is
// noise. It means `x / 3`. The same is true of the shift-based division by
// a power of two, and of the `x - (x / d) * d` that spells `x % d`.
//
// Each recogniser here checks the algebraic identity rather than matching a
// fixed instruction sequence, so it survives instruction scheduling and
// differences between gcc's and clang's chosen encodings.

use crate::ir::*;

pub fn run(instrs: &mut [LiftedInsn]) {
    for ins in instrs.iter_mut() {
        for st in ins.stmts.iter_mut() {
            let taken = std::mem::replace(st, Stmt::Nop);
            *st = match taken {
                Stmt::Assign { dst, src } => Stmt::Assign { dst, src: walk(src) },
                Stmt::Do(e) => Stmt::Do(walk(e)),
                Stmt::Return(Some(e)) => Stmt::Return(Some(walk(e))),
                Stmt::If { cond, target } => Stmt::If { cond: walk(cond), target },
                other => other,
            };
        }
    }
}

fn walk(e: Expr) -> Expr {
    // bottom up: the modulo rule is built out of an already-recovered division
    let e = match e {
        Expr::Bin { op, l, r } => Expr::Bin { op, l: Box::new(walk(*l)), r: Box::new(walk(*r)) },
        Expr::Un { op, e } => Expr::Un { op, e: Box::new(walk(*e)) },
        Expr::Cast { ty, e } => Expr::Cast { ty, e: Box::new(walk(*e)) },
        Expr::Call { name, args, indirect } => Expr::Call {
            name,
            args: args.into_iter().map(walk).collect(),
            indirect: indirect.map(|i| Box::new(walk(*i))),
        },
        Expr::Load { addr, ty } => Expr::Load { addr: Box::new(walk(*addr)), ty },
        Expr::AddrOf(x) => Expr::AddrOf(Box::new(walk(*x))),
        Expr::Index { base, idx } => {
            Expr::Index { base: Box::new(walk(*base)), idx: Box::new(walk(*idx)) }
        }
        Expr::Ternary { c, t, f } => Expr::Ternary {
            c: Box::new(walk(*c)),
            t: Box::new(walk(*t)),
            f: Box::new(walk(*f)),
        },
        other => other,
    };

    for rule in [
        fold_self_multiple,
        magic_divide,
        magic_divide_with_add,
        power_of_two_divide,
        ternary_divide,
        sign_fixup,
        unsigned_shift_divide,
        modulo,
    ] {
        if let Some(rewritten) = rule(&e) {
            return rewritten;
        }
    }
    e
}

fn strip(e: &Expr) -> &Expr {
    match e {
        Expr::Cast { e, .. } => strip(e),
        other => other,
    }
}

/// `(x * M) >> s`, where M is a reciprocal of some d, means `x / d`.
///
/// The compiler picks M = ceil(2^s / d), so d = round(2^s / M) recovers the
/// divisor. Checking that identity is what makes this safe: an ordinary
/// multiply-then-shift that happens to look similar will not satisfy it.
fn magic_divide(e: &Expr) -> Option<Expr> {
    let Expr::Bin { op: BinOp::Sar | BinOp::Shr, l, r } = e else { return None };
    let shift = strip(r).as_const()?;
    if !(32..64).contains(&shift) {
        return None;
    }
    let (x, magic) = mul_by_const(strip(l))?;
    if magic <= 1 {
        return None;
    }

    let scale = 1i128 << shift;
    let d = ((scale + magic as i128 / 2) / magic as i128) as i64;
    if d <= 1 {
        return None;
    }
    // confirm: the magic must be the one this divisor generates
    let expect = (scale + d as i128 - 1) / d as i128;
    if expect != magic as i128 {
        return None;
    }
    Some(Expr::bin(BinOp::Div, narrow(x), Expr::Const(d)))
}

/// `(x + ((x >> 31) >>> (32 - k))) >> k` means `x / 2^k` for signed x —
/// the added term is the round-toward-zero bias, nonzero only when x < 0.
fn power_of_two_divide(e: &Expr) -> Option<Expr> {
    let Expr::Bin { op: BinOp::Sar, l, r } = e else { return None };
    let k = strip(r).as_const()?;
    if !(1..63).contains(&k) {
        return None;
    }
    let Expr::Bin { op: BinOp::Add, l: base, r: bias } = strip(l) else { return None };
    if !is_sign_bias(strip(bias), k) {
        return None;
    }
    Some(Expr::bin(BinOp::Div, (**base).clone(), Expr::Const(1i64 << k)))
}

/// The unsigned case has no bias term: a bare `x >>> k` on an unsigned
/// value is a division, and reads better as one.
fn unsigned_shift_divide(e: &Expr) -> Option<Expr> {
    let Expr::Bin { op: BinOp::Shr, l, r } = e else { return None };
    let k = strip(r).as_const()?;
    if !(1..63).contains(&k) {
        return None;
    }
    // only worth rewriting when the shift is clearly arithmetic in intent:
    // a shift used for bit extraction is normally followed by a mask, and
    // that case still reads better as a shift.
    if k >= 8 {
        return None;
    }
    Some(Expr::bin(BinOp::Div, (**l).clone(), Expr::Const(1i64 << k)))
}

/// `x - (x / d) * d` is `x % d`.
fn modulo(e: &Expr) -> Option<Expr> {
    let Expr::Bin { op: BinOp::Sub, l, r } = e else { return None };
    let (quot, d) = mul_by_const(strip(r))?;
    let Expr::Bin { op: BinOp::Div, l: qx, r: qd } = strip(&quot) else { return None };
    if strip(qd).as_const()? != d {
        return None;
    }
    if strip(l) != strip(qx) {
        return None;
    }
    Some(Expr::bin(BinOp::Rem, (**qx).clone(), Expr::Const(d)))
}

/// When the reciprocal does not fit in 32 bits the compiler emits a magic
/// that has wrapped negative and compensates with an extra add:
/// `((x * M) >> 32 + x) >> k`. The true multiplier is M + 2^32.
fn magic_divide_with_add(e: &Expr) -> Option<Expr> {
    let Expr::Bin { op: BinOp::Sar, l, r } = e else { return None };
    let k = strip(r).as_const()?;
    if !(0..32).contains(&k) {
        return None;
    }
    let Expr::Bin { op: BinOp::Add, l: hi, r: plain } = strip(l) else { return None };

    let Expr::Bin { op: BinOp::Sar | BinOp::Shr, l: prod, r: hishift } = strip(hi) else {
        return None;
    };
    if strip(hishift).as_const()? != 32 {
        return None;
    }
    let (x, magic) = mul_by_const(strip(prod))?;
    if magic >= 0 {
        return None;
    }
    if strip(&x) != strip(plain) {
        return None;
    }

    let m = magic as i128 + (1i128 << 32);
    let scale = 1i128 << (32 + k);
    let d = ((scale + m / 2) / m) as i64;
    if d <= 1 {
        return None;
    }
    if (scale + d as i128 - 1) / d as i128 - (1i128 << 32) != magic as i128 {
        return None;
    }
    Some(Expr::bin(BinOp::Div, narrow(x), Expr::Const(d)))
}

/// The signed magic sequence ends by adding back the sign bit, because the
/// shift rounds toward negative infinity and C rounds toward zero. Once the
/// division itself is recovered the correction is part of it.
fn sign_fixup(e: &Expr) -> Option<Expr> {
    let Expr::Bin { op: BinOp::Sub, l, r } = e else { return None };
    let Expr::Bin { op: BinOp::Sar, r: sh, .. } = strip(r) else { return None };
    let n = strip(sh).as_const()?;
    if n != 31 && n != 63 {
        return None;
    }
    if !matches!(strip(l), Expr::Bin { op: BinOp::Div, .. }) {
        return None;
    }
    Some((**l).clone())
}

/// Division by a power of two, branchless form: the compiler biases the
/// numerator by `2^k - 1` when it is negative, then shifts.
fn ternary_divide(e: &Expr) -> Option<Expr> {
    let Expr::Bin { op: BinOp::Sar, l, r } = e else { return None };
    let k = strip(r).as_const()?;
    if !(1..63).contains(&k) {
        return None;
    }
    let Expr::Ternary { c, t, f } = strip(l) else { return None };

    // condition must be "numerator is non-negative"
    let Expr::Bin { op: BinOp::Ge, l: cl, r: cr } = strip(c) else { return None };
    if strip(cr).as_const()? != 0 {
        return None;
    }
    let x = strip(cl);
    if strip(t) != x {
        return None;
    }
    let Expr::Bin { op: BinOp::Add, l: bl, r: br } = strip(f) else { return None };
    if strip(bl) != x || strip(br).as_const()? != (1i64 << k) - 1 {
        return None;
    }
    Some(Expr::bin(BinOp::Div, x.clone(), Expr::Const(1i64 << k)))
}

/// `q * 8 - q` is `q * 7`. Multiplying by a constant one away from a power
/// of two is cheaper as a shift and a subtract, and the compiler knows it;
/// folding it back is what lets the modulo rule see its multiply.
fn fold_self_multiple(e: &Expr) -> Option<Expr> {
    let Expr::Bin { op: op @ (BinOp::Sub | BinOp::Add), l, r } = e else { return None };
    let (prod, lone, prod_first) = match (mul_by_const(strip(l)), mul_by_const(strip(r))) {
        (Some(p), None) => (p, (**r).clone(), true),
        (None, Some(p)) => (p, (**l).clone(), false),
        _ => return None,
    };
    if strip(&prod.0) != strip(&lone) {
        return None;
    }
    let k = match op {
        BinOp::Sub if prod_first => prod.1 - 1,
        BinOp::Sub => 1 - prod.1,
        _ => prod.1 + 1,
    };
    Some(Expr::bin(BinOp::Mul, lone, Expr::Const(k)))
}

fn mul_by_const(e: &Expr) -> Option<(Expr, i64)> {
    let Expr::Bin { op: BinOp::Mul, l, r } = e else { return None };
    if let Some(c) = strip(r).as_const() {
        return Some(((**l).clone(), c));
    }
    if let Some(c) = strip(l).as_const() {
        return Some(((**r).clone(), c));
    }
    None
}

/// `(x >> 31) >>> (32 - k)`, in either order of the widths the compiler
/// might have used.
fn is_sign_bias(e: &Expr, k: i64) -> bool {
    let Expr::Bin { op: BinOp::Shr, l, r } = e else { return false };
    let Some(back) = strip(r).as_const() else { return false };
    let Expr::Bin { op: BinOp::Sar, r: signshift, .. } = strip(l) else { return false };
    let Some(sh) = strip(signshift).as_const() else { return false };
    (sh == 31 && back == 32 - k) || (sh == 63 && back == 64 - k)
}

/// The magic multiply is done at double width; the result is used narrow.
fn narrow(x: Expr) -> Expr {
    match x {
        Expr::Cast { ty: Type::Int { bits: 64, .. }, e } => *e,
        other => other,
    }
}
