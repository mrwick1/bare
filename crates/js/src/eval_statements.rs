// Synchronous statement machine. Included by `eval.rs` (`include!`), so it shares that module's imports.
//
// Why: the evaluator used to run each compound statement (`{}`, `if`, loops, `switch`, `try`, labels) as a Rust call
// that called `exec` again for its body. Every nesting level cost a native frame and a `descend` count. A real React
// commit walk nests about a dozen statements per fiber level, so it hit `max_traversal_depth` long before the call cap.
//
// What: `machine` runs a statement, or a statement list, to completion in the current call. The control state of the
// compound statements that is in progress is an explicit list of small frames (`SFrame`), not Rust recursion.
// - Frames are ids, environment ids, a few flags and a pending completion. No values besides `ret` of a `try`.
// - The list is a local `Vec` of ONE machine run (its buffer comes from a small pool of empty buffers and goes back at the
//   end). It never outlives the run, so no job and no `collect` sees it.
//   (`collect` refuses while `depth != 0`, and `stmt_frames != 0` as well.)
// - A machine run belongs to one script: calls and callbacks that cross scripts start their own run, and
//   `select` puts the caller's script back before this run continues (checked in debug builds).
// - `if` is a tail step and has no frame. A block with no lexical declaration is a plain list frame.
//
// Limits, two different things:
// - `max_traversal_depth` counts NATIVE recursion: expressions, patterns, calls, constructors, the setup walk, and
//   `exec`. `exec` is only entered by `for-in`/`for-of` bodies (their helper is a Rust call) and by async code. The machine
//   entry for a function or script body is already inside `call_fn` (guarded) or `exec_current`.
// - `max_statement_frames` (default 512, hard ceiling 768) counts pending control frames of all synchronous calls
//   together, nested JS calls included. It is checked before each push. Heap frames are not native frames and are not
//   added to the traversal count; expression recursion is still guarded by `max_traversal_depth` only.
//
// Exits: every way out of a frame (normal, `break`, `continue`, `return`, a JS throw, a limit, an unsupported construct,
// a heap fault) passes through `leave`, which restores the environment (and strictness for `try`), frees what the frame
// created and gives back its count. A limit, unsupported construct or heap fault does not run `catch`, `finally` or
// `IteratorClose`; an ordinary throw does. `Err(Break)` meets loops, `switch` and labels as before.
//
// Not covered: `for-in` and `for-of` stay on their Rust helper (`exec_for_each`): its body runs `exec`, which is guarded.

enum SGo {
    /// Start a statement.
    Stmt(StmtId),
    /// The statement or frame on top finished with this completion.
    Done(Ex),
}

enum SAct {
    Go(SGo),
    /// Pop the top frame and give this completion to the next.
    Leave(Ex),
    /// Push a list frame on top of the current frame.
    List(List<Stmt>),
}

enum SPhase {
    Block,
    Catch,
    Finally,
}

struct STry {
    handler: Option<Id<Catch>>,
    finalizer: Option<StmtId>,
    env: u32,
    strict: bool,
    phase: SPhase,
    catch_env: Option<u32>,
    /// Completion of the block or the catch while the finalizer runs.
    pending: Option<Ex>,
    saved_ret: Value,
}

enum SFrame {
    /// A statement list from item `i`. `scope` is (outer env, own env) of a block with lexical declarations.
    List { l: List<Stmt>, i: u32, scope: Option<(u32, u32)> },
    While { test: ExprId, body: StmtId, labels: Vec<u32> },
    DoWhile { body: StmtId, test: ExprId, labels: Vec<u32> },
    For { test: Option<ExprId>, update: Option<ExprId>, body: StmtId, per_iter: bool, saved: u32, labels: Vec<u32> },
    /// `next` is the case whose body runs next.
    Switch { cases: List<Case>, next: u32, saved: u32, env: Option<u32>, labels: Vec<u32> },
    Try(STry),
    Labeled { label: u32 },
}

enum SStart {
    Stmt(StmtId),
    List(List<Stmt>),
}

impl<'p> Interp<'p> {
    /// Run one statement. Used by `for-in`/`for-of` bodies and async code; it counts as native recursion.
    fn exec(&mut self, s: StmtId) -> Ex {
        self.descend(|it| it.machine(SStart::Stmt(s)))
    }

    /// Run a function or script body. The caller is already guarded (`call_fn`, `exec_current`).
    fn exec_list(&mut self, l: List<Stmt>) -> Ex {
        self.machine(SStart::List(l))
    }

    fn machine(&mut self, start: SStart) -> Ex {
        let (env0, strict0, script0) = (self.env, self.strict, self.cur);
        let mut st = self.sf_pool.pop().unwrap_or_default();
        let mut go = match start {
            SStart::Stmt(s) => SGo::Stmt(s),
            SStart::List(l) => match self.sf_push(&mut st, SFrame::List { l, i: 0, scope: None }) {
                Ok(()) => SGo::Done(Ok(())),
                Err(e) => SGo::Done(Err(e.into())),
            },
        };
        let out = loop {
            debug_assert_eq!(self.cur, script0);
            go = match go {
                SGo::Stmt(s) => self.enter(&mut st, s).unwrap_or_else(|c| SGo::Done(Err(c))),
                SGo::Done(r) => {
                    if st.is_empty() {
                        break r;
                    }
                    self.resume(&mut st, r)
                }
            };
        };
        self.env = env0;
        self.strict = strict0;
        // Every frame was popped. Keep a few small buffers; a run owns its buffer while it executes.
        debug_assert!(st.is_empty());
        if st.capacity() <= 64 && self.sf_pool.len() < 16 {
            self.sf_pool.push(st);
        }
        out
    }

    fn sf_room(&self) -> Res<()> {
        if self.stmt_frames >= self.limits.max_statement_frames {
            return Err(Error::Limit(LimitKind::StatementFrames));
        }
        Ok(())
    }

    /// Push after the cap check. The count and the list stay equal.
    fn sf_push(&mut self, st: &mut Vec<SFrame>, f: SFrame) -> Res<()> {
        self.sf_room()?;
        self.sf_put(st, f);
        Ok(())
    }

    fn sf_put(&mut self, st: &mut Vec<SFrame>, f: SFrame) {
        self.stmt_frames += 1;
        st.push(f);
    }

    fn test_true(&mut self, e: ExprId) -> Res<bool> {
        let t = self.eval_expr(e)?;
        self.truthy(t)
    }

    /// Start a statement. A `?` after a frame was pushed is safe: the error goes to that frame, which cleans up.
    fn enter(&mut self, st: &mut Vec<SFrame>, s: StmtId) -> Result<SGo, Ctl> {
        self.tick()?;
        let prog = self.prog.clone();
        let stmt = *prog.stmt(s);
        let top = self.depth == 0;
        // Only a label directly on a loop or switch is its own. Any other statement ends the label list.
        if !matches!(stmt.kind, StmtKind::Labeled { .. } | StmtKind::While { .. } | StmtKind::DoWhile { .. } | StmtKind::For { .. } | StmtKind::ForIn { .. } | StmtKind::ForOf { .. } | StmtKind::Switch { .. }) {
            self.pending_labels.clear();
        }
        match stmt.kind {
            StmtKind::If { test, cons, alt } => {
                if top {
                    self.completion = Value::Undefined;
                }
                let t = self.eval_expr(test)?;
                Ok(if self.truthy(t)? {
                    SGo::Stmt(cons)
                } else if let Some(a) = alt {
                    SGo::Stmt(a)
                } else {
                    SGo::Done(Ok(()))
                })
            }
            StmtKind::Block(l) => {
                if !self.has_lexical(l) {
                    self.sf_push(st, SFrame::List { l, i: 0, scope: None })?;
                    return Ok(SGo::Done(Ok(())));
                }
                self.sf_room()?;
                let saved = self.env;
                let env = self.new_env(saved, false)?;
                self.env = env;
                self.sf_put(st, SFrame::List { l, i: 0, scope: Some((saved, env)) });
                self.declare_scope(l, false)?;
                Ok(SGo::Done(Ok(())))
            }
            StmtKind::While { test, body } => {
                let labels = std::mem::take(&mut self.pending_labels);
                if top {
                    self.completion = Value::Undefined;
                }
                self.sf_push(st, SFrame::While { test, body, labels })?;
                let a = self.while_head(test, body);
                Ok(self.act(st, a))
            }
            StmtKind::DoWhile { body, test } => {
                let labels = std::mem::take(&mut self.pending_labels);
                if top {
                    self.completion = Value::Undefined;
                }
                self.sf_push(st, SFrame::DoWhile { body, test, labels })?;
                self.tick()?;
                Ok(SGo::Stmt(body))
            }
            StmtKind::For { init, test, update, body } => {
                let labels = std::mem::take(&mut self.pending_labels);
                if top {
                    self.completion = Value::Undefined;
                }
                self.sf_push(st, SFrame::For { test, update, body, per_iter: false, saved: self.env, labels })?;
                let mut per_iter = false;
                match init {
                    ForInit::None => {}
                    ForInit::Expr(e) => {
                        self.eval_expr(e)?;
                    }
                    ForInit::Var(v) => {
                        let vs = *prog.stmt(v);
                        if let StmtKind::Var { kind, decls } = vs.kind
                            && kind != VarKind::Var
                        {
                            let env = self.new_env(self.env, false)?;
                            self.env = env;
                            per_iter = true;
                            for d in prog.list(decls) {
                                self.declare_pat_lexical(prog.decl(d).target, if kind == VarKind::Let { MUT } else { 0 });
                            }
                        }
                        self.tick()?;
                        self.pending_labels.clear();
                        self.exec_simple(v, &vs, false)?;
                    }
                }
                if let Some(SFrame::For { per_iter: p, .. }) = st.last_mut() {
                    *p = per_iter;
                }
                let a = self.for_head(test, per_iter, body);
                Ok(self.act(st, a))
            }
            StmtKind::ForIn { left, right, body } | StmtKind::ForOf { left, right, body, .. } => {
                let labels = std::mem::take(&mut self.pending_labels);
                if top {
                    self.completion = Value::Undefined;
                }
                let is_of = matches!(stmt.kind, StmtKind::ForOf { .. });
                let saved = self.env;
                let r = self.exec_for_each(left, right, body, is_of, &labels);
                self.env = saved;
                Ok(SGo::Done(r))
            }
            StmtKind::Try { block, handler, finalizer } => {
                if top {
                    self.completion = Value::Undefined;
                }
                let t = STry { handler, finalizer, env: self.env, strict: self.strict, phase: SPhase::Block, catch_env: None, pending: None, saved_ret: Value::Undefined };
                self.sf_push(st, SFrame::Try(t))?;
                Ok(SGo::Stmt(block))
            }
            StmtKind::Switch { disc, cases } => {
                let labels = std::mem::take(&mut self.pending_labels);
                if top {
                    self.completion = Value::Undefined;
                }
                let d = self.eval_expr(disc)?;
                let any_lex = prog.list(cases).any(|c| self.has_lexical(prog.case(c).body));
                self.sf_room()?;
                let saved = self.env;
                let env = if any_lex { Some(self.new_env(saved, false)?) } else { None };
                if let Some(e) = env {
                    self.env = e;
                }
                self.sf_put(st, SFrame::Switch { cases, next: 0, saved, env, labels });
                if any_lex {
                    for c in prog.list(cases) {
                        self.declare_scope(prog.case(c).body, false)?;
                    }
                }
                let mut start = None;
                for (i, c) in prog.list(cases).enumerate() {
                    if let Some(t) = prog.case(c).test {
                        let v = self.eval_expr(t)?;
                        if d.strict_equals(v) {
                            start = Some(i);
                            break;
                        }
                    }
                }
                if start.is_none() {
                    start = prog.list(cases).position(|c| prog.case(c).test.is_none());
                }
                if let Some(SFrame::Switch { next, .. }) = st.last_mut() {
                    *next = start.unwrap_or(cases.len()) as u32;
                }
                Ok(SGo::Done(Ok(())))
            }
            StmtKind::Labeled { body, .. } => {
                let label = self.stmt_label[s.index()];
                self.sf_room()?;
                self.pending_labels.push(label);
                self.sf_put(st, SFrame::Labeled { label });
                Ok(SGo::Stmt(body))
            }
            _ => Ok(SGo::Done(self.exec_simple(s, &stmt, top))),
        }
    }

    /// A statement with no nested statement. It cannot recurse, so it needs no frame and no `descend`.
    fn exec_simple(&mut self, s: StmtId, st: &Stmt, top: bool) -> Ex {
        let prog = self.prog.clone();
        match st.kind {
            StmtKind::Expr(e) | StmtKind::Directive(e) => {
                let v = self.eval_expr(e)?;
                if top {
                    self.completion = v;
                }
            }
            StmtKind::Var { kind, decls } => {
                for d in prog.list(decls) {
                    let d = *prog.decl(d);
                    let name = if let PatKind::Ident = prog.pat(d.target).kind { Some(self.name_str(self.pat_name[d.target.index()])?) } else { None };
                    match (kind, d.init) {
                        (VarKind::Var, None) => {}
                        (VarKind::Var, Some(i)) => {
                            let v = self.eval_named(i, name)?;
                            self.bind_pattern(d.target, v, Bind::Assign)?;
                        }
                        (_, init) => {
                            let v = match init {
                                Some(i) => self.eval_named(i, name)?,
                                None => Value::Undefined,
                            };
                            self.bind_pattern(d.target, v, Bind::Init)?;
                        }
                    }
                }
            }
            StmtKind::Function(fid) => self.annex_b(fid)?,
            StmtKind::Class(c) => self.exec_class_decl(c)?,
            StmtKind::Return(e) => {
                let v = match e {
                    Some(e) => self.eval_expr(e)?,
                    None => Value::Undefined,
                };
                self.ret = v;
                return Err(Ctl::Return);
            }
            StmtKind::Break(l) => return Err(Ctl::Break(if l.is_some() { self.stmt_label[s.index()] } else { 0 })),
            StmtKind::Continue(l) => return Err(Ctl::Continue(if l.is_some() { self.stmt_label[s.index()] } else { 0 })),
            StmtKind::Throw(e) => {
                let v = self.eval_expr(e)?;
                return Err(Ctl::Err(Error::Throw(v)));
            }
            StmtKind::Empty | StmtKind::Debugger => {}
            StmtKind::With { .. } => return Err(unsupported("with statement", st.span).into()),
            _ => return Err(unsupported("statement in the wrong place", st.span).into()),
        }
        Ok(())
    }

    /// Give a completion to the frame on top.
    fn resume(&mut self, st: &mut Vec<SFrame>, r: Ex) -> SGo {
        // Hot path: the next statement of a list, with no move of the frame.
        if r.is_ok()
            && let Some(SFrame::List { l, i, .. }) = st.last_mut()
            && (*i as usize) < l.len()
        {
            let s = self.list_at(*l, *i as usize);
            *i += 1;
            return SGo::Stmt(s);
        }
        let Some(mut f) = st.pop() else { return SGo::Done(r) };
        let a = self.step_frame(&mut f, r);
        st.push(f);
        self.act(st, a)
    }

    fn act(&mut self, st: &mut Vec<SFrame>, a: SAct) -> SGo {
        match a {
            SAct::Go(g) => g,
            SAct::Leave(r) => match st.pop() {
                Some(f) => self.leave(f, r),
                None => SGo::Done(r),
            },
            SAct::List(l) => match self.sf_push(st, SFrame::List { l, i: 0, scope: None }) {
                Ok(()) => SGo::Done(Ok(())),
                Err(e) => SGo::Done(Err(e.into())),
            },
        }
    }

    /// The one exit of a frame: give back its count and restore what it changed.
    fn leave(&mut self, f: SFrame, r: Ex) -> SGo {
        self.stmt_frames -= 1;
        match f {
            SFrame::List { scope: Some((saved, env)), .. } => {
                self.env = saved;
                self.release(env);
            }
            SFrame::For { saved, .. } => {
                let cur = self.env;
                self.env = saved;
                if cur != saved {
                    self.release(cur);
                }
            }
            SFrame::Switch { saved, env, .. } => {
                self.env = saved;
                if let Some(e) = env {
                    self.release(e);
                }
            }
            SFrame::Try(t) => {
                self.env = t.env;
                self.strict = t.strict;
                if let Some(e) = t.catch_env {
                    self.release(e);
                }
            }
            SFrame::List { .. } | SFrame::While { .. } | SFrame::DoWhile { .. } | SFrame::Labeled { .. } => {}
        }
        SGo::Done(r)
    }

    fn step_frame(&mut self, f: &mut SFrame, r: Ex) -> SAct {
        match f {
            SFrame::List { l, i, .. } => {
                if r.is_ok() && (*i as usize) < l.len() {
                    let s = self.list_at(*l, *i as usize);
                    *i += 1;
                    SAct::Go(SGo::Stmt(s))
                } else {
                    SAct::Leave(r)
                }
            }
            SFrame::While { test, body, labels } => match Self::loop_step(r, labels) {
                Ok(true) => self.while_head(*test, *body),
                Ok(false) => SAct::Leave(Ok(())),
                Err(c) => SAct::Leave(Err(c)),
            },
            SFrame::DoWhile { body, test, labels } => match Self::loop_step(r, labels) {
                Ok(true) => match self.test_true(*test) {
                    Ok(true) => match self.tick() {
                        Ok(()) => SAct::Go(SGo::Stmt(*body)),
                        Err(e) => SAct::Leave(Err(e.into())),
                    },
                    Ok(false) => SAct::Leave(Ok(())),
                    Err(e) => SAct::Leave(Err(e.into())),
                },
                Ok(false) => SAct::Leave(Ok(())),
                Err(c) => SAct::Leave(Err(c)),
            },
            SFrame::For { test, update, body, per_iter, labels, .. } => match Self::loop_step(r, labels) {
                Ok(true) => match self.for_update(*per_iter, *update) {
                    Ok(()) => self.for_head(*test, *per_iter, *body),
                    Err(e) => SAct::Leave(Err(e.into())),
                },
                Ok(false) => SAct::Leave(Ok(())),
                Err(c) => SAct::Leave(Err(c)),
            },
            SFrame::Switch { cases, next, labels, .. } => match r {
                Err(Ctl::Break(l)) if l == 0 || labels.contains(&l) => SAct::Leave(Ok(())),
                Err(c) => SAct::Leave(Err(c)),
                Ok(()) if (*next as usize) < cases.len() => {
                    let c = self.list_at(*cases, *next as usize);
                    *next += 1;
                    SAct::List(self.prog.case(c).body)
                }
                Ok(()) => SAct::Leave(Ok(())),
            },
            SFrame::Try(t) => self.try_step(t, r),
            SFrame::Labeled { label } => {
                self.pending_labels.clear();
                match r {
                    Err(Ctl::Break(l)) if l == *label => SAct::Leave(Ok(())),
                    r => SAct::Leave(r),
                }
            }
        }
    }

    fn while_head(&mut self, test: ExprId, body: StmtId) -> SAct {
        match self.tick().and_then(|_| self.test_true(test)) {
            Ok(true) => SAct::Go(SGo::Stmt(body)),
            Ok(false) => SAct::Leave(Ok(())),
            Err(e) => SAct::Leave(Err(e.into())),
        }
    }

    /// Start of an iteration: the per-iteration copy, the step charge, the test.
    fn for_head(&mut self, test: Option<ExprId>, per_iter: bool, body: StmtId) -> SAct {
        let r = (|| {
            if per_iter && self.envs[self.env as usize].captured {
                self.copy_env()?;
            }
            self.tick()?;
            match test {
                Some(t) => self.test_true(t),
                None => Ok(true),
            }
        })();
        match r {
            Ok(true) => SAct::Go(SGo::Stmt(body)),
            Ok(false) => SAct::Leave(Ok(())),
            Err(e) => SAct::Leave(Err(e.into())),
        }
    }

    fn for_update(&mut self, per_iter: bool, update: Option<ExprId>) -> Res<()> {
        if per_iter && self.envs[self.env as usize].captured {
            self.copy_env()?;
        }
        if let Some(u) = update {
            self.eval_expr(u)?;
        }
        Ok(())
    }

    /// `try`: block, then catch for a JS throw, then finalizer. A limit, unsupported or heap fault skips both.
    fn try_step(&mut self, t: &mut STry, r: Ex) -> SAct {
        match t.phase {
            SPhase::Block => {
                if let (Err(Ctl::Err(Error::Throw(v))), Some(h)) = (&r, t.handler) {
                    let v = *v;
                    let c = *self.prog.catch(h);
                    self.env = t.env;
                    self.strict = t.strict;
                    t.phase = SPhase::Catch;
                    match self.catch_enter(t, c, v) {
                        Ok(()) => SAct::Go(SGo::Stmt(c.body)),
                        Err(c) => {
                            self.env = t.env;
                            if let Some(e) = t.catch_env.take() {
                                self.release(e);
                            }
                            self.try_after(t, Err(c))
                        }
                    }
                } else {
                    self.try_after(t, r)
                }
            }
            SPhase::Catch => {
                self.env = t.env;
                if let Some(e) = t.catch_env.take() {
                    self.release(e);
                }
                self.try_after(t, r)
            }
            SPhase::Finally => {
                let own = t.pending.take().unwrap_or(Ok(()));
                match r {
                    Ok(()) => {
                        self.ret = t.saved_ret;
                        SAct::Leave(own)
                    }
                    Err(c) => SAct::Leave(Err(c)),
                }
            }
        }
    }

    fn catch_enter(&mut self, t: &mut STry, c: Catch, v: Value) -> Ex {
        let env = self.new_env(t.env, false)?;
        t.catch_env = Some(env);
        self.env = env;
        if let Some(p) = c.param {
            self.bind_pattern(p, v, Bind::Declare)?;
        }
        Ok(())
    }

    fn try_after(&mut self, t: &mut STry, r: Ex) -> SAct {
        let Some(fin) = t.finalizer else { return SAct::Leave(r) };
        if let Err(Ctl::Err(e)) = &r
            && !matches!(e, Error::Throw(_))
        {
            return SAct::Leave(r);
        }
        self.env = t.env;
        self.strict = t.strict;
        t.saved_ret = self.ret;
        t.pending = Some(r);
        t.phase = SPhase::Finally;
        SAct::Go(SGo::Stmt(fin))
    }
}
