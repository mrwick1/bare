//! Statements of the code generator (`bc_gen.rs`), in the order of the statement machine of the tree walker
//! (`eval_statements.rs`): the same environments, pushed and popped at the same points.
use super::*;

impl Gen<'_, '_> {
    pub(super) fn stmt(&mut self, s: StmtId) -> G<()> {
        let st = *self.prog.stmt(s);
        let saved = std::mem::replace(&mut self.pos, st.span.start);
        let r = self.stmt_inner(s, st);
        self.pos = saved;
        r
    }

    fn stmt_inner(&mut self, s: StmtId, st: Stmt) -> G<()> {
        let prog = self.prog.clone();
        if let Some(c) = self.compl {
            match st.kind {
                StmtKind::Expr(e) | StmtKind::Directive(e) => {
                    let r = self.reg()?;
                    self.expr(e, r)?;
                    self.emit(Op::Mov, c, r, 0)?;
                    self.free(r);
                    return Ok(());
                }
                StmtKind::If { .. } | StmtKind::While { .. } | StmtKind::DoWhile { .. } | StmtKind::For { .. } | StmtKind::ForIn { .. } | StmtKind::ForOf { .. } | StmtKind::Try { .. } | StmtKind::Switch { .. } => self.emit(Op::LoadUndef, c, 0, 0)?,
                _ => {}
            }
        }
        match st.kind {
            StmtKind::Expr(e) => self.expr_void(e),
            StmtKind::Directive(_) | StmtKind::Empty => Ok(()),
            StmtKind::Var { kind, decls } => self.var(kind, decls),
            // The closure was made when its scope started (`hoist`). In sloppy code, a declaration in a block also writes the
            // binding of the function scope here (Annex B, `annex_b`): not in this step.
            StmtKind::Function(f) if self.depth > 0 && !self.strict && prog.func(f).is_plain_ctor() => Err("a function declaration in a block (Annex B)"),
            StmtKind::Function(_) => Ok(()),
            StmtKind::Return(None) if self.fins.is_empty() => self.emit(Op::RetUndef, 0, 0, 0),
            StmtKind::Return(Some(e)) if self.rf.is_some() => self.return_expr(e),
            StmtKind::Return(e) => {
                let r = self.reg()?;
                match e {
                    Some(e) => self.expr(e, r)?,
                    None => self.emit(Op::LoadUndef, r, 0, 0)?,
                }
                self.exit(Exit::Return, r)?;
                self.free(r);
                Ok(())
            }
            StmtKind::If { test, cons, alt } => {
                let j = self.jump_if_false(test)?;
                self.stmt(cons)?;
                if let Some(a) = alt {
                    let end = self.jump(Op::Jmp, 0)?;
                    self.patch_all(j)?;
                    self.stmt(a)?;
                    self.patch(end)
                } else {
                    self.patch_all(j)
                }
            }
            StmtKind::While { test, body } => {
                let top = self.here();
                let j = self.jump_if_false(test)?;
                self.open(true, Some(top));
                self.stmt(body)?;
                self.jump_back(Op::Jmp, 0, top)?;
                self.patch_all(j)?;
                self.close()
            }
            StmtKind::DoWhile { body, test } => {
                let top = self.here();
                self.open(true, None);
                self.stmt(body)?;
                self.land_conts()?;
                let mut back = Vec::new();
                self.jump_cond(test, true, &mut back)?;
                for j in back {
                    self.patch_to(j, top)?;
                }
                self.close()
            }
            StmtKind::For { init, test, update, body } => {
                let scope = match init {
                    ForInit::Var(v) => self.scopes.get(&v).copied(),
                    _ => None,
                };
                // A register function keeps the loop variables in registers: no environment, no copy per iteration.
                let rf_mark = self.top;
                let envs = scope.is_some_and(|sc| self.scope_has_env(sc));
                if let Some(sc) = scope {
                    if envs {
                        self.push_env(sc)?;
                        if self.rf.is_some() {
                            self.rf_env_scope(sc)?;
                        }
                    } else {
                        self.rf_scope(sc)?;
                    }
                }
                match init {
                    ForInit::None => {}
                    ForInit::Expr(e) => self.expr_void(e)?,
                    ForInit::Var(v) => self.stmt(v)?,
                }
                // `for_head` and `for_update`: a fresh copy of the loop environment before the test and before the update.
                let top = self.here();
                if envs {
                    self.emit(Op::CopyEnv, 0, 0, 0)?;
                }
                let j = match test {
                    Some(t) => self.jump_if_false(t)?,
                    None => Vec::new(),
                };
                self.open(true, None);
                self.stmt(body)?;
                self.land_conts()?;
                if envs {
                    self.emit(Op::CopyEnv, 0, 0, 0)?;
                }
                if let Some(u) = update {
                    self.expr_void(u)?;
                }
                self.jump_back(Op::Jmp, 0, top)?;
                self.patch_all(j)?;
                self.close()?;
                if envs {
                    self.pop_env()?;
                }
                if self.rf.is_some() {
                    self.free(rf_mark);
                }
                Ok(())
            }
            StmtKind::Break(None) => {
                let i = self.jumps.iter().rposition(|j| !j.label_only).ok_or("break outside a loop")?;
                self.exit(Exit::Break(i), 0)
            }
            StmtKind::Continue(None) => {
                let i = self.jumps.iter().rposition(|j| j.is_loop).ok_or("continue outside a loop")?;
                self.exit(Exit::Continue(i), 0)
            }
            StmtKind::Break(Some(l)) => {
                let n = self.it.name_at(l);
                let i = self.jumps.iter().rposition(|j| j.labels.contains(&n)).ok_or("break to an unknown label")?;
                self.exit(Exit::Break(i), 0)
            }
            StmtKind::Continue(Some(l)) => {
                let n = self.it.name_at(l);
                let i = self.jumps.iter().rposition(|j| j.is_loop && j.labels.contains(&n)).ok_or("continue to an unknown label")?;
                self.exit(Exit::Continue(i), 0)
            }
            StmtKind::Labeled { label, body } => {
                let n = self.it.name_at(label);
                self.pending.push(n);
                match self.prog.stmt(body).kind {
                    // The `open` of the loop or `switch` takes the labels.
                    StmtKind::Labeled { .. } | StmtKind::For { .. } | StmtKind::ForIn { .. } | StmtKind::ForOf { .. } | StmtKind::While { .. } | StmtKind::DoWhile { .. } | StmtKind::Switch { .. } => self.stmt(body),
                    _ => {
                        let labels = std::mem::take(&mut self.pending);
                        self.jumps.push(Jumps { is_loop: false, depth: self.depth, fins: self.fins.len(), breaks: Vec::new(), cont: None, conts: Vec::new(), labels, label_only: true, iters: self.iters });
                        self.stmt(body)?;
                        self.close()
                    }
                }
            }
            StmtKind::Throw(e) => {
                let r = self.reg()?;
                self.expr(e, r)?;
                self.emit(Op::Throw, r, 0, 0)?;
                self.free(r);
                Ok(())
            }
            StmtKind::Try { block, handler, finalizer } => self.try_stmt(s, block, handler, finalizer),
            StmtKind::Switch { disc, cases } => self.switch(s, disc, cases),
            StmtKind::Block(l) => {
                let sc = self.scopes.get(&s).copied();
                let mark = self.top;
                let env = sc.is_some_and(|sc| self.scope_has_env(sc));
                match sc {
                    Some(sc) if !env => self.rf_scope(sc)?,
                    Some(sc) => {
                        self.push_env(sc)?;
                        if self.rf.is_some() {
                            self.rf_env_scope(sc)?;
                        }
                        self.hoist(&[l], sc)?;
                    }
                    None => {}
                }
                for x in prog.list(l) {
                    self.stmt(x)?;
                }
                if env {
                    self.pop_env()?;
                }
                if self.rf.is_some() {
                    self.free(mark);
                }
                Ok(())
            }
            StmtKind::Class(_) => Err("class"),
            StmtKind::ForIn { left, right, body } => self.for_each(left, right, body, false),
            StmtKind::ForOf { is_await: true, .. } => Err("async"),
            StmtKind::ForOf { left, right, body, .. } => self.for_each(left, right, body, true),
            StmtKind::With { .. } => Err("with"),
            StmtKind::ExportDefault(..) => Err("module code"),
            StmtKind::Debugger => Err("debugger"),
        }
    }

    fn push_env(&mut self, scope: u32) -> G<()> {
        self.emit(Op::PushEnv, 0, scope, 0)?;
        self.depth += 1;
        Ok(())
    }

    fn pop_env(&mut self) -> G<()> {
        self.emit(Op::PopEnv, 0, 0, 0)?;
        self.depth -= 1;
        Ok(())
    }

    /// `var`, `let` and `const` (`exec_simple`). A `var` assigns through its site, a `let` or `const` initializes the binding of
    /// the current environment.
    fn var(&mut self, kind: VarKind, decls: List<Declarator>) -> G<()> {
        let prog = self.prog.clone();
        for d in prog.list(decls) {
            let d = *prog.decl(d);
            if !matches!(prog.pat(d.target).kind, PatKind::Ident) {
                let r = self.reg()?;
                self.expr(d.init.ok_or("a pattern with no initializer")?, r)?;
                self.bind_pat(true, kind, d.target, r)?;
                self.free(r);
                continue;
            }
            let n = self.it.cc.cold.pat_name[d.target.index()];
            let reg = if self.rf.is_some() { self.rf_decl_opt(self.it.pat_site(d.target.index()), n)? } else { None };
            if let Some((key, l)) = reg {
                // The variable is a register. A `let` or `const` is readable after its initializer.
                match (kind, d.init) {
                    (VarKind::Var, None) => {}
                    (_, Some(i)) => self.expr_into(i, l.reg)?,
                    (_, None) => self.emit(Op::LoadUndef, l.reg, 0, 0)?,
                }
                if kind != VarKind::Var {
                    self.rf_set_init(key);
                }
                continue;
            }
            let site = self.site(self.it.pat_site(d.target.index()), n)?;
            let k = Some(n);
            let r = self.reg()?;
            match (kind, d.init) {
                (VarKind::Var, None) => {}
                (VarKind::Var, Some(i)) => {
                    self.named(i, r, k)?;
                    self.store(site, n, r)?;
                }
                (_, init) => {
                    match init {
                        Some(i) => self.named(i, r, k)?,
                        None => self.emit(Op::LoadUndef, r, 0, 0)?,
                    }
                    let Site::Env { hops: 0, index } = site else { return Err("a let or const outside its environment") };
                    self.emit(Op::InitEnv, r, 0, index)?;
                }
            }
            self.free(r);
        }
        Ok(())
    }

    /// `switch` (`enter_rare`): the value, then the environment and its closures, then the tests in order.
    fn switch(&mut self, s: StmtId, disc: ExprId, cases: List<Case>) -> G<()> {
        let prog = self.prog.clone();
        let d = self.reg()?;
        self.expr(disc, d)?;
        let sc = self.scopes.get(&s).copied();
        if sc.is_some() && self.rf.is_some() {
            return Err("a switch with let or const in a register function");
        }
        if let Some(sc) = sc {
            self.push_env(sc)?;
            let lists: Vec<List<Stmt>> = prog.list(cases).map(|c| prog.case(c).body).collect();
            self.hoist(&lists, sc)?;
        }
        let t = self.reg()?;
        let mut to = Vec::new();
        for (i, c) in prog.list(cases).enumerate() {
            if let Some(test) = prog.case(c).test {
                let j = match self.num_konst(test) {
                    Some(k) => self.jump_cmp(Op::JnSNeK, d, k)?,
                    None => {
                        self.expr(test, t)?;
                        self.jump_cmp(Op::JnSNe, t, d)?
                    }
                };
                to.push((i, j));
            }
        }
        self.free(t);
        let to_default = self.jump(Op::Jmp, 0)?;
        self.open(false, None);
        let mut default = None;
        for (i, c) in prog.list(cases).enumerate() {
            let c = *prog.case(c);
            for &(_, j) in to.iter().filter(|x| x.0 == i) {
                self.patch(j)?;
            }
            if c.test.is_none() {
                default = Some(self.here());
            }
            for x in prog.list(c.body) {
                self.stmt(x)?;
            }
        }
        match default {
            Some(p) => self.patch_to(to_default, p)?,
            None => self.jumps.last_mut().expect("open").breaks.push(to_default),
        }
        self.close()?;
        if sc.is_some() {
            self.pop_env()?;
        }
        self.free(d);
        Ok(())
    }

    /// `try` (`try_step`): the handler of a `catch` covers the block, the handler of a `finally` covers the block and the `catch`.
    fn try_stmt(&mut self, s: StmtId, block: StmtId, handler: Option<Id<Catch>>, finalizer: Option<StmtId>) -> G<()> {
        let prog = self.prog.clone();
        let depth = self.depth;
        let hdepth = u16::try_from(depth).map_err(|_| "too deep")?;
        let mark = self.top;
        let iters = u16::try_from(self.iters).map_err(|_| "too deep")?;
        if finalizer.is_some() {
            let (kind, val) = (self.reg()?, self.reg()?);
            self.fins.push(Fin { kind, val, depth, iters: self.iters, entry: Vec::new(), exits: Vec::new() });
        }
        let start = self.here() as u32;
        self.stmt(block)?;
        let mut ends = Vec::new();
        if let Some(h) = handler {
            ends.push(self.jump(Op::Jmp, 0)?);
            let c = *prog.catch(h);
            let exc = self.reg()?;
            self.code.handlers.push(Handler { start, end: self.here() as u32, target: self.here() as u32, reg: exc as u16, depth: hdepth, iters });
            let sc = *self.scopes.get(&s).ok_or("no catch scope")?;
            if self.rf.is_some() {
                if self.scope_has_env(sc) {
                    return Err("a catch with an inner function in a register function");
                }
                self.rf_scope(sc)?;
                if let Some(p) = c.param.filter(|&p| !matches!(prog.pat(p).kind, PatKind::Ident)) {
                    self.bind_pat(true, VarKind::Let, p, exc)?;
                } else if let Some(p) = c.param {
                    let n = self.it.cc.cold.pat_name[p.index()];
                    let (key, l) = self.rf_decl(self.it.pat_site(p.index()), n)?;
                    self.emit(Op::Mov, l.reg, exc, 0)?;
                    self.rf_set_init(key);
                }
                self.stmt(c.body)?;
                self.free(exc);
            } else {
                self.push_env(sc)?;
                if let Some(p) = c.param.filter(|&p| !matches!(prog.pat(p).kind, PatKind::Ident)) {
                    self.bind_pat(true, VarKind::Let, p, exc)?;
                } else if let Some(p) = c.param {
                    let n = self.it.cc.cold.pat_name[p.index()];
                    let Site::Env { hops: 0, index } = self.site(self.it.pat_site(p.index()), n)? else { return Err("a catch parameter outside its environment") };
                    self.emit(Op::InitEnv, exc, 0, index)?;
                }
                self.free(exc);
                self.stmt(c.body)?;
                self.pop_env()?;
            }
        }
        let tail = self.here() as u32;
        for j in ends {
            self.patch(j)?;
        }
        if let Some(f) = finalizer {
            let fin = self.fins.pop().expect("pushed above");
            self.emit(Op::LoadI, fin.kind, 0, 0)?;
            let skip = self.jump(Op::Jmp, 0)?;
            // An empty `try` block with no `catch` cannot throw: no handler.
            if start < tail {
                self.code.handlers.push(Handler { start, end: tail, target: self.here() as u32, reg: fin.val as u16, depth: hdepth, iters });
            }
            self.emit(Op::LoadI, fin.kind, 1, 0)?;
            self.patch(skip)?;
            for j in fin.entry {
                self.patch(j)?;
            }
            self.stmt(f)?;
            let t = self.reg()?;
            for (i, &ex) in fin.exits.iter().enumerate() {
                self.emit(Op::LoadI, t, 2 + i as u32, 0)?;
                let j = self.jump_cmp(Op::JnSEq, t, fin.kind)?;
                self.exit(ex, fin.val)?;
                self.patch(j)?;
            }
            self.emit(Op::LoadI, t, 1, 0)?;
            let j = self.jump_cmp(Op::JnSEq, t, fin.kind)?;
            self.emit(Op::Throw, fin.val, 0, 0)?;
            self.patch(j)?;
        }
        self.free(mark);
        Ok(())
    }

    /// Leave statements for `ex`. A `finally` on the way runs first: the jump goes to it, and it continues the exit at its end.
    /// `val` holds the return value of a `return`.
    pub(super) fn exit(&mut self, ex: Exit, val: u32) -> G<()> {
        let (fins, depth, iters) = match ex {
            Exit::Return => (0, 0, 0),
            Exit::Break(i) | Exit::Continue(i) => (self.jumps[i].fins, self.jumps[i].depth, self.jumps[i].iters),
        };
        // The iterators close before the environments pop: a `return` method that throws finds the depth of its handler.
        if self.fins.len() > fins {
            let f = self.fins.len() - 1;
            for _ in self.fins[f].iters..self.iters {
                self.emit(Op::IterClose, 0, 0, 0)?;
            }
            for _ in self.fins[f].depth..self.depth {
                self.emit(Op::PopEnv, 0, 0, 0)?;
            }
            let id = match self.fins[f].exits.iter().position(|&x| x == ex) {
                Some(id) => id,
                None => {
                    self.fins[f].exits.push(ex);
                    self.fins[f].exits.len() - 1
                }
            };
            let (kind, fval) = (self.fins[f].kind, self.fins[f].val);
            if ex == Exit::Return {
                self.emit(Op::Mov, fval, val, 0)?;
            }
            self.emit(Op::LoadI, kind, 2 + id as u32, 0)?;
            let j = self.jump(Op::Jmp, 0)?;
            self.fins[f].entry.push(j);
            return Ok(());
        }
        for _ in iters..self.iters {
            self.emit(Op::IterClose, 0, 0, 0)?;
        }
        if ex == Exit::Return {
            return self.emit(Op::Ret, val, 0, 0);
        }
        for _ in depth..self.depth {
            self.emit(Op::PopEnv, 0, 0, 0)?;
        }
        match ex {
            Exit::Break(i) => {
                let j = self.jump(Op::Jmp, 0)?;
                self.jumps[i].breaks.push(j);
            }
            Exit::Continue(i) => match self.jumps[i].cont {
                Some(t) => self.jump_back(Op::Jmp, 0, t)?,
                None => {
                    let j = self.jump(Op::Jmp, 0)?;
                    self.jumps[i].conts.push(j);
                }
            },
            Exit::Return => {}
        }
        Ok(())
    }

    /// `for-in` and `for-of` (`exec_for_each`): the value, the iterator, then for each value a new binding (`CopyEnv` for a `let`
    /// or `const` that a closure captures) and the body. A `break` to the loop lands on its `IterClose`; the end of the iteration
    /// pops the iterator with no close.
    fn for_each(&mut self, left: ForLeft, right: ExprId, body: StmtId, of: bool) -> G<()> {
        let prog = self.prog.clone();
        let (var, kind, target) = match left {
            ForLeft::Pat(p) => (None, VarKind::Var, p),
            ForLeft::Var(v) => {
                let StmtKind::Var { kind, decls } = prog.stmt(v).kind else { return Err("for left") };
                let d = prog.list(decls).next().map(|d| *prog.decl(d)).ok_or("for left")?;
                if d.init.is_some() {
                    return Err("a for-in initializer");
                }
                (Some(v), kind, d.target)
            }
        };
        let r = self.reg()?;
        self.expr(right, r)?;
        if of {
            self.code.calls.push((self.here() as u32, right.index() as u32));
            self.emit(Op::IterOpen, r, 0, 0)?;
        } else {
            self.emit(Op::ForInOpen, r, 0, 0)?;
        }
        self.free(r);
        self.iters += 1;
        let scope = var.and_then(|v| self.scopes.get(&v).copied());
        let rf_mark = self.top;
        let envs = scope.is_some_and(|sc| self.scope_has_env(sc));
        if let Some(sc) = scope {
            if envs {
                self.push_env(sc)?;
                if self.rf.is_some() {
                    self.rf_env_scope(sc)?;
                }
            } else {
                self.rf_scope(sc)?;
            }
        }
        let v = self.reg()?;
        let top = self.here();
        let done = self.jump(Op::IterNext, v)?;
        if envs {
            self.emit(Op::CopyEnv, 0, 0, 0)?;
        }
        self.bind_pat(var.is_some(), kind, target, v)?;
        self.open(true, Some(top));
        self.stmt(body)?;
        self.jump_back(Op::Jmp, 0, top)?;
        self.close()?;
        self.emit(Op::IterClose, 0, 0, 0)?;
        self.patch(done)?;
        self.iters -= 1;
        if envs {
            self.pop_env()?;
        }
        self.free(if self.rf.is_some() { rf_mark } else { v });
        Ok(())
    }

    /// Bind the value in `v` to the pattern `p` (`bind_pattern`): a declaration (`decl`, of `kind`) or an assignment target. `v`
    /// is not written. The order of reads, defaults and writes is the one of the tree walker.
    pub(super) fn bind_pat(&mut self, decl: bool, kind: VarKind, p: PatId, v: u32) -> G<()> {
        let prog = self.prog.clone();
        let mark = self.top;
        match prog.pat(p).kind {
            PatKind::Ident => {
                let n = self.it.cc.cold.pat_name[p.index()];
                let site = self.it.pat_site(p.index());
                let reg = if decl && self.rf.is_some() { self.rf_decl_opt(site, n)? } else { None };
                if let Some((key, l)) = reg {
                    self.emit(Op::Mov, l.reg, v, 0)?;
                    if kind != VarKind::Var {
                        self.rf_set_init(key);
                    }
                } else if decl && kind != VarKind::Var {
                    let Site::Env { hops: 0, index } = self.site(site, n)? else { return Err("a let or const outside its environment") };
                    self.emit(Op::InitEnv, v, 0, index)?;
                } else {
                    let s = if decl { self.site(site, n)? } else { self.site_w(site, n)? };
                    self.store(s, n, v)?;
                }
            }
            PatKind::Member(me) if !decl => self.put_member(me, v)?,
            PatKind::Assign { target, default } => {
                // `undefined` takes the default (an anonymous function takes the name of a name target).
                let x = self.reg()?;
                let t = self.reg()?;
                self.emit(Op::Mov, x, v, 0)?;
                self.emit(Op::LoadUndef, t, 0, 0)?;
                let j = self.jump_cmp(Op::JnSEq, t, x)?;
                let name = matches!(prog.pat(target).kind, PatKind::Ident).then(|| self.it.cc.cold.pat_name[target.index()]);
                self.named(default, x, name)?;
                self.patch(j)?;
                self.bind_pat(decl, kind, target, x)?;
            }
            PatKind::Object(l) => {
                self.emit(Op::RequireObj, v, 0, 0)?;
                let rest = prog.list(l).any(|q| matches!(prog.pat(prog.pat_prop(q).value).kind, PatKind::Rest(_)));
                // With a rest property, each key stays in a register after the one of the rest object, for `ObjRest`.
                let o = if rest { self.reg()? } else { 0 };
                let mut nkeys = 0;
                for q in prog.list(l) {
                    let pp = *prog.pat_prop(q);
                    if let PatKind::Rest(t) = prog.pat(pp.value).kind {
                        self.emit(Op::ObjRest, o, v, nkeys)?;
                        self.bind_pat(decl, kind, t, o)?;
                        continue;
                    }
                    if let (Key::Name(sp), false) = (pp.key, rest) {
                        let x = self.reg()?;
                        self.field(Op::GetField, x, v, sp)?;
                        self.bind_pat(decl, kind, pp.value, x)?;
                        self.free(x);
                        continue;
                    }
                    // The key (`coerce_key`), then the value, then the binding.
                    let k = self.reg()?;
                    if let Key::Computed(e) = pp.key {
                        self.expr(e, k)?;
                        self.emit(Op::ToKey, k, v, k)?;
                    } else {
                        let s = match pp.key {
                            Key::Name(sp) => prog.name(sp).into_owned(),
                            Key::Str(sp) => {
                                let mut u = Vec::new();
                                prog.string(sp, &mut u).map_err(|_| "string")?;
                                String::from_utf16(&u).map_err(|_| "a key with a lone surrogate")?
                            }
                            Key::Num(sp) => number_to_string(self.it.num_lit(sp, num_base(prog.text(sp)))),
                            _ => return Err("pattern key"),
                        };
                        let c = self.konst(KeyOf::String(s.encode_utf16().collect()));
                        self.emit(Op::LoadK, k, c, 0)?;
                    }
                    let x = self.reg()?;
                    self.emit(Op::GetElem, x, v, k)?;
                    self.bind_pat(decl, kind, pp.value, x)?;
                    if rest {
                        self.free(x);
                        nkeys += 1;
                    } else {
                        self.free(k);
                    }
                }
            }
            PatKind::Array(l) => {
                self.emit(Op::IterOpen, v, 1, 0)?;
                self.iters += 1;
                let x = self.reg()?;
                for q in prog.list(l) {
                    match prog.pat(q).kind {
                        PatKind::Hole => self.emit(Op::IterStep, x, 0, 0)?,
                        PatKind::Rest(t) => {
                            self.emit(Op::IterRest, x, 0, 0)?;
                            self.bind_pat(decl, kind, t, x)?;
                        }
                        _ => {
                            self.emit(Op::IterStep, x, 0, 0)?;
                            self.bind_pat(decl, kind, q, x)?;
                        }
                    }
                }
                self.emit(Op::IterClose, 0, 0, 0)?;
                self.iters -= 1;
            }
            _ => return Err("destructuring"),
        }
        self.free(mark);
        Ok(())
    }

    fn open(&mut self, is_loop: bool, cont: Option<usize>) {
        let labels = std::mem::take(&mut self.pending);
        self.jumps.push(Jumps { is_loop, depth: self.depth, fins: self.fins.len(), breaks: Vec::new(), cont, conts: Vec::new(), labels, label_only: false, iters: self.iters });
    }

    /// The `continue` target of the open loop is here.
    fn land_conts(&mut self) -> G<()> {
        let j = self.jumps.last_mut().expect("open");
        j.cont = Some(self.code.ops.len());
        for at in std::mem::take(&mut j.conts) {
            self.patch(at)?;
        }
        Ok(())
    }

    /// The `break` target of the open loop or `switch` is here.
    fn close(&mut self) -> G<()> {
        let j = self.jumps.pop().expect("open");
        for at in j.breaks {
            self.patch(at)?;
        }
        Ok(())
    }
}
