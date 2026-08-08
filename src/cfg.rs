// cfg.rs — builds basic blocks + CFG from lifted instructions, computes
// dominators, finds loop back-edges, and does a best-effort structuring
// pass (if/else, while) with goto fallback for anything it can't match.
// This is the reduced-scope stand-in for `analysis-passes` + `output-c`.

use crate::ir::*;
use crate::lifter::{reg_family, sysv_arg_index};
use std::collections::{BTreeMap, BTreeSet, HashMap};

/// Best-effort recovery of a function's own parameter count and return
/// type from its (already-lifted, address-ordered) instruction list --
/// same trick real decompilers use when there's no symbol/debug info to
/// read a signature from: a SysV arg register (rdi, rsi, rdx, rcx, r8,
/// r9) counts as an incoming parameter if the *first* thing the function
/// does with it is read it, before ever writing it -- a value nothing in
/// this function produced can only have come from the caller. Takes the
/// highest such register's index + 1 as the count, so an unused-but-
/// passed middle parameter (rare, but possible) doesn't create a gap;
/// the flip side is a truly unused *trailing* parameter (never read at
/// all) is invisible to this and won't be counted -- no way to tell that
/// apart from "this function only takes N args" without debug info.
///
/// Return type: "void" unless something writes to rax anywhere in the
/// body. `Ret` always carries `rax` in this IR (see lifter.rs), so
/// checking real writes -- not just the synthetic return -- is what
/// actually distinguishes a function that produces a value from one
/// that doesn't.
fn detect_signature(instrs: &[LiftedInsn]) -> (usize, bool) {
    let mut written = [false; 6];
    let mut read_before_write = [false; 6];
    let mut rax_written = false;

    for ins in instrs {
        for op in &ins.ops {
            let reads: Vec<&Value> = match op {
                Instr::Copy { src, .. } => vec![src],
                // `xor reg, reg` is the standard x86 zero-idiom, not a
                // genuine read of reg's old value (that's the whole
                // point of using it over `mov reg, 0`) -- counting it
                // would flag any first-touched arg register as an
                // incoming param just because the compiler happened to
                // zero it that way (e.g. `xor r8d, r8d` in the _start
                // stub, or `xor eax, eax` before a 0 return).
                Instr::Bin { op: BinOp::Xor, lhs, rhs, .. } if lhs == rhs => vec![],
                Instr::Bin { lhs, rhs, .. } => vec![lhs, rhs],
                Instr::Un { src, .. } => vec![src],
                Instr::Cmp { lhs, rhs, .. } => vec![lhs, rhs],
                Instr::Call { args, .. } => args.iter().collect(),
                Instr::Ret { val: Some(v) } => vec![v],
                _ => vec![],
            };
            for v in reads {
                if let Value::Reg(name) = v {
                    if let Some(idx) = sysv_arg_index(name) {
                        if !written[idx] {
                            read_before_write[idx] = true;
                        }
                    }
                }
            }

            let dst = match op {
                Instr::Copy { dst, .. } | Instr::Bin { dst, .. } | Instr::Un { dst, .. } => Some(dst),
                _ => None,
            };
            if let Some(Value::Reg(name)) = dst {
                if let Some(idx) = sysv_arg_index(name) {
                    written[idx] = true;
                }
                if reg_family(name) == "rax" {
                    rax_written = true;
                }
            }
        }
    }

    let param_count = read_before_write.iter().rposition(|&b| b).map(|i| i + 1).unwrap_or(0);
    (param_count, rax_written)
}

pub struct Block {
    pub addr: u64,
    pub instrs: Vec<usize>, // indices into the function's instr vec
    pub succs: Vec<u64>,    // 0, 1 (unconditional), or 2 (cond: [true, false])
    pub preds: Vec<u64>,
    pub is_cond: bool,
}

pub struct Function {
    pub name: String,
    pub entry: u64,
    pub instrs: Vec<LiftedInsn>,
    pub blocks: BTreeMap<u64, Block>,
    /// Recovered arg count (see `detect_signature`) -- SysV int/pointer
    /// args only, so a function that's actually variadic-with-floats or
    /// takes float params will undercount.
    pub param_count: usize,
    /// True if nothing in the function ever writes rax -- see `detect_signature`.
    pub is_void: bool,
}

impl Function {
    /// Build basic blocks + edges from a flat, address-sorted instruction stream.
    pub fn build(name: String, mut instrs: Vec<LiftedInsn>) -> Function {
        instrs.sort_by_key(|i| i.addr);
        let entry = instrs.first().map(|i| i.addr).unwrap_or(0);

        // 1. find leaders
        let mut leaders: BTreeSet<u64> = BTreeSet::new();
        leaders.insert(entry);
        let addr_index: HashMap<u64, usize> =
            instrs.iter().enumerate().map(|(i, ins)| (ins.addr, i)).collect();
        for (i, ins) in instrs.iter().enumerate() {
            if ins.is_block_end {
                for &t in &ins.targets {
                    leaders.insert(t);
                }
                if let Some(next) = instrs.get(i + 1) {
                    leaders.insert(next.addr);
                }
            }
        }

        // 2. group instructions into blocks between consecutive leaders
        let leader_vec: Vec<u64> = leaders.into_iter().collect();
        let mut blocks: BTreeMap<u64, Block> = BTreeMap::new();
        for (li, &laddr) in leader_vec.iter().enumerate() {
            let next_leader = leader_vec.get(li + 1).copied();
            let start_idx = match addr_index.get(&laddr) {
                Some(&ix) => ix,
                None => continue, // target outside our region (external call etc)
            };
            let mut idxs = Vec::new();
            let mut j = start_idx;
            loop {
                let ins = &instrs[j];
                if let Some(nl) = next_leader {
                    if ins.addr >= nl {
                        break;
                    }
                }
                idxs.push(j);
                if ins.is_block_end {
                    break;
                }
                j += 1;
                if j >= instrs.len() {
                    break;
                }
            }
            if idxs.is_empty() {
                continue;
            }
            let last = &instrs[*idxs.last().unwrap()];
            let mut succs = Vec::new();
            let mut is_cond = false;
            match last.ops.iter().find(|o| matches!(o, Instr::CBranch { .. } | Instr::Branch { .. })) {
                Some(Instr::CBranch { target }) => {
                    succs.push(*target); // true
                    if let Some(next) = instrs.get(*idxs.last().unwrap() + 1) {
                        succs.push(next.addr); // false / fallthrough
                    }
                    is_cond = true;
                }
                Some(Instr::Branch { target }) => {
                    succs.push(*target);
                }
                _ => {
                    if !matches!(last.ops.last(), Some(Instr::Ret { .. })) {
                        if let Some(next) = instrs.get(*idxs.last().unwrap() + 1) {
                            succs.push(next.addr);
                        }
                    }
                }
            }
            blocks.insert(
                laddr,
                Block { addr: laddr, instrs: idxs, succs, preds: Vec::new(), is_cond },
            );
        }

        // 3. fill preds
        let succ_pairs: Vec<(u64, u64)> = blocks
            .values()
            .flat_map(|b| b.succs.iter().map(move |&s| (b.addr, s)))
            .collect();
        for (from, to) in succ_pairs {
            if let Some(b) = blocks.get_mut(&to) {
                b.preds.push(from);
            }
        }

        let (param_count, has_return_value) = detect_signature(&instrs);

        Function { name, entry, instrs, blocks, param_count, is_void: !has_return_value }
    }

    /// Iterative dominator computation (Cooper/Harvey/Kennedy) over addresses
    /// in reverse-postorder.
    fn dominators(&self) -> HashMap<u64, u64> {
        let rpo = self.reverse_postorder();
        let rpo_index: HashMap<u64, usize> =
            rpo.iter().enumerate().map(|(i, &a)| (a, i)).collect();
        let mut idom: HashMap<u64, u64> = HashMap::new();
        idom.insert(self.entry, self.entry);

        let intersect = |mut a: u64, mut b: u64, idom: &HashMap<u64, u64>, rpo_index: &HashMap<u64, usize>| -> u64 {
            while a != b {
                while rpo_index[&a] > rpo_index[&b] {
                    a = idom[&a];
                }
                while rpo_index[&b] > rpo_index[&a] {
                    b = idom[&b];
                }
            }
            a
        };

        let mut changed = true;
        while changed {
            changed = false;
            for &addr in rpo.iter().filter(|&&a| a != self.entry) {
                let block = &self.blocks[&addr];
                let mut new_idom: Option<u64> = None;
                for &p in &block.preds {
                    if !idom.contains_key(&p) {
                        continue;
                    }
                    new_idom = Some(match new_idom {
                        None => p,
                        Some(cur) => intersect(cur, p, &idom, &rpo_index),
                    });
                }
                if let Some(ni) = new_idom {
                    if idom.get(&addr) != Some(&ni) {
                        idom.insert(addr, ni);
                        changed = true;
                    }
                }
            }
        }
        idom
    }

    fn reverse_postorder(&self) -> Vec<u64> {
        let mut visited = BTreeSet::new();
        let mut post = Vec::new();
        fn dfs(f: &Function, addr: u64, visited: &mut BTreeSet<u64>, post: &mut Vec<u64>) {
            if visited.contains(&addr) || !f.blocks.contains_key(&addr) {
                return;
            }
            visited.insert(addr);
            if let Some(b) = f.blocks.get(&addr) {
                for &s in &b.succs {
                    dfs(f, s, visited, post);
                }
            }
            post.push(addr);
        }
        dfs(self, self.entry, &mut visited, &mut post);
        post.reverse();
        post
    }

    fn dominates(idom: &HashMap<u64, u64>, a: u64, mut b: u64) -> bool {
        loop {
            if a == b {
                return true;
            }
            let next = match idom.get(&b) {
                Some(&n) => n,
                None => return false,
            };
            if next == b {
                return false;
            }
            b = next;
        }
    }

    /// Renders structured (best-effort) pseudocode for the whole function.
    pub fn render(&self) -> String {
        let idom = self.dominators();
        // back edges: succ (n -> h) where h dominates n
        let mut headers: BTreeSet<u64> = BTreeSet::new();
        for b in self.blocks.values() {
            for &s in &b.succs {
                if Self::dominates(&idom, s, b.addr) {
                    headers.insert(s);
                }
            }
        }

        let mut out = String::new();
        let ret_ty = if self.is_void { "void" } else { "int" };
        let params = if self.param_count == 0 {
            "void".to_string()
        } else {
            (1..=self.param_count)
                .map(|i| format!("int a{}", i))
                .collect::<Vec<_>>()
                .join(", ")
        };
        out.push_str(&format!("{} {}({})\n{{\n", ret_ty, self.name, params));
        let mut visited: BTreeSet<u64> = BTreeSet::new();
        self.emit_region(self.entry, None, 1, &headers, &idom, &mut visited, &mut out);
        out.push_str("}\n");
        out
    }

    /// Emits straight-line/structured code starting at `start`, stopping
    /// once it reaches `stop` (exclusive) or runs out of natural flow.
    fn emit_region(
        &self,
        start: u64,
        stop: Option<u64>,
        indent: usize,
        headers: &BTreeSet<u64>,
        idom: &HashMap<u64, u64>,
        visited: &mut BTreeSet<u64>,
        out: &mut String,
    ) {
        let pad = "    ".repeat(indent);
        let mut cur = Some(start);
        while let Some(addr) = cur {
            if Some(addr) == stop {
                return;
            }
            if !self.blocks.contains_key(&addr) || visited.contains(&addr) {
                if visited.contains(&addr) {
                    out.push_str(&format!("{}goto L{:x}; // already emitted (loop/merge)\n", pad, addr));
                }
                return;
            }
            visited.insert(addr);
            let block = &self.blocks[&addr];

            // WHILE loop: this block is a loop header with a conditional branch.
            if headers.contains(&addr) && block.is_cond && block.succs.len() == 2 {
                let (true_t, false_t) = (block.succs[0], block.succs[1]);
                let inside = if Self::dominates(idom, addr, true_t) && self.leads_back_to(true_t, addr, headers) {
                    Some((true_t, false_t, true))
                } else if Self::dominates(idom, addr, false_t) && self.leads_back_to(false_t, addr, headers) {
                    Some((false_t, true_t, false))
                } else {
                    None
                };
                if let Some((body, exit, cond_true_enters)) = inside {
                    self.emit_plain_instrs(block, &pad, out);
                    let cond_text = self.cond_text(block, cond_true_enters);
                    out.push_str(&format!("{}while ({}) {{\n", pad, cond_text));
                    self.emit_region(body, Some(addr), indent + 1, headers, idom, visited, out);
                    out.push_str(&format!("{}}}\n", pad));
                    cur = Some(exit);
                    continue;
                }
            }

            // IF / IF-ELSE structuring for simple diamonds.
            if block.is_cond && block.succs.len() == 2 {
                let (t, fth) = (block.succs[0], block.succs[1]);
                let t_single = self.single_succ(t);
                let f_single = self.single_succ(fth);

                self.emit_plain_instrs(block, &pad, out);
                let cond = self.cond_text(block, true);

                if t_single == Some(fth) {
                    // if(cond) falls straight to merge; else-body is `fth`
                    out.push_str(&format!("{}if (!({})) {{\n", pad, cond));
                    self.emit_region(fth, Some(t), indent + 1, headers, idom, visited, out);
                    out.push_str(&format!("{}}}\n", pad));
                    cur = Some(t);
                    continue;
                } else if f_single == Some(t) {
                    out.push_str(&format!("{}if ({}) {{\n", pad, cond));
                    self.emit_region(t, Some(fth), indent + 1, headers, idom, visited, out);
                    out.push_str(&format!("{}}}\n", pad));
                    cur = Some(fth);
                    continue;
                } else if let (Some(m1), Some(m2)) = (t_single, f_single) {
                    if m1 == m2 {
                        out.push_str(&format!("{}if ({}) {{\n", pad, cond));
                        self.emit_region(t, Some(m1), indent + 1, headers, idom, visited, out);
                        out.push_str(&format!("{}}} else {{\n", pad));
                        self.emit_region(fth, Some(m1), indent + 1, headers, idom, visited, out);
                        out.push_str(&format!("{}}}\n", pad));
                        cur = Some(m1);
                        continue;
                    }
                }
                // fallback: unstructured goto form
                out.push_str(&format!("{}if ({}) goto L{:x}; else goto L{:x};\n", pad, cond, t, fth));
                cur = self.fallthrough_addr(fth);
                let _ = self.fallthrough_addr(t); // keep for readability parity
                continue;
            }

            // plain block
            self.emit_plain_instrs(block, &pad, out);
            match block.succs.first() {
                Some(&s) => cur = Some(s),
                None => cur = None, // ret or end of function
            }
        }
    }

    fn cond_text(&self, block: &Block, want_true_branch: bool) -> String {
        // Walk the block's instructions in order, remembering the most
        // recent Cmp seen; when we hit the stashed __cond__ marker (from
        // the Jcc that follows it), pair them up. The Cmp and the marker
        // live on two different machine instructions (cmp; jcc), so this
        // has to span the whole block rather than one instruction's ops.
        let mut last_cmp: Option<(String, String)> = None;
        let mut cond_str = "cond".to_string();
        'outer: for &ix in &block.instrs {
            let ins = &self.instrs[ix];
            for op in &ins.ops {
                match op {
                    Instr::Cmp { lhs, rhs, .. } => {
                        last_cmp = Some((lhs.to_string(), rhs.to_string()));
                    }
                    Instr::Unknown { text } => {
                        if let Some(c) = text.strip_prefix("__cond__") {
                            if let Some((l, r)) = &last_cmp {
                                let sym = match c {
                                    "Eq" => "==", "Ne" => "!=", "Lt" => "<", "Le" => "<=",
                                    "Gt" => ">", "Ge" => ">=", "Below" => "<u", "BelowEq" => "<=u",
                                    "Above" => ">u", "AboveEq" => ">=u", other => other,
                                };
                                cond_str = format!("{} {} {}", l, sym, r);
                            }
                            break 'outer;
                        }
                    }
                    _ => {}
                }
            }
        }
        if want_true_branch {
            cond_str
        } else {
            format!("!({})", cond_str)
        }
    }

    fn emit_plain_instrs(&self, block: &Block, pad: &str, out: &mut String) {
        for &ix in &block.instrs {
            let ins = &self.instrs[ix];
            for op in &ins.ops {
                match op {
                    Instr::Nop => {}
                    Instr::CBranch { .. } | Instr::Branch { .. } => {} // handled by structuring
                    Instr::Unknown { text } if text.starts_with("__cond__") => {}
                    Instr::Cmp { .. } => {} // condition text pulled separately
                    Instr::Ret { .. } => {
                        // IR always carries `rax` on Ret (lifter.rs can't
                        // tell void from int at the single-instruction
                        // level) -- use the whole-function signature
                        // (detect_signature) to decide what to print.
                        if self.is_void {
                            out.push_str(&format!("{}return;\n", pad));
                        } else {
                            out.push_str(&format!("{}return rax;\n", pad));
                        }
                    }
                    other => out.push_str(&format!("{}{};\n", pad, other)),
                }
            }
        }
    }

    fn single_succ(&self, addr: u64) -> Option<u64> {
        self.blocks.get(&addr).and_then(|b| {
            if b.succs.len() == 1 {
                Some(b.succs[0])
            } else {
                None
            }
        })
    }

    fn fallthrough_addr(&self, addr: u64) -> Option<u64> {
        self.blocks.get(&addr).and_then(|b| b.succs.first().copied())
    }

    /// True if starting at `from` we can reach `header` again by following
    /// successors without leaving the natural loop (bounded DFS).
    fn leads_back_to(&self, from: u64, header: u64, _headers: &BTreeSet<u64>) -> bool {
        let mut seen = BTreeSet::new();
        let mut stack = vec![from];
        while let Some(a) = stack.pop() {
            if a == header {
                return true;
            }
            if !seen.insert(a) {
                continue;
            }
            if let Some(b) = self.blocks.get(&a) {
                for &s in &b.succs {
                    stack.push(s);
                }
            }
        }
        false
    }
}