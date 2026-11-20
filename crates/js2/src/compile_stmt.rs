//! Statements of the compiler: the entry of a function, scopes, loops, `switch`, `try` and the jumps out of them.
use super::expr::K;
use super::pat::Bind;
use super::*;
use crate::bytecode::STR_EMPTY;
use std::collections::HashSet;

/// The fewest distinct integer cases for a jump table, and the largest `max - min + 1` for each case (V8 allows 3 for each clause and
/// needs 6 cases; JSC needs 3 cases). The numbers are measured in `docs/progress.md`.
const MIN_INT_CASES: usize = 3;
const INT_SPAN: i64 = 3;
/// The fewest distinct string cases for a hash table.
const MIN_STR_CASES: usize = 3;

/// The keys of a switch that a table dispatches: each key with the index of its case.
enum Plan {
    Int(Vec<(i32, usize)>),
    Str(Vec<(u32, usize)>),
}

/// The names that a function (or a script) declares in its own scope, found before the code is compiled.
#[derive(Default)]
struct Hoist<'a> {
    /// `var` names, also those in nested blocks, and the names of sloppy-mode functions in blocks (Annex B).
    vars: Vec<Cow<'a, str>>,
    /// The function declarations of the top level, in order.
    funcs: Vec<(FuncId, Cow<'a, str>)>,
    /// The `let`, `const` and `class` declarations of the top level, with the end of the declaration.
    lex: Vec<(Cow<'a, str>, VK, u32)>,
}

/// A function declaration of a block: its name and whether the name is a binding of the block (strict code) or a variable of the function.
type BlockFn<'a> = (FuncId, Cow<'a, str>, bool);

impl<'a> Fc<'a> {
    // ------------------------------------------------------------ hoisting

    fn hoist_list(&self, list: List<Stmt>, top: bool, h: &mut Hoist<'a>) {
        for s in self.prog.list(list) {
            self.hoist_stmt(s, top, h);
        }
    }

    fn hoist_decls(&self, kind: VarKind, decls: List<Declarator>, top: bool, h: &mut Hoist<'a>) {
        let prog = self.prog;
        for d in prog.list(decls) {
            let d = *prog.decl(d);
            let mut names = Vec::new();
            self.pat_names(d.target, &mut names);
            for (n, _) in names {
                match kind {
                    VarKind::Var => h.vars.push(n),
                    VarKind::Let | VarKind::Const if top => h.lex.push((n, if kind == VarKind::Let { VK::Let } else { VK::Const }, d.span.end)),
                    _ => {}
                }
            }
        }
    }

    fn hoist_stmt(&self, s: StmtId, top: bool, h: &mut Hoist<'a>) {
        let prog = self.prog;
        match prog.stmt(s).kind {
            StmtKind::Var { kind, decls } => self.hoist_decls(kind, decls, top, h),
            StmtKind::Function(f) => {
                if let Some(n) = prog.func(f).name {
                    if top {
                        h.funcs.push((f, prog.name(n)));
                    } else if !self.strict {
                        h.vars.push(prog.name(n));
                    }
                }
            }
            StmtKind::Class(c) if top => {
                if let Some(n) = prog.class(c).name {
                    h.lex.push((prog.name(n), VK::Class, prog.stmt(s).span.end));
                }
            }
            StmtKind::If { cons, alt, .. } => {
                self.hoist_stmt(cons, false, h);
                if let Some(a) = alt {
                    self.hoist_stmt(a, false, h);
                }
            }
            StmtKind::For { init, body, .. } => {
                if let ForInit::Var(v) = init {
                    self.hoist_stmt(v, false, h);
                }
                self.hoist_stmt(body, false, h);
            }
            StmtKind::ForIn { left, body, .. } | StmtKind::ForOf { left, body, .. } => {
                if let ForLeft::Var(v) = left {
                    self.hoist_stmt(v, false, h);
                }
                self.hoist_stmt(body, false, h);
            }
            StmtKind::While { body, .. } | StmtKind::DoWhile { body, .. } | StmtKind::Labeled { body, .. } | StmtKind::With { body, .. } => {
                self.hoist_stmt(body, false, h)
            }
            StmtKind::Try { block, handler, finalizer } => {
                self.hoist_stmt(block, false, h);
                if let Some(c) = handler {
                    self.hoist_stmt(prog.catch(c).body, false, h);
                }
                if let Some(f) = finalizer {
                    self.hoist_stmt(f, false, h);
                }
            }
            StmtKind::Switch { cases, .. } => {
                for c in prog.list(cases) {
                    self.hoist_list(prog.case(c).body, false, h);
                }
            }
            StmtKind::Block(l) => self.hoist_list(l, false, h),
            _ => {}
        }
    }

    // ------------------------------------------------------------ entry of a function

    /// Allocate the parameter registers, declare the scope of the function and compile its body.
    pub(crate) fn function_body(&mut self, fid: FuncId) -> CR {
        let prog = self.prog;
        let f = *prog.func(fid);
        let params: Vec<PatId> = prog.list(f.params).collect();
        for _ in &params {
            let r = self.alloc();
            self.set_live(r);
        }
        let mut h = Hoist::default();
        let stmts = match f.body {
            Body::Block(l) => Some(l),
            Body::Expr(_) => None,
            Body::Skipped(_) => {
                self.unsup("a function that the pre-parser did not read");
                None
            }
        };
        if let Some(l) = stmts {
            self.hoist_list(l, true, &mut h);
        }
        let mut decls: Vec<Decl<'a>> = Vec::new();
        for (i, p) in params.iter().enumerate() {
            let mut pt = *prog.pat(*p);
            if let PatKind::Assign { target, .. } = pt.kind {
                pt = *prog.pat(target);
            }
            match pt.kind {
                PatKind::Ident => decls.push(Decl { name: prog.name(pt.span), kind: VK::Param, end: 0, reg: Some(i as u32) }),
                // A rest parameter: `CreateRest` makes the array in the register of the parameter.
                PatKind::Rest(t) if matches!(prog.pat(t).kind, PatKind::Ident) => {
                    decls.push(Decl { name: prog.name(prog.pat(t).span), kind: VK::Param, end: 0, reg: Some(i as u32) })
                }
                _ => {
                    let mut names = Vec::new();
                    self.pat_names(*p, &mut names);
                    decls.extend(names.into_iter().map(|(name, _)| Decl { name, kind: VK::Param, end: 0, reg: None }));
                }
            }
        }
        // In a derived constructor `this` is a variable that is the hole until `super()` returns.
        let uses_this = !self.is_arrow && (self.cap.contains_key("this") || self.derived);
        if uses_this {
            let (kind, end) = if self.derived { (VK::Let, u32::MAX) } else { (VK::Var, 0) };
            decls.push(Decl { name: Cow::Borrowed("this"), kind, end, reg: None });
        }
        let uses_nt = !self.is_arrow && self.cap.contains_key("new.target");
        if uses_nt {
            decls.push(Decl { name: Cow::Borrowed("new.target"), kind: VK::Var, end: 0, reg: None });
        }
        decls.extend(h.vars.iter().map(|n| Decl { name: n.clone(), kind: VK::Var, end: 0, reg: None }));
        decls.extend(h.funcs.iter().map(|(_, n)| Decl { name: n.clone(), kind: VK::Func, end: 0, reg: None }));
        decls.extend(h.lex.iter().map(|(n, k, e)| Decl { name: n.clone(), kind: *k, end: *e, reg: None }));
        // `arguments`: a variable that holds the arguments object. A declaration of that name (a parameter, a `var`) comes first.
        let uses_args = !self.is_arrow && (self.fn_flags & facts::ARGS != 0 || self.cap.contains_key("arguments")) && !decls.iter().any(|d| d.name == "arguments");
        if uses_args {
            decls.push(Decl { name: Cow::Borrowed("arguments"), kind: VK::Var, end: 0, reg: None });
        }
        // The name of a named function expression, when the text of the function uses it and nothing else binds it.
        if self.self_name
            && let (Some(n), Some(id)) = (f.name, self.fn_id)
        {
            let name = prog.name(n);
            let ff = self.facts.get(id);
            if !decls.iter().any(|d| d.name == name) && self.may_occur(&name, ff.parse_at, ff.end) {
                decls.push(Decl { name, kind: VK::Callee, end: 0, reg: None });
            }
        }
        let inited = match stmts {
            Some(l) => self.top_inits(l, f.span.start),
            None => Default::default(),
        };
        let span = Span { start: 0, end: u32::MAX };
        let mark = self.enter(span, &decls, false)?;
        // Variables that no path has written yet: `var` is `undefined`, a `let` that can be read early is the hole.
        for d in &decls {
            let Some(v) = self.find_var(&d.name) else { continue };
            let (home, kind) = (v.home, v.kind);
            match (kind, home, d.reg) {
                // Written by `CreateArgs` below.
                (VK::Var, _, None) if matches!(&*d.name, "arguments" | "new.target") => {}
                (VK::Callee, Home::Reg(r), None) => {
                    self.op(Op::LoadCallee, r, 0, 0);
                    self.set_live(r);
                }
                (VK::Callee, Home::Ctx { level, slot }, None) => {
                    let t = self.alloc();
                    self.op(Op::LoadCallee, t, 0, 0);
                    self.op(Op::InitCtx, t, self.depth - level, slot);
                    self.free(t);
                }
                // A `var` whose first mention is an initialized declaration of the top level needs no `undefined` at the entry.
                (VK::Var, Home::Reg(_), None) if inited.contains(&d.name) => {}
                (VK::Var, Home::Reg(r), None) => {
                    self.op(Op::LoadUndef, r, 0, 0);
                    self.set_live(r);
                }
                (VK::Var, Home::Ctx { .. }, None) if inited.contains(&d.name) => {}
                (VK::Var, Home::Ctx { level, slot }, None) if &*d.name != "this" => {
                    let t = self.alloc();
                    self.op(Op::LoadUndef, t, 0, 0);
                    self.op(Op::InitCtx, t, self.depth - level, slot);
                    self.free(t);
                }
                (VK::Let | VK::Const | VK::Class, Home::Reg(r), _) if self.early_use(d, f.span.start) => {
                    self.op(Op::LoadHole, r, 0, 0);
                    self.set_live(r);
                }
                _ => {}
            }
        }
        if uses_args && let Some(home) = self.find_var("arguments").map(|v| v.home) {
            match home {
                Home::Reg(r) => {
                    self.op(Op::CreateArgs, r, 0, 0);
                    self.set_live(r);
                }
                Home::Ctx { level, slot } => {
                    let t = self.alloc();
                    self.op(Op::CreateArgs, t, 0, 0);
                    self.op(Op::InitCtx, t, self.depth - level, slot);
                    self.free(t);
                }
            }
        }
        if uses_nt && let Some(home) = self.find_var("new.target").map(|v| v.home) {
            match home {
                Home::Reg(r) => {
                    self.op(Op::LoadNewTarget, r, 0, 0);
                    self.set_live(r);
                }
                Home::Ctx { level, slot } => {
                    let t = self.alloc();
                    self.op(Op::LoadNewTarget, t, 0, 0);
                    self.op(Op::InitCtx, t, self.depth - level, slot);
                    self.free(t);
                }
            }
        }
        if uses_this && !self.derived && let Some(Var { home: Home::Ctx { level, slot }, .. }) = self.find_var("this") {
            let (level, slot) = (*level, *slot);
            let t = self.alloc();
            self.op(self.load_this_op(), t, 0, 0);
            self.op(Op::InitCtx, t, self.depth - level, slot);
            self.free(t);
        }
        // Parameters: a default, a pattern, a captured one moves to its slot.
        for (i, p) in params.iter().enumerate() {
            let r = i as u32;
            let pt = *prog.pat(*p);
            if let PatKind::Rest(t) = pt.kind {
                self.op(Op::CreateRest, r, i as u32, 0);
                let tp = *prog.pat(t);
                if matches!(tp.kind, PatKind::Ident) {
                    if let Some(Home::Ctx { level, slot }) = self.find_var(&prog.name(tp.span)).map(|v| v.home) {
                        self.op(Op::InitCtx, r, self.depth - level, slot);
                    }
                } else {
                    self.bind_pattern(t, r, Bind::Init)?;
                }
                continue;
            }
            let (target, default) = match pt.kind {
                PatKind::Assign { target, default } => (target, Some(default)),
                _ => (*p, None),
            };
            if let Some(d) = default {
                let skip = self.label();
                self.jmp_r(Op::JSNeUndef, r, skip);
                if matches!(prog.pat(target).kind, PatKind::Ident) {
                    let name = prog.name(prog.pat(target).span);
                    self.name_hint(d, &name);
                }
                // A default that names this parameter or a later one reads it before it is set: a `ReferenceError`.
                let ds = prog.expr(d).span;
                let m = self.mark();
                if params[i..].iter().any(|&q| {
                    let t = match prog.pat(q).kind {
                        PatKind::Assign { target, .. } => target,
                        _ => q,
                    };
                    matches!(prog.pat(t).kind, PatKind::Ident) && self.may_occur(&prog.name(prog.pat(t).span), ds.start, ds.end)
                })
                {
                    let h = self.alloc();
                    self.op(Op::LoadHole, h, 0, 0);
                    self.set_live(h);
                    self.param_dz = Some((i as u32, params.len() as u32, h));
                }
                self.to(d, r)?;
                self.param_dz = None;
                self.free(m);
                self.bind(skip);
            }
            let tp = *prog.pat(target);
            if matches!(tp.kind, PatKind::Ident) {
                let name = prog.name(tp.span);
                if let Some(Home::Ctx { level, slot }) = self.find_var(&name).map(|v| v.home) {
                    self.op(Op::InitCtx, r, self.depth - level, slot);
                }
            } else if !matches!(tp.kind, PatKind::Rest(_)) {
                self.bind_pattern(target, r, Bind::Init)?;
            }
        }
        // Function declarations are made before the first statement.
        for (f, name) in &h.funcs {
            self.declare_function(*f, name)?;
        }
        match f.body {
            Body::Block(l) => {
                for s in prog.list(l) {
                    self.stmt(s)?;
                }
            }
            Body::Expr(e) => {
                let m = self.mark();
                let r = self.any(e)?;
                self.op(Op::Ret, r, 0, 0);
                self.free(m);
            }
            Body::Skipped(_) => {}
        }
        if self.derived && !self.dead {
            self.derived_return(None)?;
        }
        self.leave(mark);
        Ok(())
    }

    /// The return of a derived constructor: an object is the result; `undefined` gives `this`, which throws when `super()` has not run.
    fn derived_return(&mut self, value: Option<u32>) -> CR {
        let m = self.mark();
        let v = match value {
            Some(v) => v,
            None => {
                let u = self.alloc();
                self.op(Op::LoadUndef, u, 0, 0);
                self.set_live(u);
                u
            }
        };
        // The raw slot, with no dead-zone check: `return {}` is legal before `super()`.
        let t = match self.resolve("this", 0) {
            Res::Reg { reg, .. } => reg,
            Res::Ctx { hops, slot, .. } => {
                let t = self.alloc();
                self.op(Op::GetCtx, t, hops, slot);
                self.set_live(t);
                t
            }
            Res::Global => {
                let t = self.alloc();
                self.op(Op::LoadHole, t, 0, 0);
                self.set_live(t);
                t
            }
        };
        self.op(Op::RetDerived, v, t, 0);
        self.free(m);
        Ok(())
    }

    /// The `var` names whose first mention in the text is a declaration with an initializer in a statement of the top level (or in the head
    /// of a top-level `for`): the code writes them before any read, on every path.
    fn top_inits(&self, list: List<Stmt>, from: u32) -> std::collections::HashSet<Cow<'a, str>> {
        let prog = self.prog;
        let mut out = std::collections::HashSet::new();
        for s in prog.list(list) {
            let mut v = prog.stmt(s).kind;
            if let StmtKind::For { init: ForInit::Var(i), .. } = v {
                v = prog.stmt(i).kind;
            }
            let StmtKind::Var { kind: VarKind::Var, decls } = v else { continue };
            for d in prog.list(decls) {
                let d = *prog.decl(d);
                let t = *prog.pat(d.target);
                let (PatKind::Ident, Some(init)) = (t.kind, d.init) else { continue };
                let name = prog.name(t.span);
                let ispan = prog.expr(init).span;
                if !self.may_occur(&name, from, t.span.start) && !self.may_occur(&name, ispan.start, ispan.end) {
                    out.insert(name);
                }
            }
        }
        out
    }

    /// Make the closure of a function declaration and store it in its variable.
    fn declare_function(&mut self, f: FuncId, name: &str) -> CR {
        let m = self.mark();
        let t = match self.find_var(name).map(|v| v.home) {
            Some(Home::Reg(r)) => r,
            _ => self.alloc(),
        };
        self.function(f, t, false)?;
        self.set_live(t);
        self.store_var(name, u32::MAX, t, true)?;
        self.free(m);
        Ok(())
    }

    /// The top level of a script: function declarations become globals first.
    pub(crate) fn script_body(&mut self, list: List<Stmt>) -> CR {
        let mut h = Hoist::default();
        self.hoist_list(list, true, &mut h);
        // The top-level `let`, `const` and `class` names are variables of the script (a register, or a context slot when a function
        // uses them), not properties of the global object: they have a dead zone, and a `const` cannot be assigned.
        let mut decls: Vec<Decl<'a>> = h.lex.iter().map(|(n, k, e)| Decl { name: n.clone(), kind: *k, end: *e, reg: None }).collect();
        // An arrow function at the top level reads the `this` of the script, not the one of its call.
        let this_captured = self.cap.contains_key("this");
        if this_captured {
            decls.push(Decl { name: Cow::Borrowed("this"), kind: VK::Var, end: 0, reg: None });
        }
        let mark = if decls.is_empty() { None } else { Some(self.enter(Span { start: 0, end: u32::MAX }, &decls, false)?) };
        // The scripts that run later read these names through the global lexical environment of the realm. A name in a context slot is
        // declared now (`DeclLex`). A name in a register stays there, so a loop at the top level keeps its speed: the script checks the
        // name now (`CheckLex`) and hands its value over at the end.
        let mut in_regs = Vec::new();
        for (n, k, _) in &h.lex {
            let s = self.site_of(n)?;
            match self.find_var(n) {
                Some(Var { home: Home::Ctx { slot, .. }, .. }) => {
                    let slot = *slot;
                    self.op(if *k == VK::Const { Op::DeclLexC } else { Op::DeclLex }, slot, s, 0);
                }
                Some(Var { home: Home::Reg(r), .. }) => {
                    let r = *r;
                    self.op(Op::CheckLex, 0, s, 0);
                    in_regs.push((r, n.clone(), *k == VK::Const));
                }
                None => {}
            }
        }
        if this_captured && let Some(Var { home: Home::Ctx { level, slot }, .. }) = self.find_var("this") {
            let (level, slot) = (*level, *slot);
            let t = self.alloc();
            self.op(Op::LoadThis, t, 0, 0);
            self.op(Op::InitCtx, t, self.depth - level, slot);
            self.free(t);
        }
        for d in &decls {
            if let Some(v) = self.find_var(&d.name)
                && let (VK::Let | VK::Const | VK::Class, Home::Reg(r)) = (v.kind, v.home)
                && self.early_use(d, 0)
            {
                self.op(Op::LoadHole, r, 0, 0);
                self.set_live(r);
            }
        }
        // Each name exists as a global (`undefined`) before the first statement runs, so a read or `typeof` finds it, and a `var` with no
        // initializer is not lost.
        let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
        for n in h.vars.iter().chain(h.funcs.iter().map(|(_, n)| n)) {
            if seen.insert(n) {
                let s = self.site_of(n)?;
                self.op(Op::DeclGlobal, 0, s, 0);
            }
        }
        for (f, name) in &h.funcs {
            self.declare_function(*f, name)?;
        }
        for s in self.prog.list(list) {
            self.stmt(s)?;
        }
        if !in_regs.is_empty() {
            for (r, ..) in &in_regs {
                self.set_live(*r);
            }
            self.op(Op::PushCtx, 0, in_regs.len() as u32, 0);
            for (i, (r, n, konst)) in in_regs.iter().enumerate() {
                let (r, konst) = (*r, *konst);
                let s = self.site_of(n)?;
                self.op(Op::InitCtx, r, 0, i as u32);
                self.op(if konst { Op::DeclLexC } else { Op::DeclLex }, i as u32, s, 0);
            }
            self.op(Op::PopCtx, 0, 0, 0);
        }
        if let Some(m) = mark {
            self.leave(m);
        }
        Ok(())
    }

    // ------------------------------------------------------------ blocks

    fn block_decls(&self, list: List<Stmt>) -> (Vec<Decl<'a>>, Vec<BlockFn<'a>>) {
        let prog = self.prog;
        let mut decls = Vec::new();
        let mut funcs = Vec::new();
        for s in prog.list(list) {
            let st = *prog.stmt(s);
            match st.kind {
                StmtKind::Var { kind: kind @ (VarKind::Let | VarKind::Const), decls: ds } => {
                    for d in prog.list(ds) {
                        let d = *prog.decl(d);
                        let mut names = Vec::new();
                        self.pat_names(d.target, &mut names);
                        for (name, _) in names {
                            decls.push(Decl { name, kind: if kind == VarKind::Let { VK::Let } else { VK::Const }, end: d.span.end, reg: None });
                        }
                    }
                }
                StmtKind::Class(c) => {
                    if let Some(n) = prog.class(c).name {
                        decls.push(Decl { name: prog.name(n), kind: VK::Class, end: st.span.end, reg: None });
                    }
                }
                StmtKind::Function(f) => {
                    if let Some(n) = prog.func(f).name {
                        let name = prog.name(n);
                        if self.strict {
                            decls.push(Decl { name: name.clone(), kind: VK::Func, end: 0, reg: None });
                        }
                        funcs.push((f, name, self.strict));
                    }
                }
                _ => {}
            }
        }
        (decls, funcs)
    }

    /// Enter the scope of a list of statements (a block, a case list). `None` when it declares nothing.
    fn enter_block(&mut self, list: List<Stmt>, span: Span, switch: bool) -> CR<Option<ScopeMark>> {
        let (decls, funcs) = self.block_decls(list);
        if decls.is_empty() && funcs.is_empty() {
            return Ok(None);
        }
        let mark = self.enter(span, &decls, switch)?;
        for d in &decls {
            if let Some(v) = self.find_var(&d.name)
                && let (VK::Let | VK::Const | VK::Class, Home::Reg(r)) = (v.kind, v.home)
                && (v.always_tdz || self.early_use(d, span.start))
            {
                self.op(Op::LoadHole, r, 0, 0);
                self.set_live(r);
            }
        }
        for (f, name, _) in &funcs {
            self.declare_function(*f, name)?;
        }
        Ok(Some(mark))
    }

    fn block(&mut self, list: List<Stmt>, span: Span) -> CR {
        let mark = self.enter_block(list, span, false)?;
        for s in self.prog.list(list) {
            self.stmt(s)?;
        }
        if let Some(m) = mark {
            self.leave(m);
        }
        Ok(())
    }

    // ------------------------------------------------------------ statements

    pub(crate) fn stmt(&mut self, s: StmtId) -> CR {
        let prog = self.prog;
        let st = *prog.stmt(s);
        let m = self.mark();
        match st.kind {
            StmtKind::Expr(e) => self.effect(e)?,
            StmtKind::Directive(_) | StmtKind::Empty | StmtKind::Debugger | StmtKind::Function(_) => {}
            StmtKind::Var { kind, decls } => self.var_decls(kind, decls)?,
            StmtKind::Class(c) => {
                let t = self.alloc();
                self.class(c, t)?;
                self.set_live(t);
                if let Some(n) = prog.class(c).name {
                    self.store_var(&prog.name(n), u32::MAX, t, true)?;
                }
            }
            StmtKind::Return(arg) => self.ret(arg)?,
            StmtKind::If { test, cons, alt } => {
                let end = self.label();
                match alt {
                    // `if (t) break;` is one conditional jump when the jump needs no cleanup.
                    None if let Some(to) = self.plain_jump(cons) => self.cond(test, true, to)?,
                    None => {
                        self.cond(test, false, end)?;
                        self.stmt(cons)?;
                    }
                    Some(a) => {
                        let els = self.label();
                        self.cond(test, false, els)?;
                        self.stmt(cons)?;
                        self.jmp(end);
                        self.bind(els);
                        self.stmt(a)?;
                    }
                }
                self.bind(end);
            }
            StmtKind::While { test, body } => {
                let (head, cont, brk) = (self.label(), self.label(), self.label());
                self.bind(head);
                self.cond(test, false, brk)?;
                self.push_loop(brk, cont, None);
                self.stmt(body)?;
                self.frames.pop();
                self.bind(cont);
                self.jmp_back(head);
                self.bind(brk);
            }
            StmtKind::DoWhile { body, test } => {
                let (head, cont, brk) = (self.label(), self.label(), self.label());
                self.bind(head);
                self.mark_used(head);
                self.push_loop(brk, cont, None);
                self.stmt(body)?;
                self.frames.pop();
                self.bind(cont);
                self.cond(test, false, brk)?;
                self.jmp_back(head);
                self.bind(brk);
            }
            StmtKind::For { init, test, update, body } => self.for_stmt(st.span, init, test, update, body)?,
            StmtKind::ForIn { left, right, body } => self.for_in_of(left, right, body, false)?,
            StmtKind::ForOf { left, right, body, is_await } => {
                if is_await {
                    self.unsup("for await");
                }
                self.for_in_of(left, right, body, true)?
            }
            StmtKind::Break(label) => self.jump_out(label, true)?,
            StmtKind::Continue(label) => self.jump_out(label, false)?,
            StmtKind::Throw(e) => {
                let r = self.any(e)?;
                self.line(st.span.start);
                self.op(Op::Throw, r, 0, 0);
            }
            StmtKind::Try { block, handler, finalizer } => self.try_stmt(block, handler, finalizer)?,
            StmtKind::Switch { disc, cases } => self.switch(disc, cases, st.span)?,
            StmtKind::Labeled { label, body } => {
                let name = self.name_of(label).into_owned();
                self.pending.push(name);
                let is_loop = matches!(
                    prog.stmt(body).kind,
                    StmtKind::For { .. } | StmtKind::ForIn { .. } | StmtKind::ForOf { .. } | StmtKind::While { .. } | StmtKind::DoWhile { .. } | StmtKind::Labeled { .. }
                );
                if is_loop {
                    self.stmt(body)?;
                } else {
                    let brk = self.label();
                    let labels = std::mem::take(&mut self.pending);
                    self.push_frame(labels, FrameKind::Block { brk, is_switch: false });
                    self.stmt(body)?;
                    self.frames.pop();
                    self.bind(brk);
                }
            }
            StmtKind::Block(list) => self.block(list, st.span)?,
            StmtKind::With { .. } => self.unsup("with"),
            StmtKind::ExportDefault(..) => self.unsup("a module"),
        }
        self.free(m);
        Ok(())
    }

    fn var_decls(&mut self, kind: VarKind, decls: List<Declarator>) -> CR {
        let prog = self.prog;
        for d in prog.list(decls) {
            let d = *prog.decl(d);
            let t = *prog.pat(d.target);
            let m = self.mark();
            match (t.kind, d.init) {
                (PatKind::Ident, Some(init)) => {
                    let name = prog.name(t.span);
                    self.name_hint(init, &name);
                    match self.find_var(&name).map(|v| v.home) {
                        Some(Home::Reg(r)) => {
                            self.to(init, r)?;
                        }
                        _ => {
                            let v = self.any(init)?;
                            self.store_var(&name, t.span.start, v, true)?;
                        }
                    }
                }
                (PatKind::Ident, None) => {
                    // `var a;` does nothing. `let a;` is `undefined`.
                    if kind != VarKind::Var {
                        let name = prog.name(t.span);
                        let v = self.alloc();
                        self.op(Op::LoadUndef, v, 0, 0);
                        self.set_live(v);
                        self.store_var(&name, t.span.start, v, true)?;
                    }
                }
                (_, Some(init)) => {
                    let v = self.any(init)?;
                    let v = if self.is_var_reg(v) {
                        let t = self.alloc();
                        self.op(Op::Mov, t, v, 0);
                        self.set_live(t);
                        t
                    } else {
                        v
                    };
                    self.bind_pattern(d.target, v, Bind::Init)?;
                }
                (_, None) => {}
            }
            self.free(m);
        }
        Ok(())
    }

    // ------------------------------------------------------------ jumps out

    fn push_frame(&mut self, labels: Vec<String>, kind: FrameKind) {
        let f = Frame { labels, kind, depth: self.depth, brk_depth: self.depth, ctx: self.cur_ctx.clone(), nscopes: self.scopes.len(), pieces: Vec::new(), open: None };
        self.frames.push(f);
    }

    /// The handler of the frame on top covers the code from here.
    fn guard_top(&mut self) {
        let at = self.asm.len();
        self.frames.last_mut().expect("a frame").open = Some(at);
    }

    /// The handler of frame `k` stops covering the code from here (see `Frame::pieces`).
    fn pause(&mut self, k: usize) {
        let now = self.asm.len();
        let f = &mut self.frames[k];
        if let Some(o) = f.open.take() {
            f.pieces.push((o, now));
        }
    }

    /// The frames in `ks` cover the code from here again.
    fn resume(&mut self, ks: &[usize]) {
        let now = self.asm.len();
        for &k in ks {
            self.frames[k].open = Some(now);
        }
    }

    fn push_loop(&mut self, brk: Lb, cont: Lb, iter: Option<u32>) {
        let labels = std::mem::take(&mut self.pending);
        self.push_frame(labels, FrameKind::Loop { brk, cont, iter });
    }

    fn emit_pops(&mut self, from: u32, to: u32) {
        for _ in to..from {
            self.op(Op::PopCtx, 0, 0, 0);
        }
    }

    /// Run what a jump out of the frames above `to` must run: close iterators and repeat the finalizers (innermost first). The contexts
    /// are popped down to the context of each finalizer. Gives the depth that the code is at when it returns.
    fn unwind(&mut self, to: usize) -> CR<u32> {
        let (d0, c0) = (self.depth, self.cur_ctx.clone());
        let mut rt = self.depth;
        let mut paused: Vec<usize> = Vec::new();
        let mut k = self.frames.len();
        while k > to {
            k -= 1;
            match &self.frames[k].kind {
                FrameKind::Loop { iter: Some(rec), .. } => {
                    let rec = *rec;
                    if self.frames[k].open.is_some() {
                        self.pause(k);
                        paused.push(k);
                    }
                    self.op(Op::IterClose, rec, 0, 0);
                }
                FrameKind::Try { fin: Some(fin), .. } => {
                    let fin = *fin;
                    let (depth, ctx, nscopes) = (self.frames[k].depth, self.frames[k].ctx.clone(), self.frames[k].nscopes);
                    self.emit_pops(rt, depth);
                    rt = depth;
                    if self.frames[k].open.is_some() {
                        self.pause(k);
                        paused.push(k);
                    }
                    // The finalizer sees the scopes and contexts that were there when the `try` began, and no frame above it.
                    let tail = self.frames.split_off(k);
                    let scopes = self.scopes.split_off(nscopes);
                    self.depth = depth;
                    self.cur_ctx = ctx;
                    let r = self.stmt(fin);
                    self.scopes.extend(scopes);
                    self.frames.extend(tail);
                    r?;
                }
                _ => {}
            }
        }
        self.resume(&paused);
        self.depth = d0;
        self.cur_ctx = c0;
        Ok(rt)
    }

    /// The label of a `break` or `continue` (alone or alone in a block) that needs no pop and no cleanup, or `None`.
    fn plain_jump(&self, s: StmtId) -> Option<Lb> {
        let prog = self.prog;
        let (label, is_break) = match prog.stmt(s).kind {
            StmtKind::Break(l) => (l, true),
            StmtKind::Continue(l) => (l, false),
            StmtKind::Block(l) => {
                let mut it = prog.list(l);
                let (first, rest) = (it.next()?, it.next());
                return if rest.is_none() && matches!(prog.stmt(first).kind, StmtKind::Break(_) | StmtKind::Continue(_)) { self.plain_jump(first) } else { None };
            }
            _ => return None,
        };
        let name = label.map(|l| self.name_of(l));
        let k = self.frames.iter().rposition(|f| match (&name, &f.kind) {
            (Some(n), FrameKind::Loop { .. } | FrameKind::Block { .. }) => f.labels.iter().any(|l| l == &**n),
            (None, FrameKind::Loop { .. }) => true,
            (None, FrameKind::Block { is_switch, .. }) => is_break && *is_switch,
            _ => false,
        })?;
        if self.frames[k + 1..].iter().any(|f| matches!(f.kind, FrameKind::Try { fin: Some(_), .. } | FrameKind::Loop { iter: Some(_), .. })) {
            return None;
        }
        match self.frames[k].kind {
            FrameKind::Loop { brk, iter, .. } if is_break => (iter.is_none() && self.depth == self.frames[k].brk_depth).then_some(brk),
            FrameKind::Loop { cont, .. } => (self.depth == self.frames[k].depth).then_some(cont),
            FrameKind::Block { brk, .. } => (self.depth == self.frames[k].brk_depth).then_some(brk),
            FrameKind::Try { .. } => None,
        }
    }

    fn jump_out(&mut self, label: Option<Span>, is_break: bool) -> CR {
        let name = label.map(|l| self.name_of(l).into_owned());
        let found = self.frames.iter().rposition(|f| match (&name, &f.kind) {
            (Some(n), FrameKind::Loop { .. } | FrameKind::Block { .. }) => f.labels.iter().any(|l| l == n),
            (None, FrameKind::Loop { .. }) => true,
            (None, FrameKind::Block { is_switch, .. }) => is_break && *is_switch,
            _ => false,
        });
        let Some(k) = found else {
            self.unsup("a break or continue with no target");
            return Ok(());
        };
        let rt = self.unwind(k + 1)?;
        let (target, depth) = match self.frames[k].kind {
            FrameKind::Loop { brk, cont, iter } => {
                if is_break {
                    if let Some(rec) = iter {
                        // A `return()` that throws is not for the handler of this loop: it would close the iterator twice.
                        let was_open = self.frames[k].open.is_some();
                        self.pause(k);
                        self.op(Op::IterClose, rec, 0, 0);
                        if was_open {
                            self.resume(&[k]);
                        }
                    }
                    (brk, self.frames[k].brk_depth)
                } else {
                    (cont, self.frames[k].depth)
                }
            }
            FrameKind::Block { brk, .. } => (brk, self.frames[k].brk_depth),
            FrameKind::Try { .. } => unreachable!("a try is no target"),
        };
        self.emit_pops(rt, depth);
        self.jmp(target);
        Ok(())
    }

    fn ret(&mut self, arg: Option<ExprId>) -> CR {
        let cleanup = self.frames.iter().any(|f| matches!(f.kind, FrameKind::Try { fin: Some(_), .. } | FrameKind::Loop { iter: Some(_), .. }));
        match arg {
            None if self.derived => {
                self.unwind(0)?;
                self.derived_return(None)?;
            }
            Some(e) if self.derived => {
                let t = self.alloc();
                self.to(e, t)?;
                self.unwind(0)?;
                self.derived_return(Some(t))?;
            }
            None => {
                self.unwind(0)?;
                self.op(Op::RetUndef, 0, 0, 0);
            }
            Some(e) if !cleanup => {
                let r = self.any(e)?;
                self.op(Op::Ret, r, 0, 0);
            }
            Some(e) => {
                // The value is kept in a temporary: a finalizer can change a variable.
                let t = self.alloc();
                self.to(e, t)?;
                self.unwind(0)?;
                self.op(Op::Ret, t, 0, 0);
            }
        }
        Ok(())
    }

    // ------------------------------------------------------------ loops

    fn for_stmt(&mut self, span: Span, init: ForInit, test: Option<ExprId>, update: Option<ExprId>, body: StmtId) -> CR {
        let prog = self.prog;
        let labels = std::mem::take(&mut self.pending);
        // A `let` or `const` in the head is a scope of the loop.
        let mut scope = None;
        if let ForInit::Var(v) = init
            && let StmtKind::Var { kind: kind @ (VarKind::Let | VarKind::Const), decls } = prog.stmt(v).kind
        {
            let mut ds = Vec::new();
            for d in prog.list(decls) {
                let d = *prog.decl(d);
                let mut names = Vec::new();
                self.pat_names(d.target, &mut names);
                ds.extend(names.into_iter().map(|(name, _)| Decl { name, kind: if kind == VarKind::Let { VK::Let } else { VK::Const }, end: d.span.end, reg: None }));
            }
            scope = Some(self.enter(span, &ds, false)?);
        }
        match init {
            ForInit::None => {}
            ForInit::Var(v) => self.stmt(v)?,
            ForInit::Expr(e) => self.effect(e)?,
        }
        let per_iteration = scope.as_ref().is_some_and(|m| self.depth > m.ctx);
        let (head, cont, brk) = (self.label(), self.label(), self.label());
        self.bind(head);
        self.mark_used(head);
        if let Some(t) = test {
            self.cond(t, false, brk)?;
        }
        self.pending = labels;
        self.push_loop(brk, cont, None);
        self.stmt(body)?;
        self.frames.pop();
        self.bind(cont);
        if per_iteration {
            self.op(Op::CopyCtx, 0, 0, 0);
        }
        if let Some(u) = update {
            self.effect(u)?;
        }
        self.jmp_back(head);
        self.bind(brk);
        if let Some(m) = scope {
            self.leave(m);
        }
        Ok(())
    }

    fn for_in_of(&mut self, left: ForLeft, right: ExprId, body: StmtId, of: bool) -> CR {
        let prog = self.prog;
        let labels = std::mem::take(&mut self.pending);
        let m = self.mark();
        let v = self.any(right)?;
        let rec = self.window(3);
        self.line(prog.expr(right).span.start);
        self.op(if of { Op::IterOpen } else { Op::ForInOpen }, rec, v, 0);
        (0..3).for_each(|i| self.set_live(rec + i));
        let (head, cont, brk) = (self.label(), self.label(), self.label());
        // An exception in the body of a `for-of` closes the iterator: a handler for the body that does it and throws again.
        let ex = of.then(|| self.alloc());
        self.bind(head);
        self.mark_used(head);
        let t = self.alloc();
        self.jmp_rx(Op::IterNext, t, rec, brk);
        self.set_live(t);
        let guard = ex.map(|ex| (ex, self.live.clone(), self.asm.len(), self.depth));
        // The binding of `let` and `const` is new in each iteration.
        let mut scope = None;
        let (target, kind) = match left {
            ForLeft::Var(s) => match prog.stmt(s).kind {
                StmtKind::Var { kind, decls } => {
                    let d = *prog.decl(prog.list(decls).next().expect("a for head declares one name"));
                    (d.target, Some(kind))
                }
                _ => unreachable!("a for head is a declaration"),
            },
            ForLeft::Pat(p) => (p, None),
        };
        if let Some(k @ (VarKind::Let | VarKind::Const)) = kind {
            let mut names = Vec::new();
            self.pat_names(target, &mut names);
            let ds: Vec<Decl> = names.into_iter().map(|(name, _)| Decl { name, kind: if k == VarKind::Let { VK::Let } else { VK::Const }, end: 0, reg: None }).collect();
            scope = Some(self.enter(prog.stmt(body).span, &ds, false)?);
        }
        self.bind_pattern(target, t, if kind.is_some() { Bind::Init } else { Bind::Assign })?;
        self.pending = labels;
        self.push_loop(brk, cont, of.then_some(rec));
        {
            // `break` leaves the scope of the binding too; `continue` goes to its end.
            let outer = scope.as_ref().map_or(self.depth, |m| m.ctx);
            let top = self.frames.last_mut().expect("the loop frame");
            top.brk_depth = outer;
            // The handler covers the binding of the head and the body, except the iterator closes that a jump out repeats.
            top.open = guard.as_ref().map(|g| g.2);
        }
        self.stmt(body)?;
        let pieces = self.pop_try();
        self.bind(cont);
        if let Some(sc) = scope {
            self.leave(sc);
        }
        self.jmp_back(head);
        if let Some((ex, live, _, depth)) = guard
            && !pieces.is_empty()
        {
            // After the back edge, where no path falls in: every exit of the loop jumps to `brk`.
            let lb = self.label();
            self.mark_used(lb);
            self.bind(lb);
            self.live = live;
            self.set_live(ex);
            self.op(Op::IterClose, rec, 0, 0);
            self.op(Op::Throw, ex, 0, 0);
            for (start, end) in pieces {
                self.handlers.push(HandlerRec { start, end, target: lb, reg: ex, depth });
            }
        }
        self.bind(brk);
        self.free(m);
        Ok(())
    }

    // ------------------------------------------------------------ switch

    fn switch(&mut self, disc: ExprId, cases: List<Case>, span: Span) -> CR {
        let prog = self.prog;
        let cases: Vec<Case> = prog.list(cases).map(|c| *prog.case(c)).collect();
        let m = self.mark();
        let d = self.any(disc)?;
        let d = if self.is_var_reg(d) && cases.iter().any(|c| c.test.is_some_and(|t| self.may_clobber(d, t))) {
            let t = self.alloc();
            self.op(Op::Mov, t, d, 0);
            self.set_live(t);
            t
        } else {
            d
        };
        // One scope holds the declarations of all the cases.
        let (mut decls, mut funcs) = (Vec::new(), Vec::new());
        for c in &cases {
            let (ds, fs) = self.block_decls(c.body);
            decls.extend(ds);
            funcs.extend(fs);
        }
        let scope = if decls.is_empty() && funcs.is_empty() {
            None
        } else {
            let mk = self.enter(span, &decls, true)?;
            for dcl in &decls {
                if let Some(v) = self.find_var(&dcl.name)
                    && let (VK::Let | VK::Const | VK::Class, Home::Reg(r)) = (v.kind, v.home)
                {
                    self.op(Op::LoadHole, r, 0, 0);
                    self.set_live(r);
                }
            }
            for (f, name, _) in &funcs {
                self.declare_function(*f, name)?;
            }
            Some(mk)
        };
        let labels: Vec<Lb> = cases.iter().map(|_| self.label()).collect();
        let end = self.label();
        let default = cases.iter().position(|c| c.test.is_none()).map_or(end, |i| labels[i]);
        if let Some(plan) = self.table_plan(&cases)? {
            self.switch_table(d, plan, &labels, default);
        } else {
            for (i, c) in cases.iter().enumerate() {
                if let Some(t) = c.test {
                    self.case_jump(d, t, labels[i])?;
                }
            }
            self.jmp(default);
        }
        let pend = std::mem::take(&mut self.pending);
        self.push_frame(pend, FrameKind::Block { brk: end, is_switch: true });
        for (i, c) in cases.iter().enumerate() {
            self.bind(labels[i]);
            for s in prog.list(c.body) {
                self.stmt(s)?;
            }
        }
        self.frames.pop();
        self.bind(end);
        if let Some(mk) = scope {
            self.leave(mk);
        }
        self.free(m);
        Ok(())
    }

    /// The keys of a switch that a table can dispatch: every case is an integer constant, or every case is a string constant (`MIN_INT_CASES`,
    /// `MIN_STR_CASES`, `INT_SPAN`: the thresholds). A key has the index of the first case that has it, so a later duplicate gets no entry.
    /// A case that is not a constant, or a mix of kinds, gives `None`: the chain keeps the order of evaluation and the strict equality.
    fn table_plan(&mut self, cases: &[Case]) -> CR<Option<Plan>> {
        let mut ints: Vec<(i32, usize)> = Vec::new();
        let mut strs: Vec<(u32, usize)> = Vec::new();
        let (mut seen_i, mut seen_s) = (HashSet::new(), HashSet::new());
        for (i, c) in cases.iter().enumerate() {
            let Some(t) = c.test else { continue };
            match self.konst(t)? {
                Some(K::Int(v)) if strs.is_empty() => {
                    if seen_i.insert(v) {
                        ints.push((v, i));
                    }
                }
                Some(K::Str(a)) if ints.is_empty() => {
                    if seen_s.insert(a) {
                        strs.push((a, i));
                    }
                }
                _ => return Ok(None),
            }
        }
        let n = ints.len().max(strs.len());
        if !ints.is_empty() {
            let (lo, hi) = ints.iter().fold((i32::MAX, i32::MIN), |(lo, hi), &(v, _)| (lo.min(v), hi.max(v)));
            let span = hi as i64 - lo as i64 + 1;
            return Ok((n >= MIN_INT_CASES && span < INT_SPAN * n as i64).then_some(Plan::Int(ints)));
        }
        Ok((n >= MIN_STR_CASES).then_some(Plan::Str(strs)))
    }

    /// Jump by a table on `d` (`SwitchI` or `SwitchA`). The table goes to `Code::tables`; its targets are labels until `finish`.
    fn switch_table(&mut self, d: u32, plan: Plan, labels: &[Lb], default: Lb) {
        if self.dead {
            return;
        }
        let at = self.tables.len();
        let mut targets: Vec<Lb> = vec![default];
        let op = match plan {
            Plan::Int(keys) => {
                let lo = keys.iter().map(|k| k.0).min().expect("a table has a case");
                let len = keys.iter().map(|k| (k.0 as i64 - lo as i64) as usize).max().expect("a table has a case") + 1;
                let mut by_value = vec![default; len];
                for (v, i) in keys {
                    by_value[(v as i64 - lo as i64) as usize] = labels[i];
                }
                self.tables.extend([lo as u32, 0, len as u32]);
                self.table_fix.push((at + 1, default));
                for l in by_value {
                    self.table_fix.push((self.tables.len(), l));
                    self.tables.push(0);
                    targets.push(l);
                }
                Op::SwitchI
            }
            Plan::Str(keys) => {
                // A power of two with at least 4/3 slots for each case (a load of at most 3/4): a probe ends at an empty slot.
                let slots = (keys.len() * 4).div_ceil(3).next_power_of_two();
                let mut place: Vec<Option<[u32; 3]>> = vec![None; slots];
                for (a, i) in keys {
                    let h = self.atom_tab[a as usize].cached_hash();
                    let mut p = h as usize & (slots - 1);
                    while place[p].is_some() {
                        p = (p + 1) & (slots - 1);
                    }
                    place[p] = Some([h, a, i as u32]);
                }
                self.tables.extend([0, slots as u32 - 1]);
                self.table_fix.push((at, default));
                for slot in place {
                    match slot {
                        Some([h, a, i]) => {
                            self.table_fix.push((self.tables.len() + 2, labels[i as usize]));
                            self.tables.extend([h, a, 0]);
                            targets.push(labels[i as usize]);
                        }
                        None => self.tables.extend([0, STR_EMPTY, 0]),
                    }
                }
                Op::SwitchA
            }
        };
        self.op(op, d, at as u32, 0);
        targets.sort_by_key(|l| l.0);
        targets.dedup();
        for l in targets {
            self.reach(l);
        }
    }

    /// Jump to `to` when `d === test`.
    fn case_jump(&mut self, d: u32, test: ExprId, to: Lb) -> CR {
        let m = self.mark();
        match self.konst(test)? {
            Some(K::Int(v)) => self.jmp_rx(Op::JSEqI, d, v as u32, to),
            Some(K::Str(a)) => self.jmp_rx(Op::JSEqA, d, a, to),
            Some(K::Null) => self.jmp_r(Op::JSEqNull, d, to),
            Some(K::Undef) => self.jmp_r(Op::JSEqUndef, d, to),
            Some(K::Bool(b)) => self.jmp_rx(Op::JSEqB, d, b as u32, to),
            None => {
                let t = self.any(test)?;
                self.jmp_rr(Op::JSEq, d, t, to);
            }
        }
        self.free(m);
        Ok(())
    }

    // ------------------------------------------------------------ try

    fn try_stmt(&mut self, block: StmtId, handler: Option<Id<Catch>>, fin: Option<StmtId>) -> CR {
        let prog = self.prog;
        let m = self.mark();
        let after = self.label();
        let depth0 = self.depth;
        // The register that gets the exception. It is below every temporary of the `try`.
        let ex = self.alloc();
        let live_before = self.live.clone();
        let labels = std::mem::take(&mut self.pending);
        let (catch_lb, fin_lb) = (handler.map(|_| self.label()), fin.map(|_| self.label()));
        for l in [catch_lb, fin_lb].into_iter().flatten() {
            self.mark_used(l);
        }
        // The block.
        self.push_frame(Vec::new(), FrameKind::Try { fin });
        self.guard_top();
        self.stmt(block)?;
        let mut a_pieces = self.pop_try();
        if let Some(f) = fin {
            self.stmt(f)?;
        }
        self.jmp(after);
        let mut fin_pieces = a_pieces.clone();
        // The `catch` clause.
        if let (Some(c), Some(lb)) = (handler, catch_lb) {
            let c = *prog.catch(c);
            self.bind(lb);
            self.live = live_before.clone();
            self.set_live(ex);
            if fin.is_some() {
                self.push_frame(Vec::new(), FrameKind::Try { fin });
                self.guard_top();
            }
            let mut ds = Vec::new();
            if let Some(p) = c.param {
                let mut names = Vec::new();
                self.pat_names(p, &mut names);
                ds.extend(names.into_iter().map(|(name, _)| Decl { name, kind: VK::Catch, end: 0, reg: None }));
            }
            let sc = self.enter(c.span, &ds, false)?;
            if let Some(p) = c.param {
                self.bind_pattern(p, ex, Bind::Init)?;
            }
            self.stmt(c.body)?;
            self.leave(sc);
            if fin.is_some() {
                fin_pieces.extend(self.pop_try());
            }
            if let Some(f) = fin {
                self.stmt(f)?;
            }
            self.jmp(after);
            for (s, e) in a_pieces.drain(..) {
                self.handlers.push(HandlerRec { start: s, end: e, target: lb, reg: ex, depth: depth0 });
            }
        }
        // The `finally` clause for an exception.
        if let (Some(f), Some(lb)) = (fin, fin_lb) {
            self.bind(lb);
            self.live = live_before.clone();
            self.set_live(ex);
            self.stmt(f)?;
            self.op(Op::Throw, ex, 0, 0);
            for (s, e) in fin_pieces {
                self.handlers.push(HandlerRef(s, e, lb, ex, depth0).into());
            }
        }
        self.pending = labels;
        self.bind(after);
        self.live = live_before;
        self.free(m);
        Ok(())
    }

    /// Pop the frame on top (a `try`, a loop) and give the ranges that its handler covers.
    fn pop_try(&mut self) -> Vec<(usize, usize)> {
        let now = self.asm.len();
        match self.frames.pop() {
            Some(mut f) => {
                if let Some(o) = f.open {
                    f.pieces.push((o, now));
                }
                f.pieces
            }
            None => Vec::new(),
        }
    }
}

struct HandlerRef(usize, usize, Lb, u32, u32);

impl From<HandlerRef> for HandlerRec {
    fn from(h: HandlerRef) -> HandlerRec {
        HandlerRec { start: h.0, end: h.1, target: h.2, reg: h.3, depth: h.4 }
    }
}
