//! Scope data for the code generator, stage 1 (step B1 of `docs/engine-plan.md`): the exact environment layout of the tree
//! walker. A child module of `eval`, so it reads the walker's tables (`sc_all`, `scopes`, the names) directly. It changes none of
//! them, and the walker never calls it.
//!
//! Rule of stage 1: a bytecode function makes the same environments as the tree walker, with the same bindings in the same order.
//! So a bytecode function and a tree function can sit in one scope chain. For each scope this file gives the bindings in the order
//! of the walker (`bc_binds`), and for each identifier site the environment and the binding that it means (`bc_resolve`).
//!
//! The order of the bindings comes from the code that makes them at run time, not from `sc_names` (a set):
//! - A function (`bind_call`, then `declare_scope`): the hidden class bindings of `cls_frame` (`%this` and `%fn` of a derived
//!   constructor, then `%nt`), the own name of a named function expression, the parameters, `arguments`, the `var` names, then the
//!   `let`, `const`, function and class declarations of the body in source order.
//! - A block and a `switch`: its `let`, `const`, function and class declarations in source order.
//! - A `for`, `for-in` or `for-of` with `let` or `const`: the declared names. A `catch` clause: the names of its parameter.
//! - A name that is declared again keeps its first place. A declaration (`env_declare`) takes the new flags. A `var` (`declare_var`)
//!   keeps the old flags, except over the own name of a function expression.
//! - The script (environment 0) holds the `let`, `const` and `class` names of every script, so the place of a name there is known
//!   only after its script declared it. `bc_resolve` reads it from environment 0 (`find_in`).
//!
//! The hop count follows `sc_resolve` (`eval_scope.rs`), with one difference: a scope of a function with frame slots counts as an
//! environment, because stage 1 gives a bytecode function its environments. Stage 2 (step B12) changes the rule for both engines.
//!
//! The walk (`bc_walk`) visits the code of one function in the order of the first walk (`walk_program`), so the scopes that it
//! opens have the same numbers as in `sc_all`. It checks the kind and the parent of each scope that it meets.
//!
//! The check (`bc_scope_check`): every identifier site that the tree walker resolved must have the same hop count and the same
//! binding index here, and every site that the walker did not resolve yet must be in the same scope. A debug build of the walker
//! also checks each resolved site against the full search, so both ends are checked.
// Until the code generator (step B2) and the loop (step B3) use them, only the check and the tests call most of these functions.
#![allow(dead_code)]
use super::*;

/// What the name at an identifier site means.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Site {
    /// The binding `index` of the environment `hops` levels up.
    Env { hops: u32, index: u32 },
    /// No scope of the script declares the name: environment 0 (a lexical name of another script), then the global object.
    Global,
    /// The full search: a site behind a class, a site named `arguments`, or a binding that does not exist yet (a parameter default
    /// that reads a `var` of the same function, or a script name before its script runs).
    Dynamic,
}

/// What made a binding.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum BindKind {
    /// `%this`, `%fn` or `%nt` of a class member (`cls_frame`).
    Hidden,
    /// The own name of a named function expression (flag `SELF`).
    SelfName,
    Param,
    Arguments,
    Var,
    Let,
    Const,
    /// A function declaration: its closure is made when the scope starts.
    Func(FuncId),
    Class,
    CatchParam,
}

/// One binding of an environment. `flags` are the binding flags (`INIT`, `MUT`, `SELF`) when the scope starts: a `let`, a `const`
/// and a class have no `INIT` (their TDZ).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Bind {
    pub name: u32,
    pub flags: u8,
    pub kind: BindKind,
}

/// Flags of a function for the code generator (`FnScope::flags`).
/// It names `this` (an arrow function reads the `this` of its closure).
pub(super) const BF_THIS: u16 = 1;
/// It reads `arguments` (also from an arrow function inside it), so its environment has the `arguments` binding.
pub(super) const BF_ARGS: u16 = 2;
/// It reads `new.target`, so its environment has `%nt`.
pub(super) const BF_NEW_TARGET: u16 = 4;
/// A derived class constructor: its environment has `%this` and `%fn`.
pub(super) const BF_DERIVED: u16 = 8;
/// A class is around it: a site that leaves the class is dynamic.
pub(super) const BF_IN_CLASS: u16 = 16;
/// It has a loop or a call (`FL_EAGER`): one call can be long, so `Auto` compiles it at its first call.
pub(super) const BF_LOOP_OR_CALL: u16 = 32;
/// It has an inner function or class, `new.target`, `super` or `delete name` (`FL_NOFR`).
pub(super) const BF_INNER: u16 = 64;
/// Its environment has the own name of the function (a named function expression).
pub(super) const BF_SELF_NAME: u16 = 128;

/// The scope data of one function.
#[derive(Clone, Copy, Debug)]
pub(super) struct FnScope {
    /// The function scope, an index of `sc_all`.
    pub scope: u32,
    pub flags: u16,
}

/// The result of `bc_walk`: what the code of one function (or of the script body) holds, in the order of the first walk.
#[derive(Default)]
pub(super) struct ScopeWalk {
    cur: u32,
    next: u32,
    /// Each identifier site and the scope it is in. A site is an expression index, or the expression count plus a pattern index.
    pub sites: Vec<(u32, u32)>,
    /// Each statement that opens a scope (block, `switch`, the `let` or `const` of a loop head, the `try` of a `catch`) and the scope.
    pub stmts: Vec<(StmtId, u32)>,
    /// Each inner function and its scope. The walk does not go into it.
    pub funcs: Vec<(FuncId, u32)>,
    /// The first place where a scope of `sc_all` was not the one the walk expected (a bug of the walk).
    pub bad: Option<u32>,
}

/// The result of `bc_scope_check`. `mismatches` must be empty.
#[derive(Clone, Debug, Default)]
pub struct ScopeCheck {
    /// All identifier sites that were visited.
    pub sites: usize,
    /// Sites that the walker resolved with a known binding index: hop count and index compared.
    pub indexed: usize,
    /// Sites that the walker resolved to `GLOBAL` or to dynamic: compared.
    pub global_or_dynamic: usize,
    /// Sites that the walker resolved with no index yet (its binding was missing at the time): hop count compared.
    pub hops_only: usize,
    /// Sites that the walker did not resolve (they ran less than twice): the scope compared.
    pub unresolved: usize,
    /// Sites in a function with frame slots: the walker's slot is not an environment, so only the static hop count of a name outside
    /// the function is compared.
    pub frame: usize,
    /// Sites whose static resolution (`sc_resolve`) was compared, of all the above.
    pub static_checked: usize,
    /// One line for each difference (at most 50).
    pub mismatches: Vec<String>,
}

/// Whether two scope kinds are the same scope.
fn same_kind(a: RKind, b: RKind) -> bool {
    match (a, b) {
        (RKind::Root, RKind::Root) | (RKind::Barrier, RKind::Barrier) => true,
        (RKind::Fn(f, _), RKind::Fn(g, _)) => f == g,
        (RKind::Block(s), RKind::Block(t)) | (RKind::Switch(s), RKind::Switch(t)) | (RKind::ForVar(s), RKind::ForVar(t)) | (RKind::Catch(s), RKind::Catch(t)) => s == t,
        _ => false,
    }
}

impl<'p> Interp<'p> {
    /// The index in `sc_all` of the root scope of the current script, or `None` for a script that was never walked (the empty script
    /// of [`Interp::realm`]). Every walked script opens one root scope, in the order of the scripts.
    fn bc_root(&self) -> Option<u32> {
        let roots: Vec<u32> = (0..self.sc_all.len() as u32).filter(|&i| matches!(self.sc_all[i as usize].kind, RKind::Root)).collect();
        let unwalked = self.scripts.len() - roots.len();
        self.cur.checked_sub(unwalked).map(|k| roots[k])
    }

    /// One after the last scope inside scope `i` (its inner scopes follow it in `sc_all`).
    pub(super) fn bc_scope_end(&self, i: u32) -> u32 {
        let mut j = i + 1;
        while (j as usize) < self.sc_all.len() {
            let mut p = self.sc_all[j as usize].parent;
            while p != NONE && p > i {
                p = self.sc_all[p as usize].parent;
            }
            if p != i {
                break;
            }
            j += 1;
        }
        j
    }

    /// The scope of function `fid` of the current script. The table of the script is made at the first call.
    pub(super) fn bc_fn_scope(&mut self, fid: FuncId) -> Option<FnScope> {
        let cur = self.cur;
        if self.bc.fn_scopes.len() <= cur {
            self.bc.fn_scopes.resize_with(cur + 1, Default::default);
        }
        if self.bc.fn_scopes[cur].is_empty() {
            let mut t = vec![NONE; self.prog.stats().funcs + 1].into_boxed_slice();
            if let Some(r) = self.bc_root() {
                for i in r + 1..self.bc_scope_end(r) {
                    if let RKind::Fn(f, _) = self.sc_all[i as usize].kind {
                        t[f.index()] = i;
                    }
                }
            }
            self.bc.fn_scopes[cur] = t;
        }
        let scope = self.bc.fn_scopes[cur][fid.index()];
        if scope == NONE {
            return None;
        }
        let f = *self.prog.func(fid);
        let fsd = &self.scopes[fid.index()];
        let ff = self.cls.fflags[fid.index()];
        let mut flags = 0;
        for (on, b) in [
            (fsd.fl & FL_THIS != 0, BF_THIS),
            (fsd.uses_args && f.kind != FuncKind::Arrow, BF_ARGS),
            (ff & F_NT != 0, BF_NEW_TARGET),
            (ff & F_DERIVED != 0, BF_DERIVED),
            (fsd.fl & FL_EAGER != 0, BF_LOOP_OR_CALL),
            (fsd.fl & FL_NOFR != 0, BF_INNER),
            (matches!(self.sc_all[scope as usize].kind, RKind::Fn(_, true)) && f.name.is_some() && f.kind == FuncKind::Normal, BF_SELF_NAME),
        ] {
            if on {
                flags |= b;
            }
        }
        let mut p = self.sc_all[scope as usize].parent;
        while p != NONE {
            if matches!(self.sc_all[p as usize].kind, RKind::Barrier) {
                flags |= BF_IN_CLASS;
            }
            p = self.sc_all[p as usize].parent;
        }
        Some(FnScope { scope, flags })
    }

    /// Append the bindings of scope `s` of the current script to `out`, in the order of the tree walker.
    pub(super) fn bc_binds(&mut self, s: u32, out: &mut Vec<Bind>) {
        let start = out.len();
        // `env_declare`: a known name keeps its place and takes the new flags. `declare_var`: a known name keeps its flags too,
        // unless it is the own name of a function expression.
        let decl = |out: &mut Vec<Bind>, b: Bind, var: bool| match out[start..].iter_mut().find(|x| x.name == b.name) {
            Some(x) if !var || x.flags & SELF != 0 => (x.flags, x.kind) = (b.flags, b.kind),
            Some(_) => {}
            None => out.push(b),
        };
        let prog = self.prog.clone();
        let mut names = Vec::new();
        let mut pat = |it: &Self, out: &mut Vec<Bind>, p: PatId, flags: u8, kind: BindKind| {
            names.clear();
            it.pat_idents(p, &mut names);
            for &name in &names {
                decl(out, Bind { name, flags, kind }, false);
            }
        };
        match self.sc_all[s as usize].kind {
            RKind::Root => {
                // `declare_scope` at environment 0: a function goes to the global object.
                self.bc_lex(prog.body, false, out, start);
            }
            RKind::Barrier => {}
            RKind::Fn(fid, is_expr) => {
                let f = *prog.func(fid);
                let ff = self.cls.fflags[fid.index()];
                if ff != 0 {
                    if ff & F_DERIVED != 0 {
                        decl(out, Bind { name: self.cls.n_this, flags: MUT, kind: BindKind::Hidden }, false);
                        decl(out, Bind { name: self.cls.n_fn, flags: INIT, kind: BindKind::Hidden }, false);
                    }
                    decl(out, Bind { name: self.cls.n_nt, flags: INIT, kind: BindKind::Hidden }, false);
                }
                if is_expr && f.name.is_some() && f.kind == FuncKind::Normal {
                    decl(out, Bind { name: self.func_name[fid.index()], flags: INIT | SELF, kind: BindKind::SelfName }, false);
                }
                for p in prog.list(f.params) {
                    pat(self, out, p, INIT | MUT, BindKind::Param);
                }
                if self.scopes[fid.index()].uses_args && f.kind != FuncKind::Arrow {
                    decl(out, Bind { name: self.n_arguments, flags: INIT | MUT, kind: BindKind::Arguments }, false);
                }
                for &name in &self.scopes[fid.index()].vars {
                    decl(out, Bind { name, flags: INIT | MUT, kind: BindKind::Var }, true);
                }
                if let Body::Block(l) = f.body {
                    self.bc_lex(l, true, out, start);
                }
            }
            RKind::Block(st) => {
                if let StmtKind::Block(l) = prog.stmt(st).kind {
                    self.bc_lex(l, true, out, start);
                }
            }
            RKind::Switch(st) => {
                if let StmtKind::Switch { cases, .. } = prog.stmt(st).kind {
                    for c in prog.list(cases) {
                        self.bc_lex(prog.case(c).body, true, out, start);
                    }
                }
            }
            RKind::ForVar(st) => {
                if let StmtKind::Var { kind, decls } = prog.stmt(st).kind {
                    let (flags, k) = if kind == VarKind::Let { (MUT, BindKind::Let) } else { (0, BindKind::Const) };
                    for d in prog.list(decls) {
                        pat(self, out, prog.decl(d).target, flags, k);
                    }
                }
            }
            RKind::Catch(st) => {
                if let Some(p) = self.catch_param(st) {
                    pat(self, out, p, INIT | MUT, BindKind::CatchParam);
                }
            }
        }
    }

    /// The declarations of a statement list, as `declare_scope` makes them. `funcs`: function declarations make a binding.
    fn bc_lex(&mut self, l: List<Stmt>, funcs: bool, out: &mut Vec<Bind>, start: usize) {
        let prog = self.prog.clone();
        let add = |out: &mut Vec<Bind>, b: Bind| match out[start..].iter_mut().find(|x| x.name == b.name) {
            Some(x) => (x.flags, x.kind) = (b.flags, b.kind),
            None => out.push(b),
        };
        let mut names = Vec::new();
        for s in prog.list(l) {
            match prog.stmt(s).kind {
                StmtKind::Var { kind, decls } if kind != VarKind::Var => {
                    let (flags, k) = if kind == VarKind::Let { (MUT, BindKind::Let) } else { (0, BindKind::Const) };
                    for d in prog.list(decls) {
                        names.clear();
                        self.pat_idents(prog.decl(d).target, &mut names);
                        for &name in &names {
                            add(out, Bind { name, flags, kind: k });
                        }
                    }
                }
                StmtKind::Function(fid) if funcs => add(out, Bind { name: self.func_name[fid.index()], flags: INIT | MUT, kind: BindKind::Func(fid) }),
                StmtKind::Class(c) => {
                    if let Some(sp) = prog.class(c).name {
                        let name = self.name_at(sp);
                        add(out, Bind { name, flags: MUT, kind: BindKind::Class });
                    }
                }
                _ => {}
            }
        }
    }

    /// Whether scope `s` declares the name `n` (the set of `sc_declares`). A frame scope keeps its names unsorted.
    fn bc_declares(&mut self, s: u32, n: u32) -> bool {
        let sc = &self.sc_all[s as usize];
        if sc.base != NONE {
            return self.sc_pool[sc.start as usize..(sc.start + sc.len) as usize].contains(&n);
        }
        self.sc_declares(s, n)
    }

    /// What the name `n` means at a site in scope `scope` of the current script.
    pub(super) fn bc_resolve(&mut self, n: u32, mut scope: u32) -> Site {
        if n == self.n_arguments {
            return Site::Dynamic;
        }
        let mut hops = 0;
        loop {
            if matches!(self.sc_all[scope as usize].kind, RKind::Barrier) {
                return Site::Dynamic;
            }
            if self.bc_declares(scope, n) {
                if hops >= MAX_HOPS {
                    return Site::Dynamic;
                }
                let index = if matches!(self.sc_all[scope as usize].kind, RKind::Root) {
                    self.find_in(0, n)
                } else {
                    let mut b = Vec::new();
                    self.bc_binds(scope, &mut b);
                    b.iter().position(|b| b.name == n)
                };
                return match index {
                    Some(i) if i < 0xFFFF => Site::Env { hops, index: i as u32 },
                    _ => Site::Dynamic,
                };
            }
            let p = self.sc_all[scope as usize].parent;
            if p == NONE {
                return Site::Global;
            }
            hops += 1;
            scope = p;
        }
    }

    // ---- The walk, in the order of `w_stmt_inner`, `w_expr_inner`, `w_pat_inner`, `w_func_inner` and `w_class_inner`.

    /// Walk the code of function `fid` (its scope is `scope`), or of the script body (`fid` is `None`, `scope` is the root).
    pub(super) fn bc_walk(&self, fid: Option<FuncId>, scope: u32) -> ScopeWalk {
        let mut w = ScopeWalk { cur: scope, next: scope + 1, ..Default::default() };
        let prog = &self.prog;
        match fid {
            None => prog.list(prog.body).for_each(|s| self.sw_stmt(&mut w, s)),
            Some(fid) => {
                let f = *prog.func(fid);
                prog.list(f.params).for_each(|p| self.sw_pat(&mut w, p));
                match f.body {
                    Body::Block(l) => prog.list(l).for_each(|s| self.sw_stmt(&mut w, s)),
                    Body::Expr(e) => self.sw_expr(&mut w, e),
                }
            }
        }
        w
    }

    /// Open the next scope, which must be `kind`. Gives the scope to go back to.
    fn sw_open(&self, w: &mut ScopeWalk, kind: RKind) -> u32 {
        let i = w.next;
        match self.sc_all.get(i as usize) {
            Some(sc) if same_kind(sc.kind, kind) && sc.parent == w.cur => {}
            _ => {
                w.bad.get_or_insert(i);
            }
        }
        w.next += 1;
        std::mem::replace(&mut w.cur, i)
    }

    /// An inner function: record it and skip its scopes.
    fn sw_func(&self, w: &mut ScopeWalk, fid: FuncId) {
        let i = w.next;
        match self.sc_all.get(i as usize) {
            Some(sc) if same_kind(sc.kind, RKind::Fn(fid, false)) && sc.parent == w.cur => {}
            _ => {
                w.bad.get_or_insert(i);
            }
        }
        w.funcs.push((fid, i));
        w.next = self.bc_scope_end(i);
    }

    fn sw_pat(&self, w: &mut ScopeWalk, p: PatId) {
        let prog = &self.prog;
        match prog.pat(p).kind {
            PatKind::Ident => w.sites.push((self.pat_site(p.index()), w.cur)),
            PatKind::Member(e) => self.sw_expr(w, e),
            PatKind::Array(l) => prog.list(l).for_each(|q| self.sw_pat(w, q)),
            PatKind::Object(l) => {
                for q in prog.list(l) {
                    let pp = *prog.pat_prop(q);
                    if let Key::Computed(e) = pp.key {
                        self.sw_expr(w, e);
                    }
                    self.sw_pat(w, pp.value);
                }
            }
            PatKind::Assign { target, default } => {
                self.sw_pat(w, target);
                self.sw_expr(w, default);
            }
            PatKind::Rest(t) => self.sw_pat(w, t),
            PatKind::Hole => {}
        }
    }

    fn sw_stmt(&self, w: &mut ScopeWalk, s: StmtId) {
        let prog = &self.prog;
        match prog.stmt(s).kind {
            StmtKind::Expr(e) | StmtKind::Directive(e) | StmtKind::Throw(e) => self.sw_expr(w, e),
            StmtKind::Var { decls, .. } => {
                for d in prog.list(decls) {
                    let d = *prog.decl(d);
                    self.sw_pat(w, d.target);
                    if let Some(i) = d.init {
                        self.sw_expr(w, i);
                    }
                }
            }
            StmtKind::Function(fid) => self.sw_func(w, fid),
            StmtKind::Class(c) => self.sw_class(w, c),
            StmtKind::Return(e) => e.into_iter().for_each(|e| self.sw_expr(w, e)),
            StmtKind::If { test, cons, alt } => {
                self.sw_expr(w, test);
                self.sw_stmt(w, cons);
                alt.into_iter().for_each(|a| self.sw_stmt(w, a));
            }
            StmtKind::For { init, test, update, body } => {
                let saved = self.sw_loop_var(w, if let ForInit::Var(v) = init { Some(v) } else { None });
                match init {
                    ForInit::None => {}
                    ForInit::Var(v) => self.sw_stmt(w, v),
                    ForInit::Expr(e) => self.sw_expr(w, e),
                }
                [test, update].into_iter().flatten().for_each(|e| self.sw_expr(w, e));
                self.sw_stmt(w, body);
                w.cur = saved;
            }
            StmtKind::ForIn { left, right, body } | StmtKind::ForOf { left, right, body, .. } => {
                self.sw_expr(w, right);
                let saved = self.sw_loop_var(w, if let ForLeft::Var(v) = left { Some(v) } else { None });
                match left {
                    ForLeft::Var(v) => self.sw_stmt(w, v),
                    ForLeft::Pat(p) => self.sw_pat(w, p),
                }
                self.sw_stmt(w, body);
                w.cur = saved;
            }
            StmtKind::While { test, body } | StmtKind::DoWhile { body, test } => {
                self.sw_expr(w, test);
                self.sw_stmt(w, body);
            }
            StmtKind::Break(_) | StmtKind::Continue(_) | StmtKind::Empty | StmtKind::Debugger | StmtKind::With { .. } => {}
            StmtKind::Try { block, handler, finalizer } => {
                self.sw_stmt(w, block);
                if let Some(c) = handler {
                    let c = *prog.catch(c);
                    let saved = self.sw_open(w, RKind::Catch(s));
                    w.stmts.push((s, w.cur));
                    c.param.into_iter().for_each(|p| self.sw_pat(w, p));
                    self.sw_stmt(w, c.body);
                    w.cur = saved;
                }
                finalizer.into_iter().for_each(|f| self.sw_stmt(w, f));
            }
            StmtKind::Switch { disc, cases } => {
                self.sw_expr(w, disc);
                let lex = prog.list(cases).any(|c| self.has_lexical(prog.case(c).body));
                let saved = if lex {
                    let saved = self.sw_open(w, RKind::Switch(s));
                    w.stmts.push((s, w.cur));
                    saved
                } else {
                    w.cur
                };
                for c in prog.list(cases) {
                    let c = *prog.case(c);
                    c.test.into_iter().for_each(|t| self.sw_expr(w, t));
                    prog.list(c.body).for_each(|s| self.sw_stmt(w, s));
                }
                w.cur = saved;
            }
            StmtKind::Labeled { body, .. } => self.sw_stmt(w, body),
            StmtKind::Block(l) => {
                let saved = if self.has_lexical(l) {
                    let saved = self.sw_open(w, RKind::Block(s));
                    w.stmts.push((s, w.cur));
                    saved
                } else {
                    w.cur
                };
                prog.list(l).for_each(|s| self.sw_stmt(w, s));
                w.cur = saved;
            }
        }
    }

    /// The scope of the `let` or `const` of a loop head (`sc_lexical_var`). Gives the scope to go back to.
    fn sw_loop_var(&self, w: &mut ScopeWalk, v: Option<StmtId>) -> u32 {
        match v {
            Some(v) if matches!(self.prog.stmt(v).kind, StmtKind::Var { kind, .. } if kind != VarKind::Var) => {
                let saved = self.sw_open(w, RKind::ForVar(v));
                w.stmts.push((v, w.cur));
                saved
            }
            _ => w.cur,
        }
    }

    fn sw_class(&self, w: &mut ScopeWalk, c: ClassId) {
        let prog = &self.prog;
        let class = *prog.class(c);
        let saved = self.sw_open(w, RKind::Barrier);
        class.superclass.into_iter().for_each(|s| self.sw_expr(w, s));
        for el in prog.list(class.elems) {
            let e = *prog.elem(el);
            if let Key::Computed(k) = e.key {
                self.sw_expr(w, k);
            }
            match e.kind {
                ElemKind::Method(f) | ElemKind::Getter(f) | ElemKind::Setter(f) => self.sw_func(w, f),
                ElemKind::Field(Some(init)) => self.sw_expr(w, init),
                ElemKind::Field(None) => {}
            }
        }
        w.cur = saved;
    }

    fn sw_expr(&self, w: &mut ScopeWalk, e: ExprId) {
        let prog = &self.prog;
        match prog.expr(e).kind {
            ExprKind::Ident => w.sites.push((e.index() as u32, w.cur)),
            ExprKind::This | ExprKind::Null | ExprKind::Bool(_) | ExprKind::Number(_) | ExprKind::String | ExprKind::Hole | ExprKind::BigInt => {}
            ExprKind::RegExp | ExprKind::NewTarget | ExprKind::Super => {}
            ExprKind::Template(t) => prog.template_exprs(prog.template(t)).for_each(|x| self.sw_expr(w, x)),
            ExprKind::Tagged { tag, quasi } => {
                self.sw_expr(w, tag);
                prog.template_exprs(prog.template(quasi)).for_each(|x| self.sw_expr(w, x));
            }
            ExprKind::Array(l) | ExprKind::Seq(l) => prog.list(l).for_each(|x| self.sw_expr(w, x)),
            ExprKind::Object(l) => {
                for p in prog.list(l) {
                    let pr = *prog.prop(p);
                    if let Key::Computed(k) = pr.key {
                        self.sw_expr(w, k);
                    }
                    self.sw_expr(w, pr.value);
                }
            }
            ExprKind::Function(f) => self.sw_func(w, f),
            ExprKind::Class(c) => self.sw_class(w, c),
            ExprKind::Unary { arg, .. } | ExprKind::Update { arg, .. } | ExprKind::Spread(arg) | ExprKind::Paren(arg) | ExprKind::Chain(arg) | ExprKind::Await(arg) => self.sw_expr(w, arg),
            ExprKind::Binary { left, right, .. } | ExprKind::Logical { left, right, .. } => {
                self.sw_expr(w, left);
                self.sw_expr(w, right);
            }
            ExprKind::Assign { target, value, .. } => {
                self.sw_pat(w, target);
                self.sw_expr(w, value);
            }
            ExprKind::Cond { test, cons, alt } => [test, cons, alt].into_iter().for_each(|x| self.sw_expr(w, x)),
            ExprKind::Call { callee, args, .. } | ExprKind::New { callee, args } => {
                self.sw_expr(w, callee);
                prog.list(args).for_each(|x| self.sw_expr(w, x));
            }
            ExprKind::Member { obj, .. } | ExprKind::PrivateMember { obj, .. } => self.sw_expr(w, obj),
            ExprKind::Index { obj, index, .. } => {
                self.sw_expr(w, obj);
                self.sw_expr(w, index);
            }
            ExprKind::PrivateIn { right, .. } => self.sw_expr(w, right),
            ExprKind::Yield { arg, .. } => arg.into_iter().for_each(|a| self.sw_expr(w, a)),
        }
    }

    // ---- The check against the tree walker (risk R7 of the plan).

    /// Compare every identifier site of every script with what the tree walker resolved (see the module comment). Call it after the
    /// code ran. Each script is selected in turn, and the current one at the end.
    #[doc(hidden)]
    pub fn bc_scope_check(&mut self) -> ScopeCheck {
        let mut r = ScopeCheck::default();
        let old = self.cur;
        for id in 0..self.scripts.len() {
            self.select(id);
            self.bc_check_script(&mut r);
        }
        self.select(old);
        r
    }

    fn bc_check_script(&mut self, r: &mut ScopeCheck) {
        let Some(root) = self.bc_root() else { return };
        let mut todo = vec![(None, root)];
        while let Some((fid, scope)) = todo.pop() {
            let w = self.bc_walk(fid, scope);
            if let Some(b) = w.bad {
                r.mismatches.push(format!("script {}: the walk did not find the scope {b} of sc_all where it expected it", self.cur));
            }
            todo.extend(w.funcs.iter().map(|&(f, s)| (Some(f), s)));
            for &(site, sc) in &w.sites {
                self.bc_check_site(r, site, sc);
            }
        }
    }

    fn bc_check_site(&mut self, r: &mut ScopeCheck, site: u32, scope: u32) {
        r.sites += 1;
        let nexprs = self.prog.exprs.len() as u32;
        let n = if site < nexprs { self.expr_name[site as usize] } else { self.pat_name[(site - nexprs) as usize] };
        let s = self.slots.sites[site as usize];
        let b1 = self.bc_resolve(n, scope);
        let frame = self.sc_all[scope as usize].base != NONE;
        let mut bad = None;
        // The static resolution of the walker. A frame scope adds no hop there, so only a site outside frames is compared.
        if !frame && n != self.n_arguments {
            r.static_checked += 1;
            let st = self.sc_resolve(n, scope);
            let ok = match b1 {
                Site::Env { hops, .. } => st & LOCAL != 0 && (st >> 16) & MAX_HOPS == hops,
                Site::Global => st == GLOBAL,
                // A declared name with no binding (yet) is dynamic here, and local there.
                Site::Dynamic => st == 0 || st & LOCAL != 0,
            };
            if !ok {
                bad = Some(format!("static {st:#x}"));
            }
        }
        if s & UNRES != 0 {
            r.unresolved += 1;
            let want = (s & !(UNRES | SEEN)) - 1;
            if want != scope {
                bad = Some(format!("scope {want} in the walker, {scope} here"));
            }
        } else if s == NOFRAME {
            // A declared name of a `let`, `const` or `catch` pattern: no lookup.
        } else if frame || s & (LOCAL | FRAME) == FRAME {
            r.frame += 1;
        } else if s & LOCAL != 0 {
            let (hops, index) = ((s >> 16) & MAX_HOPS, s & 0xFFFF);
            if index == 0xFFFF {
                r.hops_only += 1;
                if !matches!(b1, Site::Env { hops: h, .. } if h == hops) && b1 != Site::Dynamic {
                    bad = Some(format!("walker hops {hops}"));
                }
            } else {
                r.indexed += 1;
                if b1 != (Site::Env { hops, index }) {
                    bad = Some(format!("walker hops {hops} index {index}"));
                }
            }
        } else {
            r.global_or_dynamic += 1;
            let ok = if s == GLOBAL { b1 == Site::Global } else { s == 0 && b1 == Site::Dynamic };
            if !ok {
                bad = Some(format!("walker {s:#x}"));
            }
        }
        if let Some(why) = bad
            && r.mismatches.len() < 50
        {
            let at = if site < nexprs { self.prog.expr(ExprId::from_index(site as usize)).span } else { self.prog.pat(PatId::from_index((site - nexprs) as usize)).span };
            r.mismatches.push(format!("script {} offset {} '{}': {why}, here {b1:?}", self.cur, at.start, self.names[n as usize]));
        }
    }
}

#[cfg(test)]
#[path = "bc_scope_tests.rs"]
mod tests;
