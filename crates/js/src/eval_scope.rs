// Static scope resolution of identifiers. Included by `eval.rs` (`include!`), so it shares that module's imports.
//
// Problem: a name lookup walked the environment chain and searched every environment on the way (a scan, or a hash
// probe above 12 bindings). A chain of closures, as in a bundle, pays this for each read of an outer variable.
//
// Method: the first walk (`walk_program`) records the static scopes that make a runtime environment: the script
// (environment 0), a function, a block with lexical declarations, a `for`/`for-in`/`for-of` with `let` or `const`, a
// `catch` clause and a `switch` with lexical declarations. A class and the body of a `with` are a `Barrier`. Each identifier site stores the scope
// it is in. The second run of a site resolves it, once (code that runs once pays no resolution), to the number of environments between the site and the
// environment that declares the name ("hops"). Later runs go up `hops` parents and read the binding at the index that
// the last run found. A name check guards the index. Nothing else is searched.
//
// What each outcome means:
// - `0`: dynamic. The full search runs (it also asks the object environments of `with`). A site behind a barrier, a site named `arguments` and a site that the walk did not
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
// engine has no `eval`, and its environments are an arena (`envs`), so the hop count and the slot index are enough, except behind
// a `with`. A function that has no inner function or class keeps its variables in a frame, not in an environment (see
// "Frame slots" below). A function with an inner function or class keeps its environment: not done.
//
// `with` opens a `Barrier` for its body (`eval_with.rs`): a site that must leave the body is dynamic, because an object environment is
// in the chain and a hop count cannot see it. A site that a scope inside the body declares is resolved. The function that holds the
// `with` has no frame slots (`FL_NOFR`). A direct `eval` would have to open a `Barrier` for the code that it can reach. The engine has
// none.

const UNRES: u32 = 1 << 31;
/// With `UNRES`: the site ran once, with the full search. Its second run resolves it.
const SEEN: u32 = 1 << 28;
/// A resolved local site: `LOCAL | hops << 16 | index`. An index of `0xFFFF` is "not learned yet".
const LOCAL: u32 = 1 << 30;
const MAX_HOPS: u32 = 0x3FFF;
/// A site whose name no scope of the script declares.
const GLOBAL: u32 = 1 << 29;
/// A site in a function with frame slots, and the slot of its binding: `FRAME | slot`. The slot is relative to `Interp::fp`.
const FRAME: u32 = 1 << 27;
/// A pattern site of a `let`, `const` or `catch` parameter that is not in a function with frame slots.
const NOFRAME: u32 = 1 << 26;
/// The environment that holds the frames (`Interp::new`).
const FRAME_ENV: u32 = 1;
/// What the first walk saw in a function (`FScope::fl`). `FL_NOFR`: something that keeps the function out of frame slots: an inner
/// function or class, `new.target`, `super`, `delete name` or a `with` statement. `FL_THIS`: it names `this`. `FL_EAGER`: it has a loop or a call, so
/// one call can be long or recursive.
const FL_NOFR: u8 = 1;
const FL_THIS: u8 = 2;
const FL_EAGER: u8 = 4;
/// The most frame slots of one function. A larger one keeps its environment.
const FR_MAX: usize = 255;
/// In `stmt_label`: a block, a `switch` or a loop head with `let` or `const` in frame slots: `FR_MARK | base << 12 | len`. A `try`
/// whose `catch` is in frame slots: `FR_MARK`. No other statement uses `stmt_label` at these places.
const FR_MARK: u32 = 1 << 31;

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
    /// A `catch` clause: the `try` statement (`catch_param` gives the parameter).
    Catch(StmtId),
}

struct RScope {
    parent: u32,
    kind: RKind,
    /// The names, built at the first search of this scope: `len` entries of `Interp::sc_pool` from `start` (`NONE` before).
    /// Sorted when there are more than 16.
    start: u32,
    len: u32,
    /// A scope of a function with frame slots (`fr_layout`): it makes no environment, and its names are in the frame from slot `base`.
    /// `NONE` for any other scope.
    base: u32,
    /// The function that the scope is in (an index of `Interp::scopes`; the script for a scope outside functions).
    func: u32,
    /// The scope is the body of a `with` (a `Barrier` too). The bytecode compiler refuses a function inside it.
    with: bool,
    /// The scope is in the tree of a lazy function (`lazy_load`). It follows the scopes of every script that was walked before it.
    lazy: bool,
}

/// Slots of the identifier sites of one script. The scopes that they refer to are in `Interp::sc_all`.
#[derive(Default)]
struct Slots {
    /// One word for each expression, then one for each pattern: 0, `UNRES | scope + 1` or a resolved slot. A string literal or
    /// member name site holds a position in `Interp::site_strs` instead. A number literal holds its value + 1 (`number_literal`).
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
        self.sc_all.push(RScope { parent: saved, kind, start: NONE, len: 0, base: NONE, func: self.sc_fn as u32, with: false, lazy: self.sc_lazy });
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
            self.cc.slots.sites[site as usize] = UNRES | (self.sc_cur + 1);
        }
    }

    /// The names of a pattern, as the first walk read them.
    fn pat_idents(&self, p: PatId, out: &mut Vec<u32>) {
        let prog = &self.prog;
        match prog.pat(p).kind {
            PatKind::Ident => out.push(self.cc.cold.pat_name[p.index()]),
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
                StmtKind::Function(fid) if funcs => out.push(self.cc.cold.func_name[fid.index()]),
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
            RKind::Root if prog.module.is_some() => {}
            RKind::Root => self.lex_names(prog.body, false, out),
            RKind::Barrier => {}
            RKind::Fn(fid, is_expr) => {
                let f = *prog.func(fid);
                if is_expr && f.name.is_some() && f.kind == FuncKind::Normal {
                    out.push(self.cc.cold.func_name[fid.index()]);
                }
                for p in prog.list(f.params) {
                    self.pat_idents(p, out);
                }
                out.extend_from_slice(&self.cc.scopes[fid.index()].vars);
                if let Some(m) = prog.module.as_ref().filter(|m| m.func == fid) {
                    for i in &m.imports {
                        out.push(self.intern_name(&prog.name(i.local)));
                    }
                }
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
            RKind::Catch(s) => {
                if let Some(p) = self.catch_param(s) {
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

    /// The parameter of the `catch` clause of the `try` statement `s`.
    fn catch_param(&self, s: StmtId) -> Option<PatId> {
        match self.prog.stmt(s).kind {
            StmtKind::Try { handler: Some(h), .. } => self.prog.catch(h).param,
            _ => None,
        }
    }

    /// The frame slot of the name `n` in the frame scope `s`.
    fn sc_slot(&self, s: u32, n: u32) -> Option<u32> {
        let sc = &self.sc_all[s as usize];
        self.sc_pool[sc.start as usize..(sc.start + sc.len) as usize].iter().position(|&x| x == n).map(|i| sc.base + i as u32)
    }

    /// Resolve the name `n` of a site in `scope`, once. A frame scope makes no environment, so it adds no hop.
    #[cold]
    fn sc_resolve(&mut self, n: u32, mut scope: u32) -> u32 {
        let mut hops = 0;
        loop {
            if matches!(self.sc_all[scope as usize].kind, RKind::Barrier) {
                return 0;
            }
            let frame = self.sc_all[scope as usize].base != NONE;
            if frame {
                if let Some(slot) = self.sc_slot(scope, n) {
                    return FRAME | slot;
                }
            } else if self.sc_declares(scope, n) {
                return if hops < MAX_HOPS { LOCAL | hops << 16 | 0xFFFF } else { 0 };
            }
            let p = self.sc_all[scope as usize].parent;
            if p == NONE {
                return GLOBAL;
            }
            hops += !frame as u32;
            scope = p;
        }
    }

    /// The binding that the name `n` means at an identifier site: its environment and index, or `None` for the global object.
    #[inline(always)]
    fn find_name(&mut self, n: u32, site: u32) -> Option<(u32, usize)> {
        let r = self.find_name_fast(n, site);
        // A frame slot is in no environment of the chain, so the search cannot show it.
        // A dynamic site may meet an object environment (`eval_with.rs`): the plain search does not know it.
        #[cfg(debug_assertions)]
        {
            let s = self.cc.slots.sites[site as usize];
            if r.is_none_or(|(e, _)| e != FRAME_ENV) && !(self.with_any && s & (LOCAL | FRAME) == 0 && s != GLOBAL) {
                assert_eq!(r, self.lookup(n), "static scope resolution of '{}' differs from the search", self.names[n as usize]);
            }
        }
        r
    }

    /// The binding of a plain hit of `find_name_fast`, read once: a frame slot first (56% of the sites that run on the journey, 70%
    /// on the typing page), then an environment (0 hops: 36% and 24%; 1 hop: 4% and 3%; more: 2.2% and 0.3%). `None`: ask `find_name`.
    #[inline(always)]
    fn fast_binding(&mut self, n: u32, site: u32) -> Option<&mut Binding> {
        let s = self.cc.slots.sites[site as usize];
        let (e, i) = if s & (UNRES | LOCAL | FRAME) == FRAME {
            (FRAME_ENV, self.fp + (s & 0xFFFF) as usize)
        } else if s & LOCAL != 0 {
            let mut e = self.env;
            for _ in 0..(s >> 16) & MAX_HOPS {
                e = self.envs.get(e as usize)?.parent;
            }
            (e, (s & 0xFFFF) as usize)
        } else {
            return None;
        };
        #[cfg(debug_assertions)]
        if e != FRAME_ENV && self.envs.get(e as usize).and_then(|x| x.bindings.get(i)).is_some_and(|b| b.name == n) {
            assert_eq!(Some((e, i)), self.lookup(n), "static scope resolution of '{}' differs from the search", self.names[n as usize]);
        }
        self.envs.get_mut(e as usize)?.bindings.get_mut(i).filter(|b| b.name == n)
    }

    #[inline(always)]
    fn find_name_fast(&mut self, n: u32, site: u32) -> Option<(u32, usize)> {
        let s = self.cc.slots.sites[site as usize];
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
        } else if s & (UNRES | FRAME) == FRAME {
            let i = self.fp + (s & 0xFFFF) as usize;
            if self.envs[FRAME_ENV as usize].bindings.get(i).is_some_and(|b| b.name == n) {
                return Some((FRAME_ENV, i));
            }
        }
        self.find_name_slow(n, site, s)
    }

    /// The sites that are not a plain hit: first runs, dynamic sites, a learned index to find, a name beyond environment 0.
    #[inline(never)]
    fn find_name_slow(&mut self, n: u32, site: u32, mut s: u32) -> Option<(u32, usize)> {
        if s & UNRES != 0 {
            let scope = (s & !(UNRES | SEEN)) - 1;
            // A site in a function with frame slots has no environment to search, so it resolves at its first run. While no
            // function has a frame (`fr_any`), no scope is a frame scope, and the first run of a site is a search, as before.
            if s & SEEN == 0 && !(self.fr_any && self.sc_all[scope as usize].base != NONE) {
                self.cc.slots.sites[site as usize] = s | SEEN;
                return self.lookup_dyn(n);
            }
            // The first call of a function that may get its frame (`FR_WAIT`) resolves nothing: a site can run twice in one call
            // (`typeof x` reads its site twice), and a resolution for an environment would not suit the frame.
            let sc = &self.sc_all[scope as usize];
            if sc.base == NONE && self.cc.scopes[sc.func as usize].fr == FR_WAIT {
                return self.lookup_dyn(n);
            }
            s = self.sc_resolve(n, scope);
        }
        if s & (LOCAL | FRAME) == FRAME {
            self.cc.slots.sites[site as usize] = s;
            return Some((FRAME_ENV, self.fp + (s & 0xFFFF) as usize));
        }
        let mut found = None;
        if s == GLOBAL {
            found = self.find_in(0, n).map(|i| (0, i));
            if found.is_none() {
                self.cc.slots.sites[site as usize] = s;
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
        self.cc.slots.sites[site as usize] = s;
        found.or_else(|| self.lookup_dyn(n))
    }
}

/// The state of a function for frame slots (`FScope::fr`). `FR_ENV`: always an environment (a class member, an async function or
/// a generator, a function in a class body, or a function that the decision below refused). The first walk gives any other
/// function `FR_EAGER` if it has a loop or a call (`FScope::eager`: one call can be long or recursive) or `FR_NEW`. The first
/// walk decides nothing else. The first call of an `FR_NEW` function sets `FR_WAIT` and runs in an environment, with its sites
/// not resolved (they run once). The first call of an `FR_EAGER` function and the next call of an `FR_WAIT` function decide
/// (`fr_gate`). So a function that is never called pays nothing, and one that runs once pays one store. `FR_ON`: frame slots.
const FR_ENV: u8 = 0;
const FR_NEW: u8 = 1;
const FR_EAGER: u8 = 2;
const FR_WAIT: u8 = 3;
const FR_ON: u8 = 4;

// ---- Frame slots
//
// A function that no closure can reach into keeps its variables in a frame: a run of `Binding`s at the end of
// `envs[FRAME_ENV].bindings`, from `Interp::fp`. It makes no environment for the call, and none for a block, a loop head, a
// `switch` or a `catch` inside it. V8 keeps such locals in registers and SpiderMonkey in frame slots.
//
// Which functions: the first walk leaves a function a frame candidate when nothing in it can capture or name its scope: no inner function or
// class (so no closure can read a variable of it), no `arguments`, no `new.target`, no `super`, no `delete name`, no `this`
// (an arrow function may use `this`: it reads the one of its closure), and it is a plain synchronous function with simple
// parameters. Async functions and generators keep their environment because their variables must survive a suspension.
// This engine has no `eval`, and a function that holds a `with` keeps its environment (`FL_NOFR`), so no code can name a local by a string.
//
// When: a function with a loop or a call (`FScope::eager`) gets its frame at its first call: one call can be long or recursive.
// A function with neither gets it at its second call, so a function that runs once pays no layout. Its first call keeps an
// environment, and a site of it resolves nothing during that call (`find_name_slow`, `pat_frame_slow`), so no site meets the frame
// layout resolved for an environment. The one other way to run a site twice is a second call of the function while the first
// runs (a getter that calls back): `fr_gate` sees the first call on the call stack and keeps the environment for good.
//
// The layout (`fr_layout`) gives each name of each scope of the function a slot (parameters first, then the `var`s of the
// function, then its `let` and `const`, then the scopes inside in source order) and builds the template of the frame: the
// name and flags of each slot. A call copies the template (a `let` or `const` starts without `INIT`: its TDZ), stores the
// arguments in the first slots and runs. A scope inside the function has `RScope::frame`, and its sites resolve at their first
// run to `FRAME | slot`. A lookup from a frame scope to a name outside it adds no hop for the scopes of the function.
//
// A block, loop head or `switch` with lexical names puts its slots in the TDZ again each time it is entered (`fr_tdz`), as a new
// environment would. No per-iteration copy is needed: no closure can see an earlier iteration.
//
// The values of a frame are not GC roots by themselves: `collect` needs no running call, and a frame exists only during a call.
// Environment 1 is a live environment, so `roots` (for a host that asks while a call runs) includes them.

impl<'p> Interp<'p> {
    /// Append the names of pattern `p` as bindings with `flags`.
    fn fr_put(&self, p: PatId, flags: u8, out: &mut Vec<Binding>, names: &mut Vec<u32>) {
        names.clear();
        self.pat_idents(p, names);
        out.extend(names.iter().map(|&name| Binding { name, flags, value: Value::Undefined }));
    }

    /// Append the `let` and `const` names of a statement list. A `let` has no `INIT` (TDZ) and is `MUT`, a `const` has neither.
    fn fr_lex(&self, l: List<Stmt>, out: &mut Vec<Binding>, names: &mut Vec<u32>) {
        for s in self.prog.list(l) {
            if let StmtKind::Var { kind, decls } = self.prog.stmt(s).kind
                && kind != VarKind::Var
            {
                for d in self.prog.list(decls) {
                    self.fr_put(self.prog.decl(d).target, if kind == VarKind::Let { MUT } else { 0 }, out, names);
                }
            }
        }
    }

    /// The bindings that a scope of a frame function declares, in slot order. A repeated name is dropped.
    fn fr_binds(&self, kind: RKind, out: &mut Vec<Binding>, names: &mut Vec<u32>) {
        let prog = &self.prog;
        let base = out.len();
        match kind {
            RKind::Fn(fid, _) => {
                let f = *prog.func(fid);
                for p in prog.list(f.params) {
                    self.fr_put(p, INIT | MUT, out, names);
                }
                out.extend(self.cc.scopes[fid.index()].vars.iter().map(|&name| Binding { name, flags: INIT | MUT, value: Value::Undefined }));
                if let Body::Block(l) = f.body {
                    self.fr_lex(l, out, names);
                }
            }
            RKind::Block(s) => {
                if let StmtKind::Block(l) = prog.stmt(s).kind {
                    self.fr_lex(l, out, names);
                }
            }
            RKind::Switch(s) => {
                if let StmtKind::Switch { cases, .. } = prog.stmt(s).kind {
                    for c in prog.list(cases) {
                        self.fr_lex(prog.case(c).body, out, names);
                    }
                }
            }
            RKind::ForVar(s) => {
                if let StmtKind::Var { kind, decls } = prog.stmt(s).kind {
                    for d in prog.list(decls) {
                        self.fr_put(prog.decl(d).target, if kind == VarKind::Let { MUT } else { 0 }, out, names);
                    }
                }
            }
            RKind::Catch(s) => {
                if let Some(p) = self.catch_param(s) {
                    self.fr_put(p, INIT | MUT, out, names);
                }
            }
            RKind::Root | RKind::Barrier => {}
        }
        let mut w = base;
        for r in base..out.len() {
            if !out[base..w].iter().any(|b| b.name == out[r].name) {
                out[w] = out[r];
                w += 1;
            }
        }
        out.truncate(w);
    }

    /// Whether a function may have frame slots. A class member, a constructor, an async function, a generator, a function with a
    /// parameter pattern and a named function expression (it binds its own name) keep their environment. The first walk saw the
    /// rest (`FScope`). A function in a class body keeps its environment too: the end of its walk set `FR_ENV`.
    fn fr_leaf(&self, fid: FuncId) -> bool {
        let f = self.prog.func(fid);
        let fsd = &self.cc.scopes[fid.index()];
        let is_expr = matches!(self.sc_all[fsd.lo as usize].kind, RKind::Fn(_, true));
        fsd.fl & FL_NOFR == 0
            && !fsd.uses_args
            && (f.kind == FuncKind::Arrow || fsd.fl & FL_THIS == 0)
            && matches!(f.kind, FuncKind::Normal | FuncKind::Arrow | FuncKind::Method | FuncKind::Getter | FuncKind::Setter)
            && !f.is_async()
            && !f.is_generator()
            && f.simple_params
            && !(is_expr && f.name.is_some() && f.kind == FuncKind::Normal)
    }

    /// A call of a function with state `FR_EAGER` or `FR_WAIT`: `true` if it runs in a frame. Decided once. The state `FR_ENV` stays
    /// if the function is not a leaf, or its frame cannot be made.
    #[cold]
    fn fr_gate(&mut self, fid: FuncId) -> bool {
        let me = fid.index();
        // The first call has not returned (a recursive call, or a getter that calls back): its sites resolve for an environment,
        // so this function keeps its environment.
        let cur = self.cur as u32;
        let running = self.cc.scopes[me].fr == FR_WAIT && self.stk.frames.iter().any(|f| f.script == cur && matches!(f.at, Where::Func(x) if x == fid));
        if running || !self.fr_leaf(fid) {
            self.cc.scopes[me].fr = FR_ENV;
            return false;
        }
        self.fr_layout(fid)
    }

    /// Give the scopes of a function that may have frame slots their slots, and build the template of its frame in `fr_pool`.
    /// `false`: the function keeps its environment (too many slots, or a repeated parameter name). Decided once. It makes no allocation
    /// once the pools are big enough: this runs once for each function, and many functions run once.
    #[cold]
    fn fr_layout(&mut self, fid: FuncId) -> bool {
        let me = fid.index();
        self.cc.scopes[me].fr = FR_ENV;
        let lo = self.cc.scopes[me].lo as usize;
        // `func` is an index in the function table of one script. The scopes of the next script follow the last function, and its root can have the
        // same index. So can the function scope of a lazy function (`lazy_load`) that is walked next: a function has one `Fn` scope, the first.
        let hi = lo + 1 + self.sc_all[lo + 1..].iter().take_while(|r| r.func as usize == me && !matches!(r.kind, RKind::Root | RKind::Fn(..))).count();
        let prog = self.prog.clone();
        let f = *prog.func(fid);
        let mut pool = std::mem::take(&mut self.fr_pool);
        let mut names = std::mem::take(&mut self.fr_names);
        let first = pool.len();
        let mut ok = true;
        for s in lo..hi {
            self.fr_binds(self.sc_all[s].kind, &mut pool, &mut names);
            if pool.len() - first > FR_MAX {
                ok = false;
                break;
            }
            self.fr_ends.push(pool.len() as u32);
        }
        // The arguments go to the first slots, so the parameter names must be distinct.
        ok = ok && prog.list(f.params).enumerate().all(|(i, p)| pool.get(first + i).is_some_and(|b| b.name == self.cc.cold.pat_name[p.index()]));
        if ok {
            let mut base = first as u32;
            for (s, &end) in (lo..hi).zip(&self.fr_ends) {
                let start = self.sc_pool.len() as u32;
                self.sc_pool.extend(pool[base as usize..end as usize].iter().map(|b| b.name));
                let (rel, len) = (base - first as u32, end - base);
                let sc = &mut self.sc_all[s];
                (sc.start, sc.len, sc.base) = (start, len, rel);
                match sc.kind {
                    RKind::Block(st) | RKind::Switch(st) | RKind::ForVar(st) => self.cc.cold.stmt_label[st.index()] = FR_MARK | rel << 12 | len,
                    RKind::Catch(st) => self.cc.cold.stmt_label[st.index()] = FR_MARK,
                    RKind::Fn(..) | RKind::Root | RKind::Barrier => {}
                }
                base = end;
            }
            let fsd = &mut self.cc.scopes[me];
            (fsd.fr, fsd.lo, fsd.fr_len) = (FR_ON, first as u32, (pool.len() - first) as u8);
            self.fr_any = true;
        } else {
            pool.truncate(first);
        }
        self.fr_ends.clear();
        self.fr_pool = pool;
        self.fr_names = names;
        ok
    }

    /// Call a function with frame slots: no environment, the variables in a new frame. The caller selected the script of `fid`.
    #[inline(never)]
    fn call_frame(&mut self, fid: FuncId, cenv: u32, new: bool, args: &[Value], saved: (u32, bool, usize)) -> Res<Value> {
        let (saved_env, saved_strict, saved_script) = saved;
        let f = *self.prog.func(fid);
        let base = self.envs[FRAME_ENV as usize].bindings.len();
        let bs = &mut self.envs[FRAME_ENV as usize].bindings;
        let fsd = &self.cc.scopes[fid.index()];
        bs.extend_from_slice(&self.fr_pool[fsd.lo as usize..fsd.lo as usize + fsd.fr_len as usize]);
        for (i, b) in bs[base..base + f.params.len()].iter_mut().enumerate() {
            b.value = args.get(i).copied().unwrap_or(Value::Undefined);
        }
        let saved_fp = std::mem::replace(&mut self.fp, base);
        self.env = cenv;
        self.strict = f.strict;
        let script = self.cur as u32;
        let r = self.in_frame(script, Where::Func(fid), new, |it| it.run_body(fid, true));
        self.envs[FRAME_ENV as usize].bindings.truncate(base);
        self.fp = saved_fp;
        self.select(saved_script);
        self.env = saved_env;
        self.strict = saved_strict;
        r
    }

    /// A block, loop head or `switch` with `let` or `const` in frame slots starts: the slots are in their TDZ.
    #[inline(never)]
    fn fr_tdz(&mut self, mark: u32) {
        let b = self.fp + (mark >> 12 & 0xFFF) as usize;
        for x in &mut self.envs[FRAME_ENV as usize].bindings[b..b + (mark & 0xFFF) as usize] {
            x.flags &= !INIT;
        }
    }

    /// The frame slot (an index in `envs[FRAME_ENV].bindings`) of the name of the pattern site `site`, if it is in a function with
    /// frame slots. For `let`, `const` and `catch` names, which are declared and not searched.
    #[inline(never)]
    fn pat_frame(&mut self, n: u32, site: u32) -> Option<usize> {
        let s = self.cc.slots.sites[site as usize];
        if s & (UNRES | LOCAL | FRAME) == FRAME {
            // The resolved slot: the end of `pat_frame_slow`, without its call.
            debug_assert_eq!(self.envs[FRAME_ENV as usize].bindings[self.fp + (s & 0xFFFF) as usize].name, n);
            return Some(self.fp + (s & 0xFFFF) as usize);
        }
        if s & (UNRES | FRAME) == 0 { None } else { self.pat_frame_slow(n, site, s) }
    }

    #[inline(never)]
    fn pat_frame_slow(&mut self, n: u32, site: u32, mut s: u32) -> Option<usize> {
        if s & UNRES != 0 {
            let scope = (s & !(UNRES | SEEN)) - 1;
            let sc = &self.sc_all[scope as usize];
            if sc.base == NONE {
                // A function that may get its frame (before its decision) must keep its sites unmarked.
                if self.cc.scopes[sc.func as usize].fr == FR_ENV {
                    self.cc.slots.sites[site as usize] = NOFRAME;
                }
                return None;
            }
            s = self.sc_resolve(n, scope);
            self.cc.slots.sites[site as usize] = s;
        }
        debug_assert!(s & (LOCAL | FRAME) == FRAME, "a declared name of a frame scope has a slot");
        let i = self.fp + (s & 0xFFFF) as usize;
        debug_assert_eq!(self.envs[FRAME_ENV as usize].bindings[i].name, n);
        Some(i)
    }
}
