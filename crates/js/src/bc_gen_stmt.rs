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
        match st.kind {
            StmtKind::Expr(e) => {
                let r = self.reg()?;
                self.expr(e, r)?;
                self.free(r);
                Ok(())
            }
            StmtKind::Directive(_) | StmtKind::Empty => Ok(()),
            StmtKind::Var { kind, decls } => self.var(kind, decls),
            // The closure was made when its scope started (`hoist`). In sloppy code, a declaration in a block also writes the
            // binding of the function scope here (Annex B, `annex_b`): not in this step.
            StmtKind::Function(f) if self.depth > 0 && !self.strict && prog.func(f).is_plain_ctor() => Err("a function declaration in a block (Annex B)"),
            StmtKind::Function(_) => Ok(()),
            StmtKind::Return(None) if self.fins.is_empty() => self.emit(Op::RetUndef, 0, 0, 0),
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
                let r = self.reg()?;
                self.expr(test, r)?;
                let j = self.jump(Op::JmpIfNot, r)?;
                self.free(r);
                self.stmt(cons)?;
                if let Some(a) = alt {
                    let end = self.jump(Op::Jmp, 0)?;
                    self.patch(j)?;
                    self.stmt(a)?;
                    self.patch(end)
                } else {
                    self.patch(j)
                }
            }
            StmtKind::While { test, body } => {
                let top = self.here();
                let r = self.reg()?;
                self.expr(test, r)?;
                let j = self.jump(Op::JmpIfNot, r)?;
                self.free(r);
                self.open(true, Some(top));
                self.stmt(body)?;
                self.jump_back(Op::Jmp, 0, top)?;
                self.patch(j)?;
                self.close()
            }
            StmtKind::DoWhile { body, test } => {
                let top = self.here();
                self.open(true, None);
                self.stmt(body)?;
                self.land_conts()?;
                let r = self.reg()?;
                self.expr(test, r)?;
                self.jump_back(Op::JmpIf, r, top)?;
                self.free(r);
                self.close()
            }
            StmtKind::For { init, test, update, body } => {
                let scope = match init {
                    ForInit::Var(v) => self.scopes.get(&v).copied(),
                    _ => None,
                };
                if let Some(sc) = scope {
                    self.push_env(sc)?;
                }
                match init {
                    ForInit::None => {}
                    ForInit::Expr(e) => {
                        let r = self.reg()?;
                        self.expr(e, r)?;
                        self.free(r);
                    }
                    ForInit::Var(v) => self.stmt(v)?,
                }
                // `for_head` and `for_update`: a fresh copy of the loop environment before the test and before the update.
                let top = self.here();
                if scope.is_some() {
                    self.emit(Op::CopyEnv, 0, 0, 0)?;
                }
                let j = match test {
                    Some(t) => {
                        let r = self.reg()?;
                        self.expr(t, r)?;
                        let j = self.jump(Op::JmpIfNot, r)?;
                        self.free(r);
                        Some(j)
                    }
                    None => None,
                };
                self.open(true, None);
                self.stmt(body)?;
                self.land_conts()?;
                if scope.is_some() {
                    self.emit(Op::CopyEnv, 0, 0, 0)?;
                }
                if let Some(u) = update {
                    let r = self.reg()?;
                    self.expr(u, r)?;
                    self.free(r);
                }
                self.jump_back(Op::Jmp, 0, top)?;
                if let Some(j) = j {
                    self.patch(j)?;
                }
                self.close()?;
                if scope.is_some() {
                    self.pop_env()?;
                }
                Ok(())
            }
            StmtKind::Break(None) => {
                let i = self.jumps.len().checked_sub(1).ok_or("break outside a loop")?;
                self.exit(Exit::Break(i), 0)
            }
            StmtKind::Continue(None) => {
                let i = self.jumps.iter().rposition(|j| j.is_loop).ok_or("continue outside a loop")?;
                self.exit(Exit::Continue(i), 0)
            }
            StmtKind::Break(Some(_)) | StmtKind::Continue(Some(_)) | StmtKind::Labeled { .. } => Err("label"),
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
                if let Some(sc) = sc {
                    self.push_env(sc)?;
                    self.hoist(&[l], sc)?;
                }
                for x in prog.list(l) {
                    self.stmt(x)?;
                }
                if sc.is_some() {
                    self.pop_env()?;
                }
                Ok(())
            }
            StmtKind::Class(_) => Err("class"),
            StmtKind::ForIn { .. } | StmtKind::ForOf { .. } => Err("for-in or for-of"),
            StmtKind::With { .. } => Err("with"),
            StmtKind::ExportDefault(_) => Err("module code"),
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
                return Err("destructuring");
            }
            let n = self.it.pat_name[d.target.index()];
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
        if let Some(sc) = sc {
            self.push_env(sc)?;
            let lists: Vec<List<Stmt>> = prog.list(cases).map(|c| prog.case(c).body).collect();
            self.hoist(&lists, sc)?;
        }
        let t = self.reg()?;
        let mut to = Vec::new();
        for (i, c) in prog.list(cases).enumerate() {
            if let Some(test) = prog.case(c).test {
                self.expr(test, t)?;
                self.emit(Op::StrictEq, t, d, t)?;
                to.push((i, self.jump(Op::JmpIf, t)?));
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
        if finalizer.is_some() {
            let (kind, val) = (self.reg()?, self.reg()?);
            self.fins.push(Fin { kind, val, depth, entry: Vec::new(), exits: Vec::new() });
        }
        let start = self.here() as u32;
        self.stmt(block)?;
        let mut ends = Vec::new();
        if let Some(h) = handler {
            ends.push(self.jump(Op::Jmp, 0)?);
            let c = *prog.catch(h);
            let exc = self.reg()?;
            self.code.handlers.push(Handler { start, end: self.here() as u32, target: self.here() as u32, reg: exc as u16, depth: hdepth });
            let sc = *self.scopes.get(&s).ok_or("no catch scope")?;
            self.push_env(sc)?;
            if let Some(p) = c.param {
                if !matches!(prog.pat(p).kind, PatKind::Ident) {
                    return Err("destructuring");
                }
                let n = self.it.pat_name[p.index()];
                let Site::Env { hops: 0, index } = self.site(self.it.pat_site(p.index()), n)? else { return Err("a catch parameter outside its environment") };
                self.emit(Op::InitEnv, exc, 0, index)?;
            }
            self.free(exc);
            self.stmt(c.body)?;
            self.pop_env()?;
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
                self.code.handlers.push(Handler { start, end: tail, target: self.here() as u32, reg: fin.val as u16, depth: hdepth });
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
                self.emit(Op::StrictEq, t, fin.kind, t)?;
                let j = self.jump(Op::JmpIfNot, t)?;
                self.exit(ex, fin.val)?;
                self.patch(j)?;
            }
            self.emit(Op::LoadI, t, 1, 0)?;
            self.emit(Op::StrictEq, t, fin.kind, t)?;
            let j = self.jump(Op::JmpIfNot, t)?;
            self.emit(Op::Throw, fin.val, 0, 0)?;
            self.patch(j)?;
        }
        self.free(mark);
        Ok(())
    }

    /// Leave statements for `ex`. A `finally` on the way runs first: the jump goes to it, and it continues the exit at its end.
    /// `val` holds the return value of a `return`.
    fn exit(&mut self, ex: Exit, val: u32) -> G<()> {
        let (fins, depth) = match ex {
            Exit::Return => (0, 0),
            Exit::Break(i) | Exit::Continue(i) => (self.jumps[i].fins, self.jumps[i].depth),
        };
        if self.fins.len() > fins {
            let f = self.fins.len() - 1;
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

    fn open(&mut self, is_loop: bool, cont: Option<usize>) {
        self.jumps.push(Jumps { is_loop, depth: self.depth, fins: self.fins.len(), breaks: Vec::new(), cont, conts: Vec::new() });
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
