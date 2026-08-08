// ptr.rs — pointer, struct and array recovery from finished expressions.
//
// `frame.rs` recovers aggregates that live *in* the frame by looking at
// addressing modes. That is not enough for aggregates reached *through* a
// pointer, because gcc at -O0 computes `p + i*4` with a separate `add`:
//
//     mov  rdx, [rbp-0x18]      ; the pointer
//     mov  eax, [rbp-0x14]      ; the index
//     cdqe
//     lea  rcx, [0+rax*4]
//     add  rdx, rcx
//     mov  eax, [rdx]
//
// By the time the base register reaches the load it has been overwritten,
// so no addressing-mode-level analysis can attribute it. After copy
// propagation, though, the load reads exactly `*(int *)(a1 + (long)i * 4)`
// — the shape is right there in the expression tree. This pass reads the
// shapes back out, decides the pointee layout, and rewrites the loads into
// `a1[i]` and `p->field_8`.

use crate::frame::Frame;
use crate::ir::*;
use std::collections::{BTreeMap, BTreeSet, HashMap};

#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
enum Base {
    Var(VarId),
}

#[derive(Clone, Debug)]
struct Access {
    disp: i64,
    scaled: Option<i64>,
    size: u8,
}

#[derive(Clone, Debug)]
enum Layout {
    /// pointer to a scalar of this many bytes
    Elem(u8),
    Struct(usize),
}

/// base + constant + optional index*scale
fn decompose(e: &Expr) -> Option<(Base, i64, Option<(Expr, i64)>)> {
    match e {
        Expr::Cast { e, .. } => decompose(e),
        Expr::Var(v) => Some((Base::Var(*v), 0, None)),
        Expr::Bin { op: BinOp::Add, l, r } => {
            combine(l, r).or_else(|| combine(r, l))
        }
        Expr::Bin { op: BinOp::Sub, l, r } => {
            let c = r.as_const()?;
            let (b, d, ix) = decompose(l)?;
            Some((b, d - c, ix))
        }
        _ => None,
    }
}

fn combine(base: &Expr, off: &Expr) -> Option<(Base, i64, Option<(Expr, i64)>)> {
    let (b, d, ix) = decompose(base)?;
    match strip_casts(off) {
        Expr::Const(c) => Some((b, d + c, ix)),
        Expr::Bin { op: BinOp::Mul, l, r } => {
            if ix.is_some() {
                return None;
            }
            let s = r.as_const()?;
            Some((b, d, Some((strip_casts(&l), s))))
        }
        Expr::Bin { op: BinOp::Shl, l, r } => {
            if ix.is_some() {
                return None;
            }
            let sh = r.as_const()?;
            if !(0..6).contains(&sh) {
                return None;
            }
            Some((b, d, Some((strip_casts(&l), 1i64 << sh))))
        }
        _ => None,
    }
}

fn strip_casts(e: &Expr) -> Expr {
    match e {
        Expr::Cast { e, .. } => strip_casts(e),
        other => other.clone(),
    }
}

fn collect_expr(e: &Expr, out: &mut BTreeMap<Base, Vec<Access>>) {
    e.walk(&mut |x| {
        if let Expr::Load { addr, ty } = x {
            if let Some((b, d, ix)) = decompose(addr) {
                let size = match ty {
                    Type::Int { bits, .. } => (*bits / 8) as u8,
                    _ => 8,
                };
                out.entry(b).or_default().push(Access {
                    disp: d,
                    scaled: ix.map(|(_, s)| s),
                    size,
                });
            }
        }
    });
}

fn each_expr<F: FnMut(&Expr)>(instrs: &[LiftedInsn], f: &mut F) {
    for ins in instrs {
        for st in &ins.stmts {
            match st {
                Stmt::Assign { dst, src } => {
                    f(dst);
                    f(src);
                }
                Stmt::Do(e) | Stmt::Return(Some(e)) | Stmt::If { cond: e, .. } => f(e),
                _ => {}
            }
        }
    }
}

/// Variables that get advanced by a constant (`p = p + 1`) are walking a
/// sequence of elements. `*p` and `*(p+1)` are then the same field of two
/// elements, not two fields of one struct.
fn walked(instrs: &[LiftedInsn]) -> BTreeSet<VarId> {
    let mut out = BTreeSet::new();
    for ins in instrs {
        for st in &ins.stmts {
            if let Stmt::Assign { dst: Expr::Var(v), src } = st {
                let mut sees_self = false;
                src.walk(&mut |x| {
                    if matches!(x, Expr::Var(o) if o == v) {
                        sees_self = true;
                    }
                });
                // `p = p + 1` and `p = &p[1]` are the same idiom; gcc picks
                // whichever the addressing mode makes cheaper.
                let advances = matches!(src, Expr::Bin { op: BinOp::Add | BinOp::Sub, .. })
                    || matches!(src, Expr::AddrOf(_));
                if sees_self && advances {
                    out.insert(*v);
                }
            }
        }
    }
    out
}

/// Infer pointee layouts, register any new structs, and rewrite the loads.
pub fn recover(instrs: &mut [LiftedInsn], frame: &mut Frame, st: &mut StructTable, fname: &str) {
    let mut accesses: BTreeMap<Base, Vec<Access>> = BTreeMap::new();
    each_expr(instrs, &mut |e| collect_expr(e, &mut accesses));
    let walking = walked(instrs);

    let mut layouts: HashMap<Base, Layout> = HashMap::new();

    for (b, accs) in &accesses {
        let Base::Var(vid) = b;
        // Something already typed as an array lives in the frame itself,
        // not behind a pointer; leave it alone.
        if matches!(frame.vars[*vid].ty, Type::Array(..)) {
            continue;
        }
        // A slot the code only ever reads as a plain value isn't a pointer.
        if accs.iter().all(|a| a.disp == 0 && a.scaled.is_none()) && accs.len() < 2 {
            continue;
        }

        let scale = accs.iter().filter_map(|a| a.scaled).max();
        if let Some(s) = scale {
            layouts.insert(b.clone(), Layout::Elem(s as u8));
            frame.vars[*vid].ty = Type::ptr(Type::from_width(s as u8, true));
            continue;
        }

        let offsets: BTreeSet<i64> = accs.iter().map(|a| a.disp).collect();
        let uniform = {
            let s0 = accs[0].size;
            accs.iter().all(|a| a.size == s0)
                && offsets.iter().all(|o| *o >= 0 && o % s0.max(1) as i64 == 0)
        };
        if walking.contains(vid) && uniform {
            let s = accs[0].size;
            layouts.insert(b.clone(), Layout::Elem(s));
            frame.vars[*vid].ty = Type::ptr(Type::from_width(s, true));
            continue;
        }
        if offsets.len() > 1 || offsets.iter().any(|&o| o != 0) {
            let mut layout: BTreeMap<i64, BTreeSet<u8>> = BTreeMap::new();
            for a in accs {
                layout.entry(a.disp).or_default().insert(a.size);
            }
            let sid = make_struct(st, fname, &layout);
            layouts.insert(b.clone(), Layout::Struct(sid));
            frame.vars[*vid].ty = Type::ptr(Type::Struct(sid));
        } else {
            let s = accs.iter().map(|a| a.size).max().unwrap_or(8);
            layouts.insert(b.clone(), Layout::Elem(s));
            frame.vars[*vid].ty = Type::ptr(Type::from_width(s, true));
        }
    }

    if layouts.is_empty() {
        return;
    }

    // keep the declared parameter list in step with the new variable types
    for (i, t) in frame.param_types.iter_mut().enumerate() {
        if let Some(v) = frame.vars.iter().find(|v| v.is_param && v.param_index == i) {
            *t = v.ty.clone();
        }
    }

    for ins in instrs.iter_mut() {
        for s in ins.stmts.iter_mut() {
            rewrite_stmt(s, &layouts, st);
        }
    }
}

fn rewrite_stmt(s: &mut Stmt, layouts: &HashMap<Base, Layout>, st: &StructTable) {
    let taken = std::mem::replace(s, Stmt::Nop);
    *s = match taken {
        Stmt::Assign { dst, src } => {
            Stmt::Assign { dst: rewrite(dst, layouts, st), src: rewrite(src, layouts, st) }
        }
        Stmt::Do(e) => Stmt::Do(rewrite(e, layouts, st)),
        Stmt::Return(Some(e)) => Stmt::Return(Some(rewrite(e, layouts, st))),
        Stmt::If { cond, target } => Stmt::If { cond: rewrite(cond, layouts, st), target },
        other => other,
    };
}

fn rewrite(e: Expr, layouts: &HashMap<Base, Layout>, st: &StructTable) -> Expr {
    match e {
        Expr::Load { addr, ty } => {
            if let Some((b, d, ix)) = decompose(&addr) {
                if let Some(layout) = layouts.get(&b) {
                    let Base::Var(vid) = b;
                    let base = Expr::Var(vid);
                    match layout {
                        Layout::Elem(s) => {
                            let s = (*s).max(1) as i64;
                            let idx = match ix {
                                Some((i, sc)) if sc == s => {
                                    if d == 0 {
                                        i
                                    } else if d % s == 0 {
                                        Expr::bin(BinOp::Add, i, Expr::Const(d / s))
                                    } else {
                                        return Expr::Load {
                                            addr: Box::new(rewrite(*addr, layouts, st)),
                                            ty,
                                        };
                                    }
                                }
                                None if d % s == 0 => Expr::Const(d / s),
                                _ => {
                                    return Expr::Load {
                                        addr: Box::new(rewrite(*addr, layouts, st)),
                                        ty,
                                    }
                                }
                            };
                            return Expr::Index {
                                base: Box::new(base),
                                idx: Box::new(rewrite(idx, layouts, st)),
                            };
                        }
                        Layout::Struct(sid) => {
                            if ix.is_none() {
                                if let Some(fid) = st.field_at(*sid, d) {
                                    return Expr::Arrow { base: Box::new(base), sid: *sid, fid };
                                }
                            }
                        }
                    }
                }
            }
            Expr::Load { addr: Box::new(rewrite(*addr, layouts, st)), ty }
        }
        Expr::AddrOf(e) => Expr::AddrOf(Box::new(rewrite(*e, layouts, st))),
        Expr::Bin { op, l, r } => Expr::Bin {
            op,
            l: Box::new(rewrite(*l, layouts, st)),
            r: Box::new(rewrite(*r, layouts, st)),
        },
        Expr::Un { op, e } => Expr::Un { op, e: Box::new(rewrite(*e, layouts, st)) },
        Expr::Cast { ty, e } => Expr::Cast { ty, e: Box::new(rewrite(*e, layouts, st)) },
        Expr::Index { base, idx } => Expr::Index {
            base: Box::new(rewrite(*base, layouts, st)),
            idx: Box::new(rewrite(*idx, layouts, st)),
        },
        Expr::Arrow { base, sid, fid } => {
            Expr::Arrow { base: Box::new(rewrite(*base, layouts, st)), sid, fid }
        }
        Expr::Dot { base, sid, fid } => {
            Expr::Dot { base: Box::new(rewrite(*base, layouts, st)), sid, fid }
        }
        Expr::Call { name, args, indirect } => Expr::Call {
            name,
            args: args.into_iter().map(|a| rewrite(a, layouts, st)).collect(),
            indirect: indirect.map(|i| Box::new(rewrite(*i, layouts, st))),
        },
        Expr::Ternary { c, t, f } => Expr::Ternary {
            c: Box::new(rewrite(*c, layouts, st)),
            t: Box::new(rewrite(*t, layouts, st)),
            f: Box::new(rewrite(*f, layouts, st)),
        },
        leaf => leaf,
    }
}

fn make_struct(st: &mut StructTable, fname: &str, layout: &BTreeMap<i64, BTreeSet<u8>>) -> usize {
    let mut fields: Vec<Field> = Vec::new();
    let mut cursor: i64 = 0;
    for (&off, sizes) in layout {
        let size = sizes.iter().copied().max().unwrap_or(1);
        if off > cursor {
            fields.push(Field {
                off: cursor,
                ty: Type::Array(
                    Box::new(Type::Int { bits: 8, signed: true }),
                    (off - cursor) as usize,
                ),
                name: format!("pad_{:x}", cursor),
            });
        }
        if off < cursor {
            continue;
        }
        fields.push(Field { off, ty: Type::from_width(size, true), name: format!("field_{:x}", off) });
        cursor = off + size as i64;
    }
    let name = format!("{}_s{}", fname, st.defs.len());
    st.add(StructDef { name, fields, size: cursor.max(0) as usize })
}
