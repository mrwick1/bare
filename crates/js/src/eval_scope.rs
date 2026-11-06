// Static scope resolution of identifiers. Included by `eval.rs` (`include!`), so it shares that module's imports.
//
// Problem: a name lookup walked the environment chain and searched every environment on the way (a scan, or a hash
// probe above 12 bindings). A chain of closures, as in a bundle, pays this for each read of an outer variable.
//
// Method: the first walk (`walk_program`) records the static scopes that make a runtime environment: the script
// (environment 0), a function, a block with lexical declarations, a `for`/`for-in`/`for-of` with `let` or `const`, a
// `catch` clause and a `switch` with lexical declarations. A class is a `Barrier`. Each identifier site stores the scope
// it is in. The second run of a site resolves it, once (code that runs once pays no resolution), to the number of environments between the site and the
// environment that declares the name ("hops"). Later runs go up `hops` parents and read the binding at the index that
// the last run found. A name check guards the index. Nothing else is searched.
//
// What each outcome means:
// - `0`: dynamic. The full search runs. A site behind a barrier, a site named `arguments` and a site that the walk did not
//   record are dynamic.
// - hops + index: the name is declared in the scope at `hops`, and in no nearer scope. The index is learned at the first
//   hit (`0xFFFF` until then). If the environment at `hops` has no such binding, the full search runs. That happens for a
//   name that a parameter default reads before the `var` of the same name exists.
// - `GLOBAL`: no scope of the script declares the name. Only environment 0 (a `let` of another script) and the global
//   object can hold it.
//
// Soundness: a scope lists a superset of the names that its environment can hold, and the hop count follows
// `has_lexical` and the other conditions that make an environment at run time. A wrong hop count could find a binding of
// the same name in another environment. In a debug build every resolved lookup is checked against the full search.
//
// Ideas taken from engines: V8 resolves a name at parse time to a context slot (depth, index) when no `with` or sloppy
// `eval` is in the way. QuickJS resolves it to a closure variable reference at compile time. SpiderMonkey keeps a
// `BindingKind` for each name, and it keeps a binding that no closure captures out of the environment object. This
// engine has neither `with` nor `eval`, and its environments are an arena (`envs`), so the hop count and the slot index
// are enough. Frame slots for variables that no closure captures need a different environment layout: not done.
//
// If `with` or a direct `eval` is ever added, the first walk must open a `Barrier` for the code that it can reach.
// Today the walk rejects `with` and the engine has no `eval`, so no scope is dynamic.

const UNRES: u32 = 1 << 31;
/// With `UNRES`: the site ran once, with the full search. Its second run resolves it.
const SEEN: u32 = 1 << 28;
/// A resolved local site: `LOCAL | hops << 16 | index`. An index of `0xFFFF` is "not learned yet".
const LOCAL: u32 = 1 << 30;
const MAX_HOPS: u32 = 0x3FFF;
/// A site whose name no scope of the script declares.
const GLOBAL: u32 = 1 << 29;

#[derive(Clone, Copy)]
enum RKind {
    /// The script: environment 0. Its names are the `let`, `const` and `class` of the top level.
    Root,
    /// A scope with unknown hops and names (a class). A lookup that must cross it is dynamic.
    Barrier,
    /// A function call: the function, and whether it is a function expression (it can bind its own name).
    Fn(FuncId, bool),
    Block(StmtId),
    Switch(StmtId),
    /// The `let`/`const` declaration of a `for`, `for-in` or `for-of`.
    ForVar(StmtId),
    /// A `catch` clause: its parameter.
    Catch(Option<PatId>),
}

struct RScope {
    parent: u32,
    kind: RKind,
    /// The names, built at the first search of this scope: `len` entries of `Interp::sc_pool` from `start` (`NONE` before).
    /// Sorted when there are more than 16.
    start: u32,
    len: u32,
}

/// Slots of the identifier sites of one script. The scopes that they refer to are in `Interp::sc_all`.
#[derive(Default)]
struct Slots {
    /// One word for each expression, then one for each pattern: 0, `UNRES | scope + 1` or a resolved slot.
    sites: Box<[u32]>,
}

impl Slots {
    fn new(st: &crate::ast::Stats) -> Slots {
        Slots { sites: vec![0; st.exprs + st.pats].into_boxed_slice() }
    }
}

impl<'p> Interp<'p> {
    /// The site of the pattern with this index.
    fn pat_site(&self, pat: usize) -> u32 {
        (self.prog.exprs.len() + pat) as u32
    }

    /// Open a scope in the walk. Give the result to `sc_end`.
    fn sc_begin(&mut self, kind: RKind) -> u32 {
        let saved = self.sc_cur;
        self.sc_all.push(RScope { parent: saved, kind, start: NONE, len: 0 });
        self.sc_cur = self.sc_all.len() as u32 - 1;
        saved
    }

    fn sc_end(&mut self, saved: u32) {
        self.sc_cur = saved;
    }

    /// Open the scope of the `let` or `const` declaration `v` in a loop head. A `var`, or none, opens no scope.
    fn sc_lexical_var(&mut self, v: Option<StmtId>) -> u32 {
        match v {
            Some(v) if matches!(self.prog.stmt(v).kind, StmtKind::Var { kind, .. } if kind != VarKind::Var) => self.sc_begin(RKind::ForVar(v)),
            _ => self.sc_cur,
        }
    }

    /// Record the scope of an identifier site in the walk.
    fn sc_site(&mut self, site: u32, n: u32) {
        if n != self.n_arguments {
            self.slots.sites[site as usize] = UNRES | (self.sc_cur + 1);
        }
    }

    /// The names of a pattern, as the first walk read them.
    fn pat_idents(&self, p: PatId, out: &mut Vec<u32>) {
        let prog = &self.prog;
        match prog.pat(p).kind {
            PatKind::Ident => out.push(self.pat_name[p.index()]),
            PatKind::Array(l) => prog.list(l).for_each(|q| self.pat_idents(q, out)),
            PatKind::Object(l) => prog.list(l).for_each(|q| self.pat_idents(prog.pat_prop(q).value, out)),
            PatKind::Assign { target, .. } | PatKind::Rest(target) => self.pat_idents(target, out),
            PatKind::Member(_) | PatKind::Hole => {}
        }
    }

    /// The names that a statement list declares in its own environment: `let`, `const`, `class`, and functions if `funcs`.
    fn lex_names(&mut self, l: List<Stmt>, funcs: bool, out: &mut Vec<u32>) {
        let prog = self.prog.clone();
        for s in prog.list(l) {
            match prog.stmt(s).kind {
                StmtKind::Var { kind, decls } if kind != VarKind::Var => {
                    for d in prog.list(decls) {
                        self.pat_idents(prog.decl(d).target, out);
                    }
                }
                StmtKind::Function(fid) if funcs => out.push(self.func_name[fid.index()]),
                StmtKind::Class(c) => {
                    if let Some(sp) = prog.class(c).name {
                        out.push(self.name_at(sp));
                    }
                }
                _ => {}
            }
        }
    }

    /// Append the names of a scope to `out`.
    fn sc_names(&mut self, kind: RKind, out: &mut Vec<u32>) {
        let prog = self.prog.clone();
        match kind {
            RKind::Root => self.lex_names(prog.body, false, out),
            RKind::Barrier => {}
            RKind::Fn(fid, is_expr) => {
                let f = *prog.func(fid);
                if is_expr && f.name.is_some() && f.kind == FuncKind::Normal {
                    out.push(self.func_name[fid.index()]);
                }
                for p in prog.list(f.params) {
                    self.pat_idents(p, out);
                }
                out.extend_from_slice(&self.scopes[fid.index()].vars);
                if let Body::Block(l) = f.body {
                    self.lex_names(l, true, out);
                }
            }
            RKind::Block(s) => {
                if let StmtKind::Block(l) = prog.stmt(s).kind {
                    self.lex_names(l, true, out);
                }
            }
            RKind::Switch(s) => {
                if let StmtKind::Switch { cases, .. } = prog.stmt(s).kind {
                    for c in prog.list(cases) {
                        self.lex_names(prog.case(c).body, true, out);
                    }
                }
            }
            RKind::ForVar(s) => {
                if let StmtKind::Var { decls, .. } = prog.stmt(s).kind {
                    for d in prog.list(decls) {
                        self.pat_idents(prog.decl(d).target, out);
                    }
                }
            }
            RKind::Catch(p) => {
                if let Some(p) = p {
                    self.pat_idents(p, out);
                }
            }
        }
    }

    fn sc_declares(&mut self, s: u32, n: u32) -> bool {
        if self.sc_all[s as usize].start == NONE {
            let mut pool = std::mem::take(&mut self.sc_pool);
            let start = pool.len();
            self.sc_names(self.sc_all[s as usize].kind, &mut pool);
            if pool.len() - start > 16 {
                pool[start..].sort_unstable();
                let mut w = start;
                for r in start..pool.len() {
                    if r == start || pool[r] != pool[w - 1] {
                        pool[w] = pool[r];
                        w += 1;
                    }
                }
                pool.truncate(w);
            }
            let sc = &mut self.sc_all[s as usize];
            sc.start = start as u32;
            sc.len = (pool.len() - start) as u32;
            self.sc_pool = pool;
        }
        let sc = &self.sc_all[s as usize];
        let v = &self.sc_pool[sc.start as usize..(sc.start + sc.len) as usize];
        if v.len() > 16 { v.binary_search(&n).is_ok() } else { v.contains(&n) }
    }

    /// Resolve the name `n` of a site in `scope`, once.
    #[cold]
    fn sc_resolve(&mut self, n: u32, mut scope: u32) -> u32 {
        let mut hops = 0;
        loop {
            if matches!(self.sc_all[scope as usize].kind, RKind::Barrier) {
                return 0;
            }
            if self.sc_declares(scope, n) {
                return if hops < MAX_HOPS { LOCAL | hops << 16 | 0xFFFF } else { 0 };
            }
            let p = self.sc_all[scope as usize].parent;
            if p == NONE {
                return GLOBAL;
            }
            hops += 1;
            scope = p;
        }
    }

    /// The binding that the name `n` means at an identifier site: its environment and index, or `None` for the global object.
    #[inline(always)]
    fn find_name(&mut self, n: u32, site: u32) -> Option<(u32, usize)> {
        let r = self.find_name_fast(n, site);
        #[cfg(debug_assertions)]
        assert_eq!(r, self.lookup(n), "static scope resolution of '{}' differs from the search", self.names[n as usize]);
        r
    }

    #[inline(always)]
    fn find_name_fast(&mut self, n: u32, site: u32) -> Option<(u32, usize)> {
        let s = self.slots.sites[site as usize];
        if s & LOCAL != 0 {
            let mut e = self.env;
            for _ in 0..(s >> 16) & MAX_HOPS {
                match self.envs.get(e as usize) {
                    Some(x) => e = x.parent,
                    None => return self.find_name_slow(n, site, s),
                }
            }
            if let Some(b) = self.envs.get(e as usize).and_then(|x| x.bindings.get((s & 0xFFFF) as usize))
                && b.name == n
            {
                return Some((e, (s & 0xFFFF) as usize));
            }
        } else if s == GLOBAL && self.envs[0].bindings.is_empty() {
            return None;
        }
        self.find_name_slow(n, site, s)
    }

    /// The sites that are not a plain hit: first runs, dynamic sites, a learned index to find, a name beyond environment 0.
    #[inline(never)]
    fn find_name_slow(&mut self, n: u32, site: u32, mut s: u32) -> Option<(u32, usize)> {
        if s & UNRES != 0 {
            if s & SEEN == 0 {
                self.slots.sites[site as usize] = s | SEEN;
                return self.lookup(n);
            }
            s = self.sc_resolve(n, (s & !(UNRES | SEEN)) - 1);
        }
        let mut found = None;
        if s == GLOBAL {
            found = self.find_in(0, n).map(|i| (0, i));
            if found.is_none() {
                self.slots.sites[site as usize] = s;
                return None;
            }
        } else if s & LOCAL != 0 {
            let mut e = self.env;
            for _ in 0..(s >> 16) & MAX_HOPS {
                e = self.envs[e as usize].parent;
                if e == NONE {
                    break;
                }
            }
            if e != NONE
                && let Some(i) = self.find_in(e, n)
            {
                found = Some((e, i));
                if i < 0xFFFF {
                    s = (s & !0xFFFF) | i as u32;
                }
            }
        }
        self.slots.sites[site as usize] = s;
        found.or_else(|| self.lookup(n))
    }
}
