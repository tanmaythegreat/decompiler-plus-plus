// cfg.rs — basic blocks, dominators, post-dominators, structuring.
//
// Fixes over the original:
//
//  * The old structurer decided an `if`'s merge point with the ad-hoc
//    rule "does one branch have a single successor equal to the other".
//    Two `if`s in a row (`clamp` in the sample program) matched none of
//    its three shapes, so it fell through to `if (c) goto A; else goto B;`
//    and then continued from B's *successor*, dropping every statement in
//    B and everything after it. The whole tail of `clamp` vanished from
//    the output. Merge points now come from the immediate post-dominator,
//    which is what that rule was approximating.
//  * The old renderer emitted `goto Lxxxx` but never emitted a single
//    label, so its output could not compile even in principle.
//  * `reverse_postorder` recursed once per basic block and would blow the
//    stack on a large function; it's an explicit worklist now.
//  * `dominators` indexed `rpo_index[&a]` and `idom[&a]` unguarded, so a
//    block reachable only through an unstructured edge panicked the
//    process.
//  * Loop bodies are real natural loops, and `break`/`continue` are
//    emitted instead of jumping out with a goto.

use crate::ir::*;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

pub struct Block {
    pub addr: u64,
    pub instrs: Vec<usize>,
    pub succs: Vec<u64>,
    pub preds: Vec<u64>,
    pub is_cond: bool,
    /// condition of the terminating branch, if any
    pub cond: Option<Expr>,
    pub ends_return: bool,
}

/// Structured output tree.
pub enum CNode {
    Stmts(Vec<Stmt>),
    If { cond: Expr, then_: Vec<CNode>, else_: Vec<CNode> },
    While { cond: Expr, body: Vec<CNode> },
    DoWhile { body: Vec<CNode>, cond: Expr },
    Forever { body: Vec<CNode> },
    Break,
    Continue,
    Goto(u64),
    Label(u64),
}

pub struct Cfg {
    pub blocks: Vec<Block>,
    pub index: HashMap<u64, usize>,
    pub entry: usize,
}

impl Cfg {
    pub fn build(instrs: &[LiftedInsn]) -> Cfg {
        let entry_addr = instrs.first().map(|i| i.addr).unwrap_or(0);
        let addr_index: HashMap<u64, usize> =
            instrs.iter().enumerate().map(|(i, ins)| (ins.addr, i)).collect();

        let mut leaders: BTreeSet<u64> = BTreeSet::new();
        leaders.insert(entry_addr);
        for (i, ins) in instrs.iter().enumerate() {
            if ins.is_block_end {
                for &t in &ins.targets {
                    if addr_index.contains_key(&t) {
                        leaders.insert(t);
                    }
                }
                if let Some(next) = instrs.get(i + 1) {
                    leaders.insert(next.addr);
                }
            }
        }

        let leader_vec: Vec<u64> = leaders.into_iter().collect();
        let mut blocks: Vec<Block> = Vec::new();
        let mut index: HashMap<u64, usize> = HashMap::new();

        for (li, &laddr) in leader_vec.iter().enumerate() {
            let next_leader = leader_vec.get(li + 1).copied();
            let Some(&start_idx) = addr_index.get(&laddr) else { continue };
            let mut idxs = Vec::new();
            let mut j = start_idx;
            while j < instrs.len() {
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
            }
            if idxs.is_empty() {
                continue;
            }

            let last_idx = *idxs.last().unwrap();
            let last = &instrs[last_idx];
            let mut succs = Vec::new();
            let mut is_cond = false;
            let mut cond = None;
            let mut ends_return = false;

            let branch = last.stmts.iter().find(|s| {
                matches!(s, Stmt::If { .. } | Stmt::Goto(_) | Stmt::Return(_))
            });
            match branch {
                Some(Stmt::If { cond: c, target }) => {
                    succs.push(*target);
                    if let Some(next) = instrs.get(last_idx + 1) {
                        succs.push(next.addr);
                    }
                    is_cond = true;
                    cond = Some(c.clone());
                }
                Some(Stmt::Goto(t)) => succs.push(*t),
                Some(Stmt::Return(_)) => ends_return = true,
                _ => {
                    if last.falls_through {
                        if let Some(next) = instrs.get(last_idx + 1) {
                            succs.push(next.addr);
                        }
                    }
                }
            }
            // drop edges to addresses outside this function's range
            succs.retain(|t| addr_index.contains_key(t));

            index.insert(laddr, blocks.len());
            blocks.push(Block {
                addr: laddr,
                instrs: idxs,
                succs,
                preds: Vec::new(),
                is_cond,
                cond,
                ends_return,
            });
        }

        let pairs: Vec<(u64, u64)> = blocks
            .iter()
            .flat_map(|b| b.succs.iter().map(move |&s| (b.addr, s)))
            .collect();
        for (from, to) in pairs {
            if let Some(&i) = index.get(&to) {
                blocks[i].preds.push(from);
            }
        }

        let entry = index.get(&entry_addr).copied().unwrap_or(0);
        Cfg { blocks, index, entry }
    }

    fn succ_idx(&self, b: usize) -> Vec<usize> {
        self.blocks[b].succs.iter().filter_map(|s| self.index.get(s).copied()).collect()
    }
    fn pred_idx(&self, b: usize) -> Vec<usize> {
        self.blocks[b].preds.iter().filter_map(|s| self.index.get(s).copied()).collect()
    }

    /// Iterative DFS reverse-postorder. The original recursed and would
    /// overflow the stack on large functions.
    fn rpo(&self) -> Vec<usize> {
        if self.blocks.is_empty() {
            return vec![];
        }
        let mut visited = vec![false; self.blocks.len()];
        let mut post = Vec::new();
        let mut stack: Vec<(usize, usize)> = vec![(self.entry, 0)];
        visited[self.entry] = true;
        while let Some((n, ci)) = stack.pop() {
            let succs = self.succ_idx(n);
            if ci < succs.len() {
                stack.push((n, ci + 1));
                let s = succs[ci];
                if !visited[s] {
                    visited[s] = true;
                    stack.push((s, 0));
                }
            } else {
                post.push(n);
            }
        }
        post.reverse();
        post
    }

    /// Cooper/Harvey/Kennedy iterative dominators.
    pub fn dominators(&self) -> Vec<Option<usize>> {
        let order = self.rpo();
        let n = self.blocks.len();
        let mut pos = vec![usize::MAX; n];
        for (i, &b) in order.iter().enumerate() {
            pos[b] = i;
        }
        let mut idom: Vec<Option<usize>> = vec![None; n];
        if order.is_empty() {
            return idom;
        }
        idom[self.entry] = Some(self.entry);

        let intersect = |mut a: usize, mut b: usize, idom: &Vec<Option<usize>>| -> Option<usize> {
            let mut guard = 0;
            while a != b {
                guard += 1;
                if guard > 4 * n + 8 {
                    return None;
                }
                // A root is its own immediate dominator, so walking up from
                // one never terminates on its own — the outer guard never
                // gets a chance to fire.
                while pos[a] > pos[b] {
                    let up = idom[a]?;
                    if up == a {
                        return None;
                    }
                    a = up;
                }
                while pos[b] > pos[a] {
                    let up = idom[b]?;
                    if up == b {
                        return None;
                    }
                    b = up;
                }
            }
            Some(a)
        };

        // The fixpoint is only guaranteed to settle on a reducible graph
        // whose blocks all reach the exit. Optimised code has neither
        // property (an infinite loop has no path to a `ret`), and there the
        // fallback inside `intersect` can flip an entry back and forth
        // forever. Cap the rounds: a partial dominator tree structures a
        // little worse, a hang structures nothing at all.
        let mut rounds = 0;
        let mut changed = true;
        while changed {
            rounds += 1;
            if rounds > 2 * n + 16 {
                break;
            }
            changed = false;
            for &b in order.iter() {
                if b == self.entry {
                    continue;
                }
                let mut new: Option<usize> = None;
                for p in self.pred_idx(b) {
                    if pos[p] == usize::MAX || idom[p].is_none() {
                        continue;
                    }
                    new = Some(match new {
                        None => p,
                        Some(cur) => match intersect(cur, p, &idom) {
                            Some(x) => x,
                            None => cur,
                        },
                    });
                }
                if let Some(ni) = new {
                    if idom[b] != Some(ni) {
                        idom[b] = Some(ni);
                        changed = true;
                    }
                }
            }
        }
        idom
    }

    /// Post-dominators, via the same algorithm on the reverse graph.
    /// This is what makes correct `if`/`if-else` merge points possible.
    pub fn post_dominators(&self) -> Vec<Option<usize>> {
        let n = self.blocks.len();
        let exits: Vec<usize> =
            (0..n).filter(|&b| self.succ_idx(b).is_empty()).collect();
        let mut idom: Vec<Option<usize>> = vec![None; n];
        if exits.is_empty() {
            return idom;
        }

        // reverse postorder of the reverse graph, seeded from every exit
        let mut visited = vec![false; n];
        let mut post = Vec::new();
        for &e in &exits {
            if visited[e] {
                continue;
            }
            let mut stack: Vec<(usize, usize)> = vec![(e, 0)];
            visited[e] = true;
            while let Some((x, ci)) = stack.pop() {
                let preds = self.pred_idx(x);
                if ci < preds.len() {
                    stack.push((x, ci + 1));
                    let p = preds[ci];
                    if !visited[p] {
                        visited[p] = true;
                        stack.push((p, 0));
                    }
                } else {
                    post.push(x);
                }
            }
        }
        post.reverse();
        let mut pos = vec![usize::MAX; n];
        for (i, &b) in post.iter().enumerate() {
            pos[b] = i;
        }
        for &e in &exits {
            idom[e] = Some(e);
        }

        let intersect = |mut a: usize, mut b: usize, idom: &Vec<Option<usize>>| -> Option<usize> {
            let mut guard = 0;
            while a != b {
                guard += 1;
                if guard > 4 * n + 8 {
                    return None;
                }
                // A root is its own immediate dominator, so walking up from
                // one never terminates on its own — the outer guard never
                // gets a chance to fire.
                while pos[a] > pos[b] {
                    let up = idom[a]?;
                    if up == a {
                        return None;
                    }
                    a = up;
                }
                while pos[b] > pos[a] {
                    let up = idom[b]?;
                    if up == b {
                        return None;
                    }
                    b = up;
                }
            }
            Some(a)
        };

        // The fixpoint is only guaranteed to settle on a reducible graph
        // whose blocks all reach the exit. Optimised code has neither
        // property (an infinite loop has no path to a `ret`), and there the
        // fallback inside `intersect` can flip an entry back and forth
        // forever. Cap the rounds: a partial dominator tree structures a
        // little worse, a hang structures nothing at all.
        let mut rounds = 0;
        let mut changed = true;
        while changed {
            rounds += 1;
            if rounds > 2 * n + 16 {
                break;
            }
            changed = false;
            for &b in post.iter() {
                if exits.contains(&b) {
                    continue;
                }
                let mut new: Option<usize> = None;
                for s in self.succ_idx(b) {
                    if pos[s] == usize::MAX || idom[s].is_none() {
                        continue;
                    }
                    new = Some(match new {
                        None => s,
                        Some(cur) => match intersect(cur, s, &idom) {
                            Some(x) => x,
                            None => cur,
                        },
                    });
                }
                if let Some(ni) = new {
                    if idom[b] != Some(ni) {
                        idom[b] = Some(ni);
                        changed = true;
                    }
                }
            }
        }
        idom
    }

    pub fn dominates(idom: &[Option<usize>], a: usize, mut b: usize) -> bool {
        let mut guard = 0;
        loop {
            if a == b {
                return true;
            }
            guard += 1;
            if guard > idom.len() + 4 {
                return false;
            }
            match idom[b] {
                Some(n) if n != b => b = n,
                _ => return false,
            }
        }
    }
}

// ------------------------------------------------------------ structuring

pub struct Loop {
    pub header: usize,
    pub body: HashSet<usize>,
    pub latches: Vec<usize>,
    pub follow: Option<usize>,
}

pub struct Structurer<'a> {
    cfg: &'a Cfg,
    instrs: &'a [LiftedInsn],
    #[allow(dead_code)]
    idom: Vec<Option<usize>>,
    ipdom: Vec<Option<usize>>,
    loops: HashMap<usize, Loop>,
    emitted: HashSet<usize>,
    /// second pass: emit a label in front of every block something jumps to
    label_pass: bool,
    pub goto_targets: HashSet<u64>,
}

impl<'a> Structurer<'a> {
    pub fn new(cfg: &'a Cfg, instrs: &'a [LiftedInsn]) -> Structurer<'a> {
        let idom = cfg.dominators();
        let ipdom = cfg.post_dominators();
        let loops = find_loops(cfg, &idom);
        Structurer {
            cfg,
            instrs,
            idom,
            ipdom,
            loops,
            emitted: HashSet::new(),
            label_pass: false,
            goto_targets: HashSet::new(),
        }
    }

    pub fn run(&mut self) -> Vec<CNode> {
        // Two passes: the first discovers which addresses are jumped to,
        // the second emits the matching labels. A `goto` without a label is
        // exactly the defect that made the original output uncompilable,
        // and the targets are not known until the walk is finished.
        self.walk();
        self.emitted.clear();
        self.label_pass = true;
        self.walk()
    }

    fn walk(&mut self) -> Vec<CNode> {
        let mut out = Vec::new();
        if self.cfg.blocks.is_empty() {
            return out;
        }
        self.emit_seq(Some(self.cfg.entry), None, &mut Vec::new(), &mut out);

        // Anything the structured walk never reached still has to appear,
        // or code would silently vanish from the output — the failure
        // mode the old `clamp` rendering had.
        let mut leftovers: Vec<usize> =
            (0..self.cfg.blocks.len()).filter(|b| !self.emitted.contains(b)).collect();
        leftovers.sort_by_key(|&b| self.cfg.blocks[b].addr);
        for b in leftovers {
            if self.emitted.contains(&b) {
                continue;
            }
            let addr = self.cfg.blocks[b].addr;
            self.goto_targets.insert(addr);
            if !self.label_pass {
                out.push(CNode::Label(addr));
            }
            self.emit_seq(Some(b), None, &mut Vec::new(), &mut out);
        }
        out
    }

    fn block_stmts(&self, b: usize) -> Vec<Stmt> {
        let mut out = Vec::new();
        for &ix in &self.cfg.blocks[b].instrs {
            for st in &self.instrs[ix].stmts {
                match st {
                    Stmt::Nop | Stmt::If { .. } | Stmt::Goto(_) => {}
                    Stmt::Assign { dst, .. } if is_frame_reg(dst) => {}
                    other => out.push(other.clone()),
                }
            }
        }
        out
    }

    fn emit_seq(
        &mut self,
        start: Option<usize>,
        stop: Option<usize>,
        loop_stack: &mut Vec<(usize, Option<usize>)>,
        out: &mut Vec<CNode>,
    ) {
        let mut cur = start;
        loop {
            let Some(b) = cur else { return };
            if Some(b) == stop {
                return;
            }
            // a jump back into an enclosing loop
            if let Some(&(h, follow)) = loop_stack.last() {
                if b == h {
                    out.push(CNode::Continue);
                    return;
                }
                if Some(b) == follow && stop != Some(b) {
                    out.push(CNode::Break);
                    return;
                }
            }
            if self.emitted.contains(&b) {
                let addr = self.cfg.blocks[b].addr;
                self.goto_targets.insert(addr);
                out.push(CNode::Goto(addr));
                return;
            }
            self.emitted.insert(b);
            if self.label_pass && self.goto_targets.contains(&self.cfg.blocks[b].addr) {
                out.push(CNode::Label(self.cfg.blocks[b].addr));
            }

            // ---- loop ------------------------------------------------
            if self.loops.contains_key(&b) {
                let (body_nodes, follow) = self.emit_loop(b, loop_stack);
                out.extend(body_nodes);
                cur = follow;
                continue;
            }

            // ---- conditional ------------------------------------------
            if self.cfg.blocks[b].is_cond && self.cfg.blocks[b].succs.len() == 2 {
                let stmts = self.block_stmts(b);
                if !stmts.is_empty() {
                    out.push(CNode::Stmts(stmts));
                }
                let cond = self.cfg.blocks[b].cond.clone().unwrap_or(Expr::Unknown("cond".into()));
                let t = self.cfg.index[&self.cfg.blocks[b].succs[0]];
                let f = self.cfg.index[&self.cfg.blocks[b].succs[1]];
                // the merge point is the immediate post-dominator
                let follow = self.ipdom[b].filter(|&p| p != b);

                let mut then_ = Vec::new();
                let mut else_ = Vec::new();
                let (cond, tb, fb) = if Some(t) == follow {
                    (cond.negated(), f, t)
                } else {
                    (cond, t, f)
                };
                self.emit_seq(Some(tb), follow.or(stop), loop_stack, &mut then_);
                if Some(fb) != follow {
                    self.emit_seq(Some(fb), follow.or(stop), loop_stack, &mut else_);
                }
                out.push(CNode::If { cond, then_, else_ });
                cur = follow;
                if follow.is_none() {
                    return;
                }
                continue;
            }

            // ---- straight-line ----------------------------------------
            let stmts = self.block_stmts(b);
            if !stmts.is_empty() {
                out.push(CNode::Stmts(stmts));
            }
            if self.cfg.blocks[b].ends_return {
                return;
            }
            cur = self.cfg.blocks[b].succs.first().and_then(|s| self.cfg.index.get(s).copied());
            if cur.is_none() {
                return;
            }
        }
    }

    fn emit_loop(
        &mut self,
        h: usize,
        loop_stack: &mut Vec<(usize, Option<usize>)>,
    ) -> (Vec<CNode>, Option<usize>) {
        let (body_set, latches, follow) = {
            let l = &self.loops[&h];
            (l.body.clone(), l.latches.clone(), l.follow)
        };
        let mut out = Vec::new();
        loop_stack.push((h, follow));

        let hdr_stmts = self.block_stmts(h);
        let hdr_cond = self.cfg.blocks[h].cond.clone();
        let hdr_is_cond = self.cfg.blocks[h].is_cond && self.cfg.blocks[h].succs.len() == 2;

        if hdr_is_cond {
            let t = self.cfg.index[&self.cfg.blocks[h].succs[0]];
            let f = self.cfg.index[&self.cfg.blocks[h].succs[1]];
            let (entry, cond) = if body_set.contains(&t) && Some(f) == follow {
                (t, hdr_cond.clone().unwrap())
            } else if body_set.contains(&f) && Some(t) == follow {
                (f, hdr_cond.clone().unwrap().negated())
            } else if body_set.contains(&t) {
                (t, hdr_cond.clone().unwrap())
            } else {
                (f, hdr_cond.clone().unwrap().negated())
            };

            let mut body = Vec::new();
            self.emit_seq(Some(entry), Some(h), loop_stack, &mut body);

            if hdr_stmts.is_empty() {
                // clean `while (cond)`
                out.push(CNode::While { cond, body });
            } else {
                // The header does real work, and that work has to run on
                // every iteration including the first, so it cannot be
                // hoisted above the loop the way the original did.
                let mut inner = vec![CNode::Stmts(hdr_stmts)];
                inner.push(CNode::If {
                    cond: cond.negated(),
                    then_: vec![CNode::Break],
                    else_: vec![],
                });
                inner.extend(body);
                out.push(CNode::Forever { body: inner });
            }
        } else {
            // header isn't the test: a do/while or an irreducible shape
            let latch_cond = latches
                .iter()
                .find(|&&l| self.cfg.blocks[l].is_cond)
                .and_then(|&l| self.cfg.blocks[l].cond.clone());
            let mut body = Vec::new();
            if !hdr_stmts.is_empty() {
                body.push(CNode::Stmts(hdr_stmts));
            }
            let next = self.cfg.blocks[h].succs.first().and_then(|s| self.cfg.index.get(s).copied());
            self.emit_seq(next, Some(h), loop_stack, &mut body);
            match latch_cond {
                Some(c) => out.push(CNode::DoWhile { body, cond: c }),
                None => out.push(CNode::Forever { body }),
            }
        }

        loop_stack.pop();
        (out, follow)
    }
}

fn is_frame_reg(e: &Expr) -> bool {
    matches!(e, Expr::Reg(r) if r.full == "rsp" || r.full == "rbp")
}

fn find_loops(cfg: &Cfg, idom: &[Option<usize>]) -> HashMap<usize, Loop> {
    let mut loops: HashMap<usize, Loop> = HashMap::new();
    for b in 0..cfg.blocks.len() {
        for s in cfg.succ_idx(b) {
            if Cfg::dominates(idom, s, b) {
                // back edge b -> s
                let e = loops.entry(s).or_insert_with(|| Loop {
                    header: s,
                    body: HashSet::new(),
                    latches: Vec::new(),
                    follow: None,
                });
                e.latches.push(b);
            }
        }
    }
    // natural loop body: everything that reaches a latch without leaving
    // through the header
    for (&h, l) in loops.iter_mut() {
        let mut body: HashSet<usize> = HashSet::new();
        body.insert(h);
        let mut stack: Vec<usize> = l.latches.clone();
        while let Some(n) = stack.pop() {
            if body.insert(n) {
                for p in cfg.pred_idx(n) {
                    if p != h {
                        stack.push(p);
                    }
                }
            }
        }
        l.body = body;
    }
    // follow node: first successor of a body block that lies outside
    let keys: Vec<usize> = loops.keys().copied().collect();
    for h in keys {
        let body = loops[&h].body.clone();
        let mut cands: BTreeMap<u64, usize> = BTreeMap::new();
        // prefer an exit straight out of the header (a `while` loop)
        for s in cfg.succ_idx(h) {
            if !body.contains(&s) {
                cands.insert(cfg.blocks[s].addr, s);
            }
        }
        if cands.is_empty() {
            for &n in &body {
                for s in cfg.succ_idx(n) {
                    if !body.contains(&s) {
                        cands.insert(cfg.blocks[s].addr, s);
                    }
                }
            }
        }
        loops.get_mut(&h).unwrap().follow = cands.values().next().copied();
    }
    loops
}
