// cgen.rs — C output.
//
// The original printed each IR op with a `Display` impl, which is why
// memory came out as `*[rbp-0x4]` (not valid C), immediates always came
// out as unsigned hex (`0xfffffff9` where the source said `-7`), and
// unsigned comparisons printed as `<u` / `>=u` (also not valid C).

use crate::cfg::CNode;
use crate::frame::Frame;
use crate::ir::*;

pub struct Emitter<'a> {
    pub frame: &'a Frame,
    pub st: &'a StructTable,
    /// register family -> width it gets declared at. A value that stays
    /// in a register across a branch (`eax` holding the result of an
    /// if/else) has to become a real local, or the output names something
    /// that was never declared and cannot compile.
    pub reg_width: std::collections::HashMap<String, u8>,
    /// render without the width-conversion casts. They are real -- the
    /// machine does perform them -- but they bury the logic, so a reader
    /// wants to be able to switch them off.
    pub no_cast: bool,
    /// one entry per emitted line: the instruction address it came from
    pub trace: std::cell::RefCell<Vec<Option<u64>>>,
}

fn reg_name_at(r: &RegRef, width: u8) -> String {
    reg_display(&RegRef { full: r.full.clone(), size: width, high8: false })
}

fn reg_display(r: &RegRef) -> String {
    if r.is_xmm() || !r.full.starts_with('r') {
        return r.full.clone();
    }
    let base = &r.full[1..];
    let numbered = base.chars().next().map(|c| c.is_ascii_digit()).unwrap_or(false);
    match (r.size, numbered) {
        (8, _) => r.full.clone(),
        (4, false) => format!("e{}", base),
        (4, true) => format!("{}d", r.full),
        (2, false) => base.to_string(),
        (2, true) => format!("{}w", r.full),
        (1, false) => match base {
            "ax" | "bx" | "cx" | "dx" => format!("{}l", &base[..1]),
            _ => format!("{}l", base),
        },
        (1, true) => format!("{}b", r.full),
        _ => r.full.clone(),
    }
}

fn const_display(c: i64) -> String {
    if (-4096..=65535).contains(&c) {
        format!("{}", c)
    } else if c < 0 {
        format!("-0x{:x}", -(c as i128))
    } else {
        format!("0x{:x}", c)
    }
}

impl<'a> Emitter<'a> {
    pub fn expr(&self, e: &Expr) -> String {
        self.expr_prec(e, 0)
    }

    fn paren(&self, s: String, inner: u8, outer: u8) -> String {
        if inner < outer {
            format!("({})", s)
        } else {
            s
        }
    }

    fn expr_prec(&self, e: &Expr, outer: u8) -> String {
        match e {
            Expr::Const(c) => const_display(*c),
            Expr::Lit(s) => s.clone(),
            Expr::Reg(r) => match self.reg_width.get(&r.full) {
                Some(&w) => reg_name_at(r, w),
                None => reg_display(r),
            },
            Expr::Var(v) => self.frame.var(*v).name.clone(),
            Expr::Mem(_) => "/* unresolved memory */ 0".to_string(),
            Expr::Unknown(s) => format!("/* {} */ 0", s),

            Expr::Load { addr, ty } => {
                // `*(int *)p` — and if the address is already a cast to
                // the right pointer type, don't cast twice.
                let inner = match &**addr {
                    Expr::Cast { ty: cty, e } if *cty == Type::ptr(ty.clone()) => {
                        format!("({} *){}", ty.base_name(self.st), self.expr_prec(e, 12))
                    }
                    other => format!("({} *){}", ty.base_name(self.st), self.expr_prec(other, 12)),
                };
                self.paren(format!("*{}", inner), 12, outer)
            }
            Expr::AddrOf(inner) => self.paren(format!("&{}", self.expr_prec(inner, 12)), 12, outer),
            Expr::Index { base, idx } => {
                // an index is already an integer; `a[(long)i]` is noise
                let idx: &Expr = match &**idx {
                    Expr::Cast { e, .. } => e,
                    other => other,
                };
                if idx.as_const() == Some(0) {
                    self.paren(format!("*{}", self.expr_prec(base, 13)), 12, outer)
                } else {
                    format!("{}[{}]", self.expr_prec(base, 13), self.expr_prec(idx, 0))
                }
            }
            Expr::Arrow { base, sid, fid } => {
                let f = self
                    .st
                    .get(*sid)
                    .and_then(|s| s.fields.get(*fid))
                    .map(|f| f.name.clone())
                    .unwrap_or_else(|| format!("f{}", fid));
                format!("{}->{}", self.expr_prec(base, 13), f)
            }
            Expr::Dot { base, sid, fid } => {
                let f = self
                    .st
                    .get(*sid)
                    .and_then(|s| s.fields.get(*fid))
                    .map(|f| f.name.clone())
                    .unwrap_or_else(|| format!("f{}", fid));
                format!("{}.{}", self.expr_prec(base, 13), f)
            }
            Expr::Un { op, e } => {
                self.paren(format!("{}{}", op.sym(), self.expr_prec(e, 12)), 12, outer)
            }
            Expr::Cast { ty: _, e } if self.no_cast => self.expr_prec(e, outer),
            Expr::Cast { ty, e } => self.paren(
                format!("({}){}", ty.cast_name(self.st), self.expr_prec(e, 12)),
                12,
                outer,
            ),
            Expr::Bin { op, l, r } => {
                let p = op.prec();
                // C has no `<u`; unsigned comparison is spelled with casts
                let (ls, rs) = if op.is_unsigned_cmp() {
                    (
                        format!("(unsigned long){}", self.expr_prec(l, 12)),
                        format!("(unsigned long){}", self.expr_prec(r, 12)),
                    )
                } else {
                    (self.expr_prec(l, p), self.expr_prec(r, p + 1))
                };
                self.paren(format!("{} {} {}", ls, op.sym(), rs), p, outer)
            }
            Expr::Ternary { c, t, f } => self.paren(
                format!(
                    "{} ? {} : {}",
                    self.expr_prec(c, 2),
                    self.expr_prec(t, 0),
                    self.expr_prec(f, 0)
                ),
                1,
                outer,
            ),
            Expr::Call { name, args, indirect } => {
                let a: Vec<String> = args.iter().map(|x| self.expr_prec(x, 0)).collect();
                match indirect {
                    Some(target) => {
                        format!("(*{})({})", self.expr_prec(target, 12), a.join(", "))
                    }
                    None => format!("{}({})", name, a.join(", ")),
                }
            }
        }
    }

    /// Append text and record which instruction each resulting line came
    /// from, so the viewer can highlight the matching disassembly.
    fn emit(&self, out: &mut String, text: String, addr: Option<u64>) {
        let lines = text.matches('\n').count();
        out.push_str(&text);
        let mut t = self.trace.borrow_mut();
        for _ in 0..lines {
            t.push(addr);
        }
    }

    pub fn stmt(&self, s: &Stmt, addr: Option<u64>, pad: &str, out: &mut String) {
        match s {
            Stmt::Nop => {}
            Stmt::Assign { dst, src } => {
                // `x = x + 1` reads better as `x += 1`, and `x = x + 1`
                // with a literal 1 as `x++`.
                if let Expr::Bin { op, l, r } = src {
                    if **l == *dst {
                        if matches!(op, BinOp::Add | BinOp::Sub) && r.as_const() == Some(1) {
                            let s = if *op == BinOp::Add { "++" } else { "--" };
                            self.emit(out, format!("{}{}{};\n", pad, self.expr(dst), s), addr);
                            return;
                        }
                        if !op.is_cmp() {
                            self.emit(out, format!(
                                "{}{} {}= {};\n",
                                pad,
                                self.expr(dst),
                                op.sym(),
                                self.expr_prec(r, 0)
                            ), addr);
                            return;
                        }
                    }
                }
                self.emit(out, format!("{}{} = {};\n", pad, self.expr(dst), self.expr(src)), addr);
            }
            Stmt::Do(e) => self.emit(out, format!("{}{};\n", pad, self.expr(e)), addr),
            Stmt::Return(None) => self.emit(out, format!("{}return;\n", pad), addr),
            Stmt::Return(Some(e)) => {
                self.emit(out, format!("{}return {};\n", pad, self.expr(e)), addr)
            }
            Stmt::If { cond, target } => self.emit(out, format!(
                "{}if ({}) goto L{:x};\n",
                pad,
                self.expr(cond),
                target
            ), addr),
            Stmt::Goto(t) => self.emit(out, format!("{}goto L{:x};\n", pad, t), addr),
            Stmt::Asm(t) => self.emit(out, format!("{}__asm__(\"{}\");\n", pad, t.replace('"', "'")), addr),
        }
    }

    pub fn nodes(
        &self,
        ns: &[CNode],
        indent: usize,
        labels: &std::collections::HashSet<u64>,
        out: &mut String,
    ) {
        let pad = "    ".repeat(indent);
        for n in ns {
            match n {
                CNode::Stmts(ss) => {
                    for s in ss {
                        self.stmt(&s.1, Some(s.0), &pad, out);
                    }
                }
                CNode::Label(a) => {
                    if labels.contains(a) {
                        self.emit(out, format!("{}L{:x}:\n", "    ".repeat(indent.saturating_sub(1)), a), Some(*a));
                    }
                }
                CNode::Goto(a) => self.emit(out, format!("{}goto L{:x};\n", pad, a), None),
                CNode::Break => self.emit(out, format!("{}break;\n", pad), None),
                CNode::Continue => self.emit(out, format!("{}continue;\n", pad), None),
                CNode::If { cond, then_, else_ } => {
                    self.emit(out, format!("{}if ({}) {{\n", pad, self.expr(cond)), None);
                    self.nodes(then_, indent + 1, labels, out);
                    if else_.is_empty() {
                        self.emit(out, format!("{}}}\n", pad), None);
                    } else {
                        self.emit(out, format!("{}}} else {{\n", pad), None);
                        self.nodes(else_, indent + 1, labels, out);
                        self.emit(out, format!("{}}}\n", pad), None);
                    }
                }
                CNode::While { cond, body } => {
                    self.emit(out, format!("{}while ({}) {{\n", pad, self.expr(cond)), None);
                    self.nodes(body, indent + 1, labels, out);
                    self.emit(out, format!("{}}}\n", pad), None);
                }
                CNode::DoWhile { body, cond } => {
                    self.emit(out, format!("{}do {{\n", pad), None);
                    self.nodes(body, indent + 1, labels, out);
                    self.emit(out, format!("{}}} while ({});\n", pad, self.expr(cond)), None);
                }
                CNode::Forever { body } => {
                    self.emit(out, format!("{}for (;;) {{\n", pad), None);
                    self.nodes(body, indent + 1, labels, out);
                    self.emit(out, format!("{}}}\n", pad), None);
                }
            }
        }
    }

    /// Local declarations, in frame order, for the variables actually used.
    pub fn declarations(&self, used: &std::collections::HashSet<VarId>) -> String {
        let mut out = String::new();
        for v in self.frame.locals() {
            let id = self.frame.vars.iter().position(|x| x.off == v.off).unwrap();
            if !used.contains(&id) && !v.addr_taken {
                continue;
            }
            let decl = v.ty.declare(&v.name, self.st);
            out.push_str(&format!(
                "    {};{}// [rbp{}{:#x}]\n",
                decl,
                " ".repeat(28usize.saturating_sub(decl.len())),
                if v.off + 8 < 0 { "-" } else { "+" },
                (v.off + 8).abs(),
            ));
        }
        out
    }
}

/// Declarations for register-resident temporaries that survived.
impl<'a> Emitter<'a> {
    pub fn reg_declarations(&self) -> String {
        let mut names: Vec<(&String, &u8)> = self.reg_width.iter().collect();
        names.sort();
        let mut out = String::new();
        for (full, &w) in names {
            let r = RegRef { full: full.clone(), size: w, high8: false };
            let ty = if r.is_xmm() {
                Type::Float { bits: 64 }
            } else {
                Type::from_width(w, true)
            };
            let name = reg_display(&r);
            out.push_str(&format!(
                "    {};{}// register {}\n",
                ty.declare(&name, self.st),
                " ".repeat(28usize.saturating_sub(ty.declare(&name, self.st).len())),
                full
            ));
        }
        out
    }
}

/// Register families left in the body, and the widest access to each.
pub fn used_regs(ns: &[CNode], out: &mut std::collections::HashMap<String, u8>) {
    fn from_expr(e: &Expr, out: &mut std::collections::HashMap<String, u8>) {
        e.walk(&mut |x| {
            if let Expr::Reg(r) = x {
                let cur = out.entry(r.full.clone()).or_insert(r.size);
                if r.size > *cur {
                    *cur = r.size;
                }
            }
        });
    }
    for n in ns {
        match n {
            CNode::Stmts(ss) => {
                for s in ss {
                    match &s.1 {
                        Stmt::Assign { dst, src } => {
                            from_expr(dst, out);
                            from_expr(src, out);
                        }
                        Stmt::Do(e) | Stmt::Return(Some(e)) | Stmt::If { cond: e, .. } => {
                            from_expr(e, out)
                        }
                        _ => {}
                    }
                }
            }
            CNode::If { cond, then_, else_ } => {
                from_expr(cond, out);
                used_regs(then_, out);
                used_regs(else_, out);
            }
            CNode::While { cond, body } => {
                from_expr(cond, out);
                used_regs(body, out);
            }
            CNode::DoWhile { body, cond } => {
                from_expr(cond, out);
                used_regs(body, out);
            }
            CNode::Forever { body } => used_regs(body, out),
            _ => {}
        }
    }
}

/// Collect every variable id mentioned in the structured body.
pub fn used_vars(ns: &[CNode], out: &mut std::collections::HashSet<VarId>) {
    fn from_expr(e: &Expr, out: &mut std::collections::HashSet<VarId>) {
        e.walk(&mut |x| {
            if let Expr::Var(v) = x {
                out.insert(*v);
            }
        });
    }
    for n in ns {
        match n {
            CNode::Stmts(ss) => {
                for s in ss {
                    match &s.1 {
                        Stmt::Assign { dst, src } => {
                            from_expr(dst, out);
                            from_expr(src, out);
                        }
                        Stmt::Do(e) | Stmt::Return(Some(e)) | Stmt::If { cond: e, .. } => {
                            from_expr(e, out)
                        }
                        _ => {}
                    }
                }
            }
            CNode::If { cond, then_, else_ } => {
                from_expr(cond, out);
                used_vars(then_, out);
                used_vars(else_, out);
            }
            CNode::While { cond, body } => {
                from_expr(cond, out);
                used_vars(body, out);
            }
            CNode::DoWhile { body, cond } => {
                from_expr(cond, out);
                used_vars(body, out);
            }
            CNode::Forever { body } => used_vars(body, out),
            _ => {}
        }
    }
}
