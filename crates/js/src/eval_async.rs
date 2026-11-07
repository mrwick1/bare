// Async functions and `await` of the evaluator. Included by `eval.rs` (`include!`), so it shares that module's
// imports and private items.
//
// What runs where
// - An async function whose body has no `await` runs on the ordinary evaluator (a throw rejects its promise).
// - A body with `await` runs on an explicit machine. The machine keeps NO Rust call stack across an await. Its
//   state is a `Run`: a stack of small continuation frames (`Fr`, ids and counters only, no heap values) and a
//   stack of slots (`Slot`: values, property keys, counters). Only AST nodes that contain an `await` become
//   frames. Every await-free subtree runs on the ordinary evaluator, so its semantics are the existing ones.
// - The first run is synchronous up to the first await (a real Promise is returned). `await v` does
//   `PromiseResolve(%Promise%, v)` and stores the activation on that promise as a reaction. The reaction becomes a
//   `Job::Resume` in the same FIFO microtask queue as `then` jobs, even for a fulfilled or non-promise `v`. For a
//   `v` that is not an object, the promise of `PromiseResolve` is never visible and already fulfilled, so no
//   promise is made: the resume job is queued at once, at the same point in the queue.
//   A resume restores the script, the environment and the strict flag of the activation, and gives back the
//   caller's on every exit (success, throw and limit). Nothing here owns an OS thread, a timer or a busy loop.
// - Normal completion resolves the result promise. A JavaScript throw rejects it. A limit, `Unsupported` or heap
//   fault is an `Error` (never a rejection); inside a job it follows the failed-checkpoint rule of
//   `drain_microtasks`.
//
// Supported (the preflight, `async_preflight`, rejects every other await position with `Unsupported`, before
// the script has any effect)
// - Functions: async declarations, expressions, arrows (also concise bodies) and object-literal methods.
//   An async function is not a constructor and has no `.prototype`.
// - `await` in: expression statements, `var`/`let`/`const` initializers, `return`, `throw`, `if` tests,
//   `while`/`do`/`for` heads and bodies, `for (x of arr)` (right side and body), blocks, `try`/`catch`/`finally`,
//   `switch` (discriminant, case tests and bodies),
//   labeled statements (at most three labels on one loop), and inside these expressions: unary (not `delete`),
//   binary, logical, conditional, comma, parenthesized, template, array and object literals (the value of a
//   non-computed key, spread), member and index reads, calls and `new` (callee, member base and arguments, with
//   spread), simple assignment and compound or logical assignment to a name, a member, a private member or a
//   `super` property, `obj.#p` reads and calls, `#p in x`, `super.m(await x)` and `[a, b] = await x`.
//   Async methods of classes, static ones included, keep `this`, the class scope and `%home` in their environments.
// - Not supported (Unsupported at preflight): `await` in a parameter, a binding pattern, a computed object key, a
//   `for-in`, an optional chain, `delete`, `++`/`--`, a tagged template (tag or substitutions), the heritage or a computed key
//   of a class, `super(...)` with an await in its arguments, or `super[await k]`. Top-level
//   await and modules stay unsupported as before. Generators, async generators and `for await` run here too: see
//   `eval_generator.rs` and `eval_asyncgen.rs`.
// - Known deviations (the same as the ordinary evaluator): environments, `arguments` and `this` as in `eval.rs`;
//   `await` of a thenable whose `then` is read twice is as in `Promise`.
//
// Budget and caps (all checked BEFORE the thing grows, each with a distinct `LimitKind`)
// - `AsyncLimits::max_activations` suspended activations, `max_frames` and `max_slots` summed over every
//   activation (running ones too), `max_code` marked syntax nodes over every script.
// - The queue and reaction caps of `PromiseLimits` apply to the resume jobs. The step budget is shared:
//   every machine step is one `tick`, and a drain shares one budget over all resumed jobs.
// - The evaluator's configured call and traversal caps apply to resumes too: a resume counts as a call.
//
// GC contract. A suspended activation is reachable only through the promise it awaits (a reaction) or a queued
// resume job. `collect` traces, for every reachable activation: the result promise, the awaited promise, every
// slot (values and property keys), the call environment, the environment at the suspension and every
// environment saved in a frame. An activation nobody reaches (an unreachable cycle, a promise that never
// settles) is dropped, its slot is freed and its handle (`ActRef`, with a never reused generation) goes stale.
// A resume through a stale handle does nothing. Marks of the preflight are keyed by (script slot, node id).

/// Caps of the async machine. See the header.
#[derive(Clone, Copy, Debug)]
pub struct AsyncLimits {
    pub max_activations: usize,
    pub max_frames: usize,
    pub max_slots: usize,
    pub max_code: usize,
}

impl Default for AsyncLimits {
    fn default() -> Self {
        AsyncLimits { max_activations: 1 << 16, max_frames: 1 << 20, max_slots: 1 << 20, max_code: 1 << 20 }
    }
}

/// Counters for tools and tests.
#[derive(Clone, Copy, Debug, Default)]
pub struct AsyncStats {
    /// Suspended activations.
    pub activations: usize,
    /// Slots of the activation table (free ones included).
    pub table: usize,
    pub frames: usize,
    pub slots: usize,
    pub marks: usize,
    /// Entries of the generator table (generator objects not yet collected).
    pub generators: usize,
}

/// A generation-safe handle of a suspended activation.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct ActRef {
    idx: u32,
    era: u32,
}

#[derive(Clone, Copy)]
enum Slot {
    V(Value),
    K(PropertyKey),
    N(u32),
    /// A for-of iteration in flight: the whole `It` (source, kind, position), including a generic iterator and its
    /// `next` method. `run_trace` marks every value in it.
    It(It),
}

/// An abrupt completion in flight. A `Return` or `Throw` value is held in a slot while a `finally` runs.
#[derive(Clone, Copy)]
enum Abr {
    Break(u32),
    Continue(u32),
    Return(Value),
    Throw(Value),
}

enum Next {
    Eval(ExprId),
    Exec(StmtId),
    Val(Value),
    Abr(Abr),
}

enum Out {
    Done(Value),
    Threw(Value),
    /// Wait for a promise.
    Wait(ObjId),
    /// `await` of a value that is not an object: resume with it in a job, as from a fulfilled promise.
    Ready(Value),
    /// A generator yields a value (`false`), or passes an inner result object of `yield*` through (`true`).
    Yield(Value, bool),
}

enum Step {
    Next(Next),
    Out(Out),
}

/// Labels of a loop (at most three, checked by the preflight).
#[derive(Clone, Copy, Default)]
struct Lbl {
    l: [u32; 3],
    n: u8,
}

impl Lbl {
    fn has(&self, x: u32) -> bool {
        x == 0 || self.l[..self.n as usize].contains(&x)
    }
}

/// A continuation frame: what to do with the next value or completion. No heap value lives here.
#[derive(Clone, Copy)]
enum Fr {
    Body,
    BodyExpr,
    /// Operand `i` of expression `e` is being evaluated. Earlier operands are in the slots from `base`.
    Op { e: ExprId, i: u32, base: u32 },
    Discard,
    Ret,
    Thr,
    List { l: List<Stmt>, i: u32 },
    Scope { saved: u32, env: u32 },
    IfT { s: StmtId },
    VarD { s: StmtId, d: u32 },
    Lab { label: u32 },
    WhileT { s: StmtId, lb: Lbl },
    WhileB { s: StmtId, lb: Lbl },
    DoB { s: StmtId, lb: Lbl },
    DoT { s: StmtId, lb: Lbl },
    ForScope { saved: u32 },
    ForInit { s: StmtId, lb: Lbl, per: bool },
    ForT { s: StmtId, lb: Lbl, per: bool },
    ForB { s: StmtId, lb: Lbl, per: bool },
    ForU { s: StmtId, lb: Lbl, per: bool },
    OfR { s: StmtId, lb: Lbl },
    OfScope { outer: u32, cur: u32, base: u32 },
    /// The body of a for-of runs. Slot `base` is `Slot::It`.
    OfB { s: StmtId, lb: Lbl, base: u32 },
    TryB { s: StmtId, env: u32 },
    TryC { s: StmtId, env: u32 },
    /// The discriminant of switch `s` is being evaluated.
    SwD { s: StmtId },
    /// A switch runs. Catches an unlabeled break. While a case test awaits, slot `base` holds the discriminant.
    SwS { base: u32 },
    /// The test of case `i` is being evaluated.
    SwT { s: StmtId, base: u32, i: u32 },
    /// The body of case `i` runs. A normal end falls through to case `i + 1`.
    SwB { s: StmtId, i: u32 },
    /// A `finally` block runs. `kind`: 0 normal, 1 break, 2 continue, 3 return, 4 throw (the value is in slot `base`).
    Fin { kind: u8, label: u32, base: u32 },
    /// A `yield*` waits for the next resume. Slot `base` is the inner `Slot::It`.
    Deleg { base: u32 },
    /// Async generators (`eval_asyncgen.rs`): yield the awaited value.
    AYield,
    /// Return the awaited value (`return x`, a `return()` resume, the end of a `yield*` by `return`).
    ARet,
    /// An async `yield*` waits for the awaited result of `next` or `throw` (`ret` false) or of `return` (`ret` true).
    ADeleg { base: u32, ret: bool },
    /// An async `yield*` closed an inner iterator that has no `throw`: the awaited result of `return`.
    ADelegClose { base: u32 },
    /// A `for await` waits for the awaited result of `next`. Slot `base` is the `Slot::It`.
    AOfN { s: StmtId, lb: Lbl, base: u32 },
    /// The body of a `for await` runs.
    AOfB { s: StmtId, lb: Lbl, base: u32 },
    /// A `for await` left early and waits for the result of `return`. The completion is kept as in `Fin`.
    AClose { kind: u8, label: u32, base: u32 },
}

/// The whole state of one running or suspended async function call.
struct Run {
    /// Slot of the script that owns the function: every id in the frames belongs to it.
    script: u32,
    fid: FuncId,
    strict: bool,
    /// An async generator: `result` is the generator object, not a promise.
    agen: bool,
    result: ObjId,
    call_env: u32,
    /// The environment at the suspension.
    cur_env: u32,
    awaiting: Option<ObjId>,
    stack: Vec<Fr>,
    slots: Vec<Slot>,
}

struct ActSlot {
    era: u32,
    run: Option<Run>,
}

#[derive(Default)]
struct AsyncState {
    limits: AsyncLimits,
    acts: Vec<ActSlot>,
    free: Vec<u32>,
    live: usize,
    next_era: u32,
    frames: usize,
    slots: usize,
    /// Nodes with an `await` inside: (script slot, node index, 0 expr | 1 stmt | 2 func).
    marks: FxSet<(u32, u32, u8)>,
    /// Generator objects (`eval_generator.rs`).
    gens: FxMap<ObjId, Gen>,
    gen_intr: Option<GenIntr>,
    /// The `Function` constructor, the prototype of `GeneratorFunction` and `AsyncFunction`.
    fn_ctor: Option<ObjId>,
    /// `%AsyncFunction.prototype%`, made by the first async function.
    afn_proto: Option<ObjId>,
    /// Async generator objects (`eval_asyncgen.rs`).
    agens: FxMap<ObjId, AGen>,
    ag_intr: Option<AgIntr>,
    /// The preflight scans an async generator: a `return` with a value awaits it.
    scan_ag: bool,
}

fn nx(n: Next) -> Res<Step> {
    Ok(Step::Next(n))
}

fn val(v: Value) -> Res<Step> {
    Ok(Step::Next(Next::Val(v)))
}

fn sval(run: &Run, i: usize) -> Value {
    match run.slots[i] {
        Slot::V(v) => v,
        _ => Value::Undefined,
    }
}

fn skey(run: &Run, i: usize) -> PropertyKey {
    match run.slots[i] {
        Slot::K(k) => k,
        _ => PropertyKey::Index(0),
    }
}

fn run_trace(run: &Run, work: &mut Vec<Value>, env_work: &mut Vec<u32>) {
    work.push(Value::Object(run.result));
    work.extend(run.awaiting.map(Value::Object));
    for s in &run.slots {
        match *s {
            Slot::V(v) => work.push(v),
            Slot::K(PropertyKey::String(s)) => work.push(Value::String(s)),
            Slot::K(PropertyKey::Symbol(y)) => work.push(Value::Symbol(y)),
            Slot::It(it) => {
                work.push(it.src);
                match it.kind {
                    ItKind::Plain => {}
                    ItKind::Coll { owner, .. } => work.push(Value::Object(owner)),
                    ItKind::Generic { iter, next } => work.extend([iter, next]),
                }
            }
            _ => {}
        }
    }
    env_work.extend([run.call_env, run.cur_env]);
    for f in &run.stack {
        match *f {
            Fr::Scope { saved, env } => env_work.extend([saved, env]),
            Fr::ForScope { saved } => env_work.push(saved),
            Fr::OfScope { outer, cur, .. } => env_work.extend([outer, cur]),
            Fr::TryB { env, .. } | Fr::TryC { env, .. } => env_work.push(env),
            _ => {}
        }
    }
}

impl<'p> Interp<'p> {
    // ------------------------------------------------------------ host API

    pub fn set_async_limits(&mut self, mut l: AsyncLimits) {
        l.max_frames = l.max_frames.min(u32::MAX as usize);
        l.max_slots = l.max_slots.min(u32::MAX as usize);
        self.aw.limits = l;
    }

    pub fn async_stats(&self) -> AsyncStats {
        AsyncStats { activations: self.aw.live, table: self.aw.acts.len(), frames: self.aw.frames, slots: self.aw.slots, marks: self.aw.marks.len(), generators: self.aw.gens.len() }
    }

    // ------------------------------------------------------------ preflight (marks)

    fn aw_expr(&self, e: ExprId) -> bool {
        self.aw.marks.contains(&(self.cur as u32, e.index() as u32, 0))
    }

    fn aw_stmt(&self, s: StmtId) -> bool {
        self.aw.marks.contains(&(self.cur as u32, s.index() as u32, 1))
    }

    fn aw_func(&self, f: FuncId) -> bool {
        self.aw.marks.contains(&(self.cur as u32, f.index() as u32, 2))
    }

    fn mark(&mut self, id: usize, tag: u8) -> Res<()> {
        let key = (self.cur as u32, id as u32, tag);
        if self.aw.marks.contains(&key) {
            return Ok(());
        }
        if self.aw.marks.len() >= self.aw.limits.max_code {
            return Err(Error::Limit(LimitKind::AsyncCode));
        }
        self.aw.marks.insert(key);
        Ok(())
    }

    /// A script that was refused leaves no marks.
    fn async_forget(&mut self, script: usize) {
        self.aw.marks.retain(|k| k.0 != script as u32);
    }

    /// Check the body of the async function `fid` of the current script and mark the nodes that have an `await`
    /// inside. Every await position the machine cannot run is an `Unsupported`.
    fn async_preflight(&mut self, fid: FuncId) -> Res<()> {
        let prog = self.prog.clone();
        let f = *prog.func(fid);
        self.aw.scan_ag = f.mode == FuncMode::AsyncGen;
        for p in prog.list(f.params) {
            if self.scan_pat(p)? {
                return Err(unsupported("await in a parameter", f.span));
            }
        }
        let mut any = false;
        match f.body {
            Body::Expr(e) => any |= self.scan_expr(e)?,
            Body::Block(l) => {
                for s in prog.list(l) {
                    any |= self.scan_stmt(s)?;
                }
            }
        }
        if any {
            self.mark(fid.index(), 2)?;
        }
        Ok(())
    }

    /// True if the pattern has an `await` inside (a default or a computed key).
    fn scan_pat(&mut self, p: PatId) -> Res<bool> {
        self.descend(|it| it.scan_pat_inner(p))
    }

    fn scan_pat_inner(&mut self, p: PatId) -> Res<bool> {
        let prog = self.prog.clone();
        let mut any = false;
        match prog.pat(p).kind {
            PatKind::Ident | PatKind::Hole => {}
            PatKind::Member(e) => any |= self.scan_expr(e)?,
            PatKind::Array(l) => {
                for q in prog.list(l) {
                    any |= self.scan_pat(q)?;
                }
            }
            PatKind::Object(l) => {
                for q in prog.list(l) {
                    let pp = *prog.pat_prop(q);
                    if let Key::Computed(e) = pp.key {
                        any |= self.scan_expr(e)?;
                    }
                    any |= self.scan_pat(pp.value)?;
                }
            }
            PatKind::Assign { target, default } => {
                any |= self.scan_pat(target)?;
                any |= self.scan_expr(default)?;
            }
            PatKind::Rest(t) => any |= self.scan_pat(t)?,
        }
        Ok(any)
    }

    fn scan_expr(&mut self, e: ExprId) -> Res<bool> {
        self.descend(|it| it.scan_expr_inner(e))
    }

    fn scan_list(&mut self, l: List<Expr>) -> Res<bool> {
        let prog = self.prog.clone();
        let mut any = false;
        for x in prog.list(l) {
            any |= self.scan_expr(x)?;
        }
        Ok(any)
    }

    fn scan_expr_inner(&mut self, e: ExprId) -> Res<bool> {
        let prog = self.prog.clone();
        let ex = *prog.expr(e);
        let no = |what: &'static str| Err(unsupported(what, ex.span));
        let aw = match ex.kind {
            ExprKind::Await(a) => {
                self.scan_expr(a)?;
                true
            }
            ExprKind::Yield { arg, .. } => {
                if let Some(a) = arg {
                    self.scan_expr(a)?;
                }
                true
            }
            ExprKind::Template(t) => {
                let t = *prog.template(t);
                let mut any = false;
                for x in prog.template_exprs(&t) {
                    any |= self.scan_expr(x)?;
                }
                any
            }
            ExprKind::Tagged { tag, quasi } => {
                let t = *prog.template(quasi);
                let mut any = self.scan_expr(tag)?;
                for x in prog.template_exprs(&t) {
                    any |= self.scan_expr(x)?;
                }
                if any {
                    return no("await in a tagged template");
                }
                false
            }
            ExprKind::Array(l) | ExprKind::Seq(l) => self.scan_list(l)?,
            ExprKind::Object(l) => {
                let mut any = false;
                for p in prog.list(l) {
                    let pr = *prog.prop(p);
                    let mut here = false;
                    if let Key::Computed(k) = pr.key {
                        here |= self.scan_expr(k)?;
                    }
                    if matches!(pr.kind, PropKind::Init | PropKind::Shorthand | PropKind::Spread) {
                        let v = self.scan_expr(pr.value)?;
                        if v && matches!(pr.key, Key::Computed(_)) {
                            return no("await in the value of a computed object key");
                        }
                        here |= v;
                    }
                    if here && matches!(pr.key, Key::Computed(_)) {
                        return no("await in a computed object key");
                    }
                    any |= here;
                }
                any
            }
            ExprKind::Unary { op, arg } => {
                let a = self.scan_expr(arg)?;
                if a && op == UnaryOp::Delete {
                    return no("await inside delete");
                }
                a
            }
            ExprKind::Update { arg, .. } => {
                if self.scan_expr(arg)? {
                    return no("await inside ++ or --");
                }
                false
            }
            ExprKind::Binary { left, right, .. } | ExprKind::Logical { left, right, .. } => {
                let l = self.scan_expr(left)?;
                self.scan_expr(right)? | l
            }
            ExprKind::Assign { op, target, value } => {
                let pat = *prog.pat(target);
                let t = match pat.kind {
                    PatKind::Ident => false,
                    PatKind::Member(me) => self.scan_expr(me)?,
                    PatKind::Array(_) | PatKind::Object(_) => {
                        let t = self.scan_pat(target)?;
                        if t {
                            return no("await in an assignment pattern");
                        }
                        t
                    }
                    _ => false,
                };
                let v = self.scan_expr(value)?;
                if (t || v) && matches!(pat.kind, PatKind::Array(_) | PatKind::Object(_)) && op != AssignOp::Assign {
                    return no("assignment target");
                }
                t | v
            }
            ExprKind::Cond { test, cons, alt } => {
                let a = self.scan_expr(test)?;
                let b = self.scan_expr(cons)?;
                self.scan_expr(alt)? | a | b
            }
            ExprKind::Call { callee, args, optional } => {
                let c = self.scan_expr(callee)?;
                let a = self.scan_list(args)?;
                if c || a {
                    if optional {
                        return no("await inside an optional call");
                    }
                    let mut x = callee;
                    while let ExprKind::Paren(i) = prog.expr(x).kind {
                        x = i;
                    }
                    if matches!(prog.expr(x).kind, ExprKind::Super) {
                        return no("await in the arguments of super()");
                    }
                    if matches!(prog.expr(x).kind, ExprKind::Chain(_)) {
                        return no("await in a call of an optional chain");
                    }
                }
                c | a
            }
            ExprKind::New { callee, args } => {
                let c = self.scan_expr(callee)?;
                self.scan_list(args)? | c
            }
            ExprKind::Member { obj, optional, .. } => {
                let o = self.scan_expr(obj)?;
                if o && optional {
                    return no("await inside an optional chain");
                }
                o
            }
            ExprKind::Index { obj, index, optional } => {
                let o = self.scan_expr(obj)?;
                let i = self.scan_expr(index)?;
                if i && matches!(prog.expr(obj).kind, ExprKind::Super) {
                    return no("await in a super property key");
                }
                if (o || i) && optional {
                    return no("await inside an optional chain");
                }
                o | i
            }
            ExprKind::PrivateMember { obj, optional, .. } => {
                let o = self.scan_expr(obj)?;
                if o && optional {
                    return no("await inside an optional chain");
                }
                o
            }
            ExprKind::PrivateIn { right, .. } => self.scan_expr(right)?,
            ExprKind::Spread(a) | ExprKind::Paren(a) => self.scan_expr(a)?,
            ExprKind::Chain(a) => {
                if self.scan_expr(a)? {
                    return no("await inside an optional chain");
                }
                false
            }
            // leaves, nested functions (scanned when the walk reaches them) and what the walk rejects
            _ => false,
        };
        if aw {
            self.mark(e.index(), 0)?;
        }
        Ok(aw)
    }

    fn scan_stmt(&mut self, s: StmtId) -> Res<bool> {
        self.descend(|it| it.scan_stmt_inner(s))
    }

    fn scan_stmt_inner(&mut self, s: StmtId) -> Res<bool> {
        let prog = self.prog.clone();
        let st = *prog.stmt(s);
        let no = |what: &'static str| Err(unsupported(what, st.span));
        let aw = match st.kind {
            StmtKind::Expr(e) | StmtKind::Throw(e) => self.scan_expr(e)?,
            StmtKind::Return(Some(e)) => self.scan_expr(e)? | self.aw.scan_ag,
            StmtKind::Var { decls, .. } => {
                let mut any = false;
                for d in prog.list(decls) {
                    let d = *prog.decl(d);
                    if self.scan_pat(d.target)? {
                        return no("await in a binding pattern");
                    }
                    if let Some(i) = d.init {
                        any |= self.scan_expr(i)?;
                    }
                }
                any
            }
            StmtKind::If { test, cons, alt } => {
                let t = self.scan_expr(test)?;
                let c = self.scan_stmt(cons)?;
                let a = match alt {
                    Some(a) => self.scan_stmt(a)?,
                    None => false,
                };
                t | c | a
            }
            StmtKind::While { test, body } | StmtKind::DoWhile { body, test } => {
                let t = self.scan_expr(test)?;
                self.scan_stmt(body)? | t
            }
            StmtKind::For { init, test, update, body } => {
                let mut any = match init {
                    ForInit::None => false,
                    ForInit::Var(v) => self.scan_stmt(v)?,
                    ForInit::Expr(e) => self.scan_expr(e)?,
                };
                for e in [test, update].into_iter().flatten() {
                    any |= self.scan_expr(e)?;
                }
                any | self.scan_stmt(body)?
            }
            StmtKind::ForIn { left, right, body } => {
                let l = match left {
                    ForLeft::Var(v) => self.scan_stmt(v)?,
                    ForLeft::Pat(p) => self.scan_pat(p)?,
                };
                if l | self.scan_expr(right)? | self.scan_stmt(body)? {
                    return no("await inside for-in");
                }
                false
            }
            StmtKind::ForOf { left, right, body, is_await } => {
                let l = match left {
                    ForLeft::Var(v) => self.scan_stmt(v)?,
                    ForLeft::Pat(p) => self.scan_pat(p)?,
                };
                if l {
                    return no("await in a for-of binding");
                }
                let r = self.scan_expr(right)?;
                self.scan_stmt(body)? | r | is_await
            }
            StmtKind::Try { block, handler, finalizer } => {
                let mut any = self.scan_stmt(block)?;
                if let Some(c) = handler {
                    let c = *prog.catch(c);
                    if let Some(p) = c.param
                        && self.scan_pat(p)?
                    {
                        return no("await in a catch binding");
                    }
                    any |= self.scan_stmt(c.body)?;
                }
                if let Some(f) = finalizer {
                    any |= self.scan_stmt(f)?;
                }
                any
            }
            StmtKind::Switch { disc, cases } => {
                let mut any = self.scan_expr(disc)?;
                for c in prog.list(cases) {
                    let c = *prog.case(c);
                    if let Some(t) = c.test {
                        any |= self.scan_expr(t)?;
                    }
                    for b in prog.list(c.body) {
                        any |= self.scan_stmt(b)?;
                    }
                }
                any
            }
            StmtKind::Labeled { body, .. } => {
                let b = self.scan_stmt(body)?;
                if b {
                    let (mut n, mut x) = (1, body);
                    while let StmtKind::Labeled { body, .. } = prog.stmt(x).kind {
                        n += 1;
                        x = body;
                    }
                    if n > 3 {
                        return no("more than three labels on a statement with await");
                    }
                }
                b
            }
            StmtKind::Block(l) => {
                let mut any = false;
                for x in prog.list(l) {
                    any |= self.scan_stmt(x)?;
                }
                any
            }
            _ => false,
        };
        if aw {
            self.mark(s.index(), 1)?;
        }
        Ok(aw)
    }

    // ------------------------------------------------------------ counters

    fn fpush(&mut self, run: &mut Run, f: Fr) -> Res<()> {
        if self.aw.frames >= self.aw.limits.max_frames {
            return Err(Error::Limit(LimitKind::AsyncFrames));
        }
        self.aw.frames += 1;
        run.stack.push(f);
        Ok(())
    }

    fn fpop(&mut self, run: &mut Run) -> Option<Fr> {
        let f = run.stack.pop()?;
        self.aw.frames -= 1;
        Some(f)
    }

    fn spush(&mut self, run: &mut Run, s: Slot) -> Res<()> {
        if self.aw.slots >= self.aw.limits.max_slots {
            return Err(Error::Limit(LimitKind::AsyncSlots));
        }
        self.aw.slots += 1;
        run.slots.push(s);
        Ok(())
    }

    fn strunc(&mut self, run: &mut Run, len: usize) {
        let n = run.slots.len().saturating_sub(len);
        run.slots.truncate(len);
        self.aw.slots -= n;
    }

    /// Give the counters of a finished or dropped run back and free the scope environments of its frames.
    fn run_discard(&mut self, mut run: Run) {
        while let Some(f) = self.fpop(&mut run) {
            match f {
                Fr::Scope { env, .. } => self.release(env),
                Fr::OfScope { cur, .. } if cur != NONE => self.release(cur),
                _ => {}
            }
        }
        self.strunc(&mut run, 0);
        self.release(run.call_env);
    }

    // ------------------------------------------------------------ the activation table

    fn act_ref(&self, a: ActRef) -> Option<&Run> {
        let s = self.aw.acts.get(a.idx as usize)?;
        if s.era == a.era { s.run.as_ref() } else { None }
    }

    /// Take a suspended run out of the table. A stale handle gives `None`.
    fn act_take(&mut self, a: ActRef) -> Option<Run> {
        let s = self.aw.acts.get_mut(a.idx as usize)?;
        if s.era != a.era {
            return None;
        }
        let run = s.run.take()?;
        self.aw.free.push(a.idx);
        self.aw.live -= 1;
        Some(run)
    }

    /// Store `run` as an activation that waits for the promise `Ok(p)`, or that resumes with the value `Err(v)`,
    /// or fail before anything changes.
    fn suspend(&mut self, mut run: Run, w: Result<ObjId, Value>) -> Res<()> {
        let pending = w.is_ok_and(|p| matches!(self.promise.map.get(&p).map(|d| &d.state), Some(PState::Pending)));
        let precheck = (|| {
            if self.aw.live >= self.aw.limits.max_activations {
                return Err(Error::Limit(LimitKind::AsyncActivations));
            }
            if pending && self.promise.reactions >= self.promise.limits.max_reactions {
                return Err(Error::Limit(LimitKind::PromiseReactions));
            }
            if !pending && self.promise.queue.len() >= self.promise.limits.max_queue {
                return Err(Error::Limit(LimitKind::PromiseQueue));
            }
            self.aw.next_era.checked_add(1).ok_or(Error::Limit(LimitKind::AsyncActivations))
        })();
        let era = match precheck {
            Ok(g) => g,
            Err(e) => {
                self.run_discard(run);
                return Err(e);
            }
        };
        self.aw.next_era = era;
        run.cur_env = self.env;
        run.awaiting = w.ok();
        let idx = match self.aw.free.pop() {
            Some(i) => {
                self.aw.acts[i as usize] = ActSlot { era, run: Some(run) };
                i
            }
            None => {
                self.aw.acts.push(ActSlot { era, run: Some(run) });
                self.aw.acts.len() as u32 - 1
            }
        };
        self.aw.live += 1;
        let act = ActRef { idx, era };
        let p = match w {
            Ok(p) => p,
            Err(v) => {
                self.promise.queue.push_back(Job::Resume { act, arg: v, rejected: false });
                return Ok(());
            }
        };
        let d = self.promise.map.get_mut(&p).expect("tracked promise");
        d.handled = true;
        match d.state {
            PState::Pending => {
                if d.reactions.capacity() == 0 {
                    d.reactions.reserve_exact(1);
                }
                d.reactions.push(Reaction { on_ok: Value::Undefined, on_err: Value::Undefined, derived: p, act: Some(act) });
                self.promise.reactions += 1;
            }
            PState::Fulfilled(v) => self.promise.queue.push_back(Job::Resume { act, arg: v, rejected: false }),
            PState::Rejected(v) => self.promise.queue.push_back(Job::Resume { act, arg: v, rejected: true }),
        }
        Ok(())
    }

    // ------------------------------------------------------------ GC

    fn async_mark(&self, a: ActRef, work: &mut Vec<Value>, env_work: &mut Vec<u32>, seen: &mut [bool]) {
        let Some(run) = self.act_ref(a) else { return };
        if std::mem::replace(&mut seen[a.idx as usize], true) {
            return;
        }
        run_trace(run, work, env_work);
    }

    /// Activations of the queued resume jobs are roots.
    fn async_queue_roots(&self, work: &mut Vec<Value>, env_work: &mut Vec<u32>, seen: &mut [bool]) {
        for j in &self.promise.queue {
            if let Job::Resume { act, .. } = *j {
                self.async_mark(act, work, env_work, seen);
            }
        }
    }

    /// Activations that wait on the promise `o` are reachable when `o` is.
    fn async_promise_trace(&self, o: ObjId, work: &mut Vec<Value>, env_work: &mut Vec<u32>, seen: &mut [bool]) {
        let Some(d) = self.promise.map.get(&o) else { return };
        for r in &d.reactions {
            if let Some(a) = r.act {
                self.async_mark(a, work, env_work, seen);
            }
        }
    }

    /// Drop the activations that `collect` did not reach. Handles to them are stale from now on.
    fn async_prune(&mut self, seen: &[bool]) {
        for i in 0..self.aw.acts.len() {
            if self.aw.acts[i].run.is_some() && !seen.get(i).copied().unwrap_or(false) {
                let run = self.aw.acts[i].run.take().expect("checked");
                self.aw.frames -= run.stack.len();
                self.aw.slots -= run.slots.len();
                self.aw.live -= 1;
                self.aw.free.push(i as u32);
            }
        }
        while self.aw.acts.last().is_some_and(|s| s.run.is_none()) {
            self.aw.acts.pop();
        }
        let n = self.aw.acts.len() as u32;
        self.aw.free.retain(|&i| i < n);
        if self.aw.acts.is_empty() {
            self.aw.acts.shrink_to_fit();
        }
    }

    /// Every value of every activation, for `roots`.
    fn async_all_values(&self, out: &mut Vec<Value>) {
        let mut scratch = Vec::new();
        for s in &self.aw.acts {
            if let Some(run) = &s.run {
                run_trace(run, out, &mut scratch);
            }
        }
    }

    // ------------------------------------------------------------ calling

    /// Call an async function. It runs to its first await and gives back its result promise.
    fn call_async(&mut self, (script, fid): (u32, FuncId), fobj: ObjId, cenv: u32, self_binding: bool, this: Value, args: &[Value]) -> Res<Value> {
        let (saved_env, saved_strict, saved_script) = (self.env, self.strict, self.cur);
        let result = self.new_promise()?;
        let env = self.new_env(cenv, true)?;
        self.env = env;
        self.select(script as usize);
        let r = self.in_frame(script, Where::Func(fid), false, |it| it.async_start((script, fid), fobj, env, self_binding, this, args, result));
        self.select(saved_script);
        self.env = saved_env;
        self.strict = saved_strict;
        r.map(|()| Value::Object(result))
    }

    #[allow(clippy::too_many_arguments)]
    fn async_start(&mut self, (script, fid): (u32, FuncId), fobj: ObjId, env: u32, self_binding: bool, this: Value, args: &[Value], result: ObjId) -> Res<()> {
        let prog = self.prog.clone();
        let f = *prog.func(fid);
        if !self.aw_func(fid) {
            // No await in the body: the ordinary evaluator runs it.
            let r = self.call_body(fobj, fid, env, self_binding, this, args);
            self.release(env);
            return match r {
                Ok(v) => self.resolve_promise(result, v),
                Err(Error::Throw(v)) => self.reject_promise(result, v),
                Err(e) => Err(e),
            };
        }
        let bound = self.bind_call(fobj, fid, env, self_binding, this, args).and_then(|()| match f.body {
            Body::Block(l) => self.declare_scope(l, true),
            Body::Expr(_) => Ok(()),
        });
        match bound {
            Ok(()) => {}
            Err(Error::Throw(v)) => {
                self.release(env);
                return self.reject_promise(result, v);
            }
            Err(e) => {
                self.release(env);
                return Err(e);
            }
        }
        let mut run = Run { script, fid, strict: self.strict, agen: false, result, call_env: env, cur_env: env, awaiting: None, stack: Vec::new(), slots: Vec::new() };
        let first = match f.body {
            Body::Expr(e) => self.fpush(&mut run, Fr::BodyExpr).map(|()| Next::Eval(e)),
            Body::Block(l) => self.fpush(&mut run, Fr::Body).and_then(|()| self.list_from(&mut run, l, 0)).map(|s| match s {
                Step::Next(n) => n,
                Step::Out(_) => Next::Val(Value::Undefined),
            }),
        };
        let r = match first {
            Ok(n) => self.drive(&mut run, n),
            Err(Error::Throw(v)) => self.drive(&mut run, Next::Abr(Abr::Throw(v))),
            Err(e) => Err(e),
        };
        self.async_finish(run, r)
    }

    /// Resume a suspended activation (a queued `Job::Resume`). A stale handle does nothing.
    fn async_resume(&mut self, a: ActRef, arg: Value, rejected: bool) -> Res<()> {
        let Some(mut run) = self.act_take(a) else { return Ok(()) };
        let (saved_env, saved_strict, saved_script) = (self.env, self.strict, self.cur);
        self.depth += 1;
        self.select(run.script as usize);
        self.env = run.cur_env;
        self.strict = run.strict;
        run.awaiting = None;
        let next = if rejected { Next::Abr(Abr::Throw(arg)) } else { Next::Val(arg) };
        let (script, fid) = (run.script, run.fid);
        let r = self.in_frame(script, Where::Func(fid), false, |it| {
            let r = it.drive(&mut run, next);
            it.async_finish(run, r)
        });
        self.depth -= 1;
        self.select(saved_script);
        self.env = saved_env;
        self.strict = saved_strict;
        r
    }

    /// What the end of a machine run does: settle the result promise, store the activation or give the error.
    fn async_finish(&mut self, run: Run, r: Res<Out>) -> Res<()> {
        if run.agen {
            return self.ag_after(run, r);
        }
        let result = run.result;
        match r {
            Ok(Out::Done(v)) => {
                self.run_discard(run);
                self.resolve_promise(result, v)
            }
            Ok(Out::Threw(v)) => {
                self.run_discard(run);
                self.reject_promise(result, v)
            }
            Ok(Out::Wait(p)) => self.suspend(run, Ok(p)),
            Ok(Out::Ready(v)) => self.suspend(run, Err(v)),
            Ok(Out::Yield(..)) => {
                self.run_discard(run);
                Err(unsupported("yield outside a generator", Span::default()))
            }
            Err(e) => {
                self.run_discard(run);
                Err(e)
            }
        }
    }

    // ------------------------------------------------------------ the machine

    fn drive(&mut self, run: &mut Run, mut next: Next) -> Res<Out> {
        loop {
            self.tick()?;
            let r = match next {
                Next::Eval(e) => self.eval_node(run, e),
                Next::Exec(s) => self.exec_node(run, s),
                Next::Val(v) => self.deliver(run, v),
                Next::Abr(a) => self.unwind(run, a),
            };
            next = match r {
                Ok(Step::Next(n)) => n,
                Ok(Step::Out(o)) => return Ok(o),
                Err(Error::Throw(v)) => Next::Abr(Abr::Throw(v)),
                Err(e) => return Err(e),
            };
        }
    }

    fn ctl_step(&mut self, c: Ctl) -> Res<Step> {
        match c {
            Ctl::Break(l) => nx(Next::Abr(Abr::Break(l))),
            Ctl::Continue(l) => nx(Next::Abr(Abr::Continue(l))),
            Ctl::Return => nx(Next::Abr(Abr::Return(std::mem::replace(&mut self.ret, Value::Undefined)))),
            Ctl::Err(e) => Err(e),
        }
    }

    fn list_at<T>(&self, l: List<T>, i: usize) -> Id<T> {
        Id::from_raw_pub(self.prog.lists[l.start as usize + i])
    }

    fn strip(&self, mut e: ExprId) -> ExprId {
        while let ExprKind::Paren(i) = self.prog.expr(e).kind {
            e = i;
        }
        e
    }

    // ---- expressions

    fn ask(&mut self, run: &mut Run, e: ExprId, i: u32, base: u32, x: ExprId) -> Res<Step> {
        self.fpush(run, Fr::Op { e, i, base })?;
        nx(Next::Eval(x))
    }

    fn eval_node(&mut self, run: &mut Run, e: ExprId) -> Res<Step> {
        if !self.aw_expr(e) {
            let v = self.eval_expr(e)?;
            return val(v);
        }
        let prog = self.prog.clone();
        let ex = *prog.expr(e);
        let base = run.slots.len() as u32;
        match ex.kind {
            ExprKind::Paren(i) => nx(Next::Eval(i)),
            ExprKind::Await(a) | ExprKind::Unary { arg: a, .. } | ExprKind::Yield { arg: Some(a), .. } => self.ask(run, e, 0, base, a),
            ExprKind::Yield { arg: None, .. } if run.agen => {
                self.fpush(run, Fr::AYield)?;
                Ok(Step::Out(Out::Ready(Value::Undefined)))
            }
            ExprKind::Yield { arg: None, .. } => Ok(Step::Out(Out::Yield(Value::Undefined, false))),
            ExprKind::Binary { left, .. } | ExprKind::Logical { left, .. } => self.ask(run, e, 0, base, left),
            ExprKind::Cond { test, .. } => self.ask(run, e, 0, base, test),
            ExprKind::Seq(l) => {
                let x = self.list_at(l, 0);
                self.ask(run, e, 0, base, x)
            }
            ExprKind::Template(t) => {
                let t = *prog.template(t);
                let x = prog.template_exprs(&t).next().ok_or_else(|| unsupported("template", ex.span))?;
                self.ask(run, e, 0, base, x)
            }
            ExprKind::Array(_) => {
                let a = h!(self, self.heap.new_array(Some(self.array_proto)));
                self.spush(run, Slot::V(Value::Object(a)))?;
                self.spush(run, Slot::N(0))?;
                self.arr_next(run, e, base, 0)
            }
            ExprKind::Object(_) => {
                let o = h!(self, self.heap.new_object(Some(self.object_proto)));
                self.spush(run, Slot::V(Value::Object(o)))?;
                self.obj_next(run, e, base, 0)
            }
            ExprKind::Member { obj, .. } | ExprKind::Index { obj, .. } | ExprKind::PrivateMember { obj, .. } => self.ask(run, e, 0, base, obj),
            ExprKind::PrivateIn { right, .. } => self.ask(run, e, 0, base, right),
            ExprKind::Call { callee, .. } => {
                let first = match self.callee_shape(callee) {
                    CShape::Plain => callee,
                    CShape::Member(_, obj) | CShape::Index(obj, _) | CShape::Private(_, obj) => obj,
                    CShape::Super(m) => {
                        // `super.name(...)`: the function and `this` are known now. Only the arguments wait.
                        let this = self.this_dyn()?;
                        let key = self.super_key(m)?;
                        let f = self.super_get(this, key)?;
                        self.spush(run, Slot::V(f))?;
                        self.spush(run, Slot::V(this))?;
                        return self.args_next(run, e, base, 1, 0);
                    }
                };
                self.ask(run, e, 0, base, first)
            }
            ExprKind::New { callee, .. } => self.ask(run, e, 0, base, callee),
            ExprKind::Assign { target, .. } => match prog.pat(target).kind {
                PatKind::Member(me) => {
                    let m = self.strip(me);
                    let obj = match prog.expr(m).kind {
                        ExprKind::Member { obj, .. } | ExprKind::Index { obj, .. } if matches!(prog.expr(obj).kind, ExprKind::Super) => {
                            // `super.x = ...`: the receiver and the key are known now. Only the value waits.
                            let this = self.this_dyn()?;
                            let key = self.super_key(m)?;
                            self.spush(run, Slot::V(this))?;
                            self.spush(run, Slot::K(key))?;
                            return self.assign_ready(run, e, base);
                        }
                        ExprKind::Member { obj, .. } | ExprKind::Index { obj, .. } | ExprKind::PrivateMember { obj, .. } => obj,
                        _ => return Err(unsupported("assignment target", ex.span)),
                    };
                    self.ask(run, e, 0, base, obj)
                }
                _ => self.assign_ready(run, e, base),
            },
            _ => Err(unsupported("await in this expression", ex.span)),
        }
    }

    fn callee_shape(&self, callee: ExprId) -> CShape {
        let c = self.strip(callee);
        match self.prog.expr(c).kind {
            ExprKind::Member { obj, .. } if matches!(self.prog.expr(obj).kind, ExprKind::Super) => CShape::Super(c),
            ExprKind::Member { obj, .. } => CShape::Member(c, obj),
            ExprKind::Index { obj, index, .. } => CShape::Index(obj, index),
            ExprKind::PrivateMember { obj, .. } => CShape::Private(c, obj),
            _ => CShape::Plain,
        }
    }

    /// `Await(v)`. For a value that is not an object, `PromiseResolve` makes a promise that no script can see and
    /// that is already fulfilled, so the machine skips it and queues the resume job at once, in the same order.
    fn await_value(&mut self, v: Value) -> Res<Step> {
        if !matches!(v, Value::Object(_)) {
            return Ok(Step::Out(Out::Ready(v)));
        }
        match self.promise_resolve(v)? {
            Value::Object(p) => Ok(Step::Out(Out::Wait(p))),
            _ => Err(unsupported("await", Span::default())),
        }
    }

    fn arr_next(&mut self, run: &mut Run, e: ExprId, base: u32, from: u32) -> Res<Step> {
        let ExprKind::Array(l) = self.prog.expr(e).kind else { return Err(unsupported("array", Span::default())) };
        let b = base as usize;
        let mut j = from as usize;
        while j < l.len() {
            let x = self.list_at(l, j);
            match self.prog.expr(x).kind {
                ExprKind::Hole => {
                    let Slot::N(n) = run.slots[b + 1] else { return Err(unsupported("array", Span::default())) };
                    run.slots[b + 1] = Slot::N(n + 1);
                    j += 1;
                }
                ExprKind::Spread(inner) => return self.ask(run, e, j as u32, base, inner),
                _ => return self.ask(run, e, j as u32, base, x),
            }
        }
        let (Value::Object(a), Slot::N(n)) = (sval(run, b), run.slots[b + 1]) else { return Err(unsupported("array", Span::default())) };
        h!(self, self.heap.set(a, self.k_length, Value::Number(n as f64)));
        self.strunc(run, b);
        val(Value::Object(a))
    }

    fn obj_next(&mut self, run: &mut Run, e: ExprId, base: u32, from: u32) -> Res<Step> {
        let ExprKind::Object(l) = self.prog.expr(e).kind else { return Err(unsupported("object", Span::default())) };
        let b = base as usize;
        let Value::Object(o) = sval(run, b) else { return Err(unsupported("object", Span::default())) };
        for j in from as usize..l.len() {
            let pid = self.list_at(l, j);
            let p = *self.prog.prop(pid);
            if matches!(p.kind, PropKind::Init | PropKind::Shorthand | PropKind::Spread) && self.aw_expr(p.value) {
                return self.ask(run, e, j as u32, base, p.value);
            }
            self.object_prop(o, pid, None)?;
        }
        self.strunc(run, b);
        val(Value::Object(o))
    }

    fn args_next(&mut self, run: &mut Run, e: ExprId, base: u32, np: u32, k: u32) -> Res<Step> {
        let (ExprKind::Call { args, .. } | ExprKind::New { args, .. }) = self.prog.expr(e).kind else { return Err(unsupported("call", Span::default())) };
        if (k as usize) < args.len() {
            let x = self.list_at(args, k as usize);
            let x = if let ExprKind::Spread(inner) = self.prog.expr(x).kind { inner } else { x };
            return self.ask(run, e, np + k, base, x);
        }
        let b = base as usize;
        let is_new = matches!(self.prog.expr(e).kind, ExprKind::New { .. });
        let (f, this, from) = if is_new { (sval(run, b), Value::Undefined, b + 1) } else { (sval(run, b), sval(run, b + 1), b + 2) };
        let argv: Vec<Value> = (from..run.slots.len()).map(|i| sval(run, i)).collect();
        self.strunc(run, b);
        let (ExprKind::Call { callee, .. } | ExprKind::New { callee, .. }) = self.prog.expr(e).kind else { return Err(unsupported("call", Span::default())) };
        self.mark_call(e);
        let r = if is_new {
            self.construct(f, &argv, callee)
        } else if self.is_callable(f) {
            self.call_fn(f, this, &argv)
        } else {
            let m = format!("{} is not a function", self.src_text(callee));
            Err(self.type_err(&m))
        };
        self.stk.pos = 0;
        val(r?)
    }

    fn nullish_base_err(&mut self, base: Value, iv: Value) -> Error {
        let t = self.display(base);
        let ks = self.coerce_string(iv).map(|s| self.heap.to_string_lossy(s).unwrap_or_default()).unwrap_or_default();
        self.type_err(&format!("Cannot read properties of {t} (reading '{ks}')"))
    }

    /// The reference of an assignment target is ready (its member parts, if any, are in the slots from `base`).
    fn assign_ready(&mut self, run: &mut Run, e: ExprId, base: u32) -> Res<Step> {
        let ExprKind::Assign { op, target, value } = self.prog.expr(e).kind else { return Err(unsupported("assignment", Span::default())) };
        let (np, member) = self.assign_parts(target);
        let b = base as usize;
        match op {
            AssignOp::Assign => self.ask(run, e, np, base, value),
            AssignOp::Compound(_) | AssignOp::Logical(_) => {
                let r = match self.prog.pat(target).kind {
                    PatKind::Ident => Ref::Name(self.pat_name[target.index()], self.pat_site(target.index())),
                    PatKind::Member(_) if member => self.assign_ref(run, target, b)?,
                    _ => return Err(unsupported("assignment target", self.prog.pat(target).span)),
                };
                let old = self.ref_get(&r)?;
                if let AssignOp::Logical(l) = op {
                    let short = match l {
                        LogicalOp::And => !self.truthy(old)?,
                        LogicalOp::Or => self.truthy(old)?,
                        LogicalOp::Nullish => !is_nullish(old),
                    };
                    if short {
                        self.strunc(run, b);
                        return val(old);
                    }
                }
                self.spush(run, Slot::V(old))?;
                self.ask(run, e, np, base, value)
            }
        }
    }

    /// (number of member operands before the value, is a member target)
    fn assign_parts(&self, target: PatId) -> (u32, bool) {
        match self.prog.pat(target).kind {
            PatKind::Member(me) => match self.prog.expr(self.strip(me)).kind {
                // a super property keeps the receiver and the key in two slots, like a name
                ExprKind::Member { obj, .. } | ExprKind::Index { obj, .. } if matches!(self.prog.expr(obj).kind, ExprKind::Super) => (1, true),
                ExprKind::Index { .. } => (2, true),
                _ => (1, true),
            },
            _ => (0, false),
        }
    }

    /// The member reference of an assignment target whose receiver and key are in the slots at `b`.
    fn assign_ref(&mut self, run: &Run, target: PatId, b: usize) -> Res<Ref> {
        let PatKind::Member(me) = self.prog.pat(target).kind else { return Err(unsupported("assignment target", self.prog.pat(target).span)) };
        let m = self.strip(me);
        Ok(match self.prog.expr(m).kind {
            ExprKind::PrivateMember { .. } => {
                let (pn, n, fl) = self.priv_lookup(m)?;
                Ref::Priv(sval(run, b), pn, n, fl)
            }
            ExprKind::Member { obj, .. } | ExprKind::Index { obj, .. } if matches!(self.prog.expr(obj).kind, ExprKind::Super) => Ref::Super(sval(run, b), skey(run, b + 1)),
            _ => Ref::Prop(sval(run, b), skey(run, b + 1)),
        })
    }

    fn op_value(&mut self, run: &mut Run, e: ExprId, i: u32, base: u32, v: Value) -> Res<Step> {
        let prog = self.prog.clone();
        let ex = *prog.expr(e);
        let b = base as usize;
        match ex.kind {
            ExprKind::Await(_) => self.await_value(v),
            ExprKind::Yield { delegate: false, .. } if run.agen => {
                self.fpush(run, Fr::AYield)?;
                self.await_value(v)
            }
            ExprKind::Yield { delegate: false, .. } => Ok(Step::Out(Out::Yield(v, false))),
            ExprKind::Yield { delegate: true, .. } if run.agen => {
                let it = self.aiter_open(v)?;
                self.spush(run, Slot::It(it))?;
                self.adeleg_send(run, base, GenHow::Next(Value::Undefined))
            }
            ExprKind::Yield { delegate: true, .. } => {
                let it = self.iter_start_val(v)?;
                self.spush(run, Slot::It(it))?;
                self.deleg_step(run, base, GenHow::Next(Value::Undefined))
            }
            ExprKind::Unary { op, .. } => {
                let r = match op {
                    UnaryOp::Typeof => {
                        let t = self.type_of(v);
                        Value::String(h!(self, self.heap.intern_str(t)))
                    }
                    UnaryOp::Void => Value::Undefined,
                    UnaryOp::Not => Value::Bool(!self.truthy(v)?),
                    UnaryOp::Neg => Value::Number(-self.coerce_number(v)?),
                    UnaryOp::Plus => Value::Number(self.coerce_number(v)?),
                    UnaryOp::BitNot => Value::Number(!to_int32(self.coerce_number(v)?) as f64),
                    UnaryOp::Delete => return Err(unsupported("await inside delete", ex.span)),
                };
                val(r)
            }
            ExprKind::Binary { op, right, .. } => {
                if i == 0 {
                    self.spush(run, Slot::V(v))?;
                    self.ask(run, e, 1, base, right)
                } else {
                    let a = sval(run, b);
                    self.strunc(run, b);
                    val(self.binary(op, a, v)?)
                }
            }
            ExprKind::Logical { op, right, .. } => {
                let short = match op {
                    LogicalOp::And => !self.truthy(v)?,
                    LogicalOp::Or => self.truthy(v)?,
                    LogicalOp::Nullish => !is_nullish(v),
                };
                if short { val(v) } else { nx(Next::Eval(right)) }
            }
            ExprKind::Cond { cons, alt, .. } => nx(Next::Eval(if self.truthy(v)? { cons } else { alt })),
            ExprKind::Seq(l) => {
                if (i as usize) + 1 < l.len() {
                    let x = self.list_at(l, i as usize + 1);
                    self.ask(run, e, i + 1, base, x)
                } else {
                    val(v)
                }
            }
            ExprKind::Template(t) => {
                let t = *prog.template(t);
                let s = self.coerce_string(v)?;
                self.spush(run, Slot::V(Value::String(s)))?;
                let n = t.n_exprs as usize;
                if (i as usize) + 1 < n {
                    let x = prog.template_exprs(&t).nth(i as usize + 1).ok_or_else(|| unsupported("template", ex.span))?;
                    return self.ask(run, e, i + 1, base, x);
                }
                let mut out: Vec<u16> = Vec::new();
                for (k, q) in prog.template_quasis(&t).enumerate() {
                    let _ = prog.quasi(prog.quasi_node(q), false, &mut out);
                    if k < n
                        && let Value::String(s) = sval(run, b + k)
                    {
                        self.append_string(&mut out, h!(self, self.heap.str_units(s)))?;
                    }
                }
                self.strunc(run, b);
                val(Value::String(h!(self, self.heap.intern_vec(out))))
            }
            ExprKind::Array(l) => {
                let (Value::Object(a), Slot::N(mut n)) = (sval(run, b), run.slots[b + 1]) else { return Err(unsupported("array", ex.span)) };
                let x = self.list_at(l, i as usize);
                if let ExprKind::Spread(inner) = prog.expr(x).kind {
                    let mut it = self.iter_start(v, inner)?;
                    while let Some(item) = self.iter_next(&mut it)? {
                        self.def(a, PropertyKey::Index(n), item, true, true, true)?;
                        n += 1;
                    }
                } else {
                    self.def(a, PropertyKey::Index(n), v, true, true, true)?;
                    n += 1;
                }
                run.slots[b + 1] = Slot::N(n);
                self.arr_next(run, e, base, i + 1)
            }
            ExprKind::Object(l) => {
                let Value::Object(o) = sval(run, b) else { return Err(unsupported("object", ex.span)) };
                let pid = self.list_at(l, i as usize);
                self.object_prop(o, pid, Some(v))?;
                self.obj_next(run, e, base, i + 1)
            }
            ExprKind::Member { name, .. } => {
                let key = self.expr_key(e, name)?;
                val(self.get_prop(v, key)?)
            }
            ExprKind::PrivateMember { .. } => {
                let (pn, n, fl) = self.priv_lookup(e)?;
                val(self.priv_get(v, pn, n, fl)?)
            }
            ExprKind::PrivateIn { .. } => {
                let (pn, n, _) = self.priv_lookup(e)?;
                let Value::Object(o) = v else {
                    let t = self.display(v);
                    let name = self.names[n as usize].clone();
                    return Err(self.type_err(&format!("Cannot use 'in' operator to search for '{name}' in {t}")));
                };
                val(Value::Bool(self.priv_slot(o, pn).is_some()))
            }
            ExprKind::Index { index, .. } => {
                if i == 0 {
                    self.spush(run, Slot::V(v))?;
                    self.ask(run, e, 1, base, index)
                } else {
                    let bv = sval(run, b);
                    self.strunc(run, b);
                    if is_nullish(bv) {
                        return Err(self.nullish_base_err(bv, v));
                    }
                    let key = self.coerce_key(v)?;
                    val(self.get_prop(bv, key)?)
                }
            }
            ExprKind::Call { callee, args, .. } => {
                let shape = self.callee_shape(callee);
                let np = if matches!(shape, CShape::Index(..)) { 2 } else { 1 };
                if i < np {
                    match shape {
                        CShape::Plain => {
                            self.spush(run, Slot::V(v))?;
                            self.spush(run, Slot::V(Value::Undefined))?;
                        }
                        CShape::Super(_) => return Err(unsupported("super call state", ex.span)),
                        CShape::Private(m, _) => {
                            let (pn, n, fl) = self.priv_lookup(m)?;
                            let f = self.priv_get(v, pn, n, fl)?;
                            self.spush(run, Slot::V(f))?;
                            self.spush(run, Slot::V(v))?;
                        }
                        CShape::Member(m, _) => {
                            let ExprKind::Member { name, .. } = prog.expr(m).kind else { return Err(unsupported("call", ex.span)) };
                            let key = self.expr_key(m, name)?;
                            let f = self.get_prop(v, key)?;
                            self.spush(run, Slot::V(f))?;
                            self.spush(run, Slot::V(v))?;
                        }
                        CShape::Index(_, index) => {
                            if i == 0 {
                                self.spush(run, Slot::V(v))?;
                                return self.ask(run, e, 1, base, index);
                            }
                            let bv = sval(run, b);
                            if is_nullish(bv) {
                                self.strunc(run, b);
                                return Err(self.nullish_base_err(bv, v));
                            }
                            let key = self.coerce_key(v)?;
                            let f = self.get_prop(bv, key)?;
                            run.slots[b] = Slot::V(f);
                            self.spush(run, Slot::V(bv))?;
                        }
                    }
                    return self.args_next(run, e, base, np, 0);
                }
                self.arg_value(run, e, args, base, np, i - np, v)
            }
            ExprKind::New { args, .. } => {
                if i == 0 {
                    self.spush(run, Slot::V(v))?;
                    return self.args_next(run, e, base, 1, 0);
                }
                self.arg_value(run, e, args, base, 1, i - 1, v)
            }
            ExprKind::Assign { op, target, .. } => {
                let (np, member) = self.assign_parts(target);
                if i < np {
                    let PatKind::Member(me) = prog.pat(target).kind else { return Err(unsupported("assignment target", ex.span)) };
                    let m = self.strip(me);
                    match prog.expr(m).kind {
                        ExprKind::Member { name, .. } => {
                            let key = self.expr_key(m, name)?;
                            self.spush(run, Slot::V(v))?;
                            self.spush(run, Slot::K(key))?;
                        }
                        ExprKind::Index { index, .. } => {
                            if i == 0 {
                                self.spush(run, Slot::V(v))?;
                                return self.ask(run, e, 1, base, index);
                            }
                            let bv = sval(run, b);
                            if is_nullish(bv) {
                                self.strunc(run, b);
                                return Err(self.nullish_base_err(bv, v));
                            }
                            let key = self.coerce_key(v)?;
                            self.spush(run, Slot::K(key))?;
                        }
                        ExprKind::PrivateMember { .. } => {
                            self.spush(run, Slot::V(v))?;
                            self.spush(run, Slot::N(0))?;
                        }
                        _ => return Err(unsupported("assignment target", ex.span)),
                    }
                    return self.assign_ready(run, e, base);
                }
                // the value arrived
                if matches!(prog.pat(target).kind, PatKind::Array(_) | PatKind::Object(_)) {
                    self.bind_pattern(target, v, Bind::Assign)?;
                    self.strunc(run, b);
                    return val(v);
                }
                let r = if member { self.assign_ref(run, target, b)? } else { Ref::Name(self.pat_name[target.index()], self.pat_site(target.index())) };
                let res = match op {
                    AssignOp::Compound(bop) => {
                        let old = sval(run, b + if member { 2 } else { 0 });
                        self.binary(bop, old, v)?
                    }
                    _ => v,
                };
                self.ref_put(&r, res)?;
                self.strunc(run, b);
                val(res)
            }
            _ => Err(unsupported("await in this expression", ex.span)),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn arg_value(&mut self, run: &mut Run, e: ExprId, args: List<Expr>, base: u32, np: u32, k: u32, v: Value) -> Res<Step> {
        let x = self.list_at(args, k as usize);
        if let ExprKind::Spread(inner) = self.prog.expr(x).kind {
            let mut it = self.iter_start(v, inner)?;
            while let Some(item) = self.iter_next(&mut it)? {
                self.spush(run, Slot::V(item))?;
            }
        } else {
            self.spush(run, Slot::V(v))?;
        }
        self.args_next(run, e, base, np, k + 1)
    }

    // ---- statements

    fn exec_node(&mut self, run: &mut Run, s: StmtId) -> Res<Step> {
        if !self.aw_stmt(s) {
            return match self.exec(s) {
                Ok(()) => val(Value::Undefined),
                Err(c) => self.ctl_step(c),
            };
        }
        let prog = self.prog.clone();
        let st = *prog.stmt(s);
        match st.kind {
            StmtKind::Expr(e) => {
                self.fpush(run, Fr::Discard)?;
                nx(Next::Eval(e))
            }
            StmtKind::Return(Some(e)) => {
                self.fpush(run, Fr::Ret)?;
                nx(Next::Eval(e))
            }
            StmtKind::Throw(e) => {
                self.fpush(run, Fr::Thr)?;
                nx(Next::Eval(e))
            }
            StmtKind::Var { .. } => self.var_from(run, s, 0),
            StmtKind::If { test, .. } => {
                self.fpush(run, Fr::IfT { s })?;
                nx(Next::Eval(test))
            }
            StmtKind::Block(l) => {
                if self.has_lexical(l) {
                    let saved = self.env;
                    let env = self.new_env(saved, false)?;
                    self.env = env;
                    self.fpush(run, Fr::Scope { saved, env })?;
                    self.declare_scope(l, false)?;
                }
                self.list_from(run, l, 0)
            }
            StmtKind::While { .. } | StmtKind::DoWhile { .. } | StmtKind::For { .. } | StmtKind::ForOf { .. } => self.start_loop(run, s, Lbl::default()),
            StmtKind::Labeled { .. } => {
                let mut lb = Lbl::default();
                let mut cur = s;
                while let StmtKind::Labeled { body, .. } = prog.stmt(cur).kind {
                    if lb.n as usize >= lb.l.len() {
                        return Err(unsupported("more than three labels", st.span));
                    }
                    lb.l[lb.n as usize] = self.stmt_label[cur.index()];
                    lb.n += 1;
                    cur = body;
                }
                if matches!(prog.stmt(cur).kind, StmtKind::While { .. } | StmtKind::DoWhile { .. } | StmtKind::For { .. } | StmtKind::ForOf { .. }) {
                    return self.start_loop(run, cur, lb);
                }
                for k in 0..lb.n as usize {
                    self.fpush(run, Fr::Lab { label: lb.l[k] })?;
                }
                nx(Next::Exec(cur))
            }
            StmtKind::Try { block, .. } => {
                self.fpush(run, Fr::TryB { s, env: self.env })?;
                nx(Next::Exec(block))
            }
            StmtKind::Switch { disc, .. } => {
                self.fpush(run, Fr::SwD { s })?;
                nx(Next::Eval(disc))
            }
            _ => Err(unsupported("await in this statement", st.span)),
        }
    }

    /// Run the statements of `l` from index `i`. Await-free statements run inline on the ordinary evaluator.
    fn list_from(&mut self, run: &mut Run, l: List<Stmt>, mut i: usize) -> Res<Step> {
        while i < l.len() {
            let s = self.list_at(l, i);
            i += 1;
            if self.aw_stmt(s) {
                if i < l.len() {
                    self.fpush(run, Fr::List { l, i: i as u32 })?;
                }
                return nx(Next::Exec(s));
            }
            if let Err(c) = self.exec(s) {
                return self.ctl_step(c);
            }
        }
        val(Value::Undefined)
    }

    /// Compare the discriminant (slot `base`) with the case tests of switch `s` from case `i`, as in
    /// `eval_statements.rs`. Await-free tests run inline. No match: the `default` clause, or nothing.
    /// The discriminant `d` goes into slot `base` only before a test that awaits, and leaves it when a body starts.
    fn sw_test(&mut self, run: &mut Run, s: StmtId, base: u32, d: Value, mut i: usize) -> Res<Step> {
        let StmtKind::Switch { cases, .. } = self.prog.stmt(s).kind else { return Err(unsupported("switch", Span::default())) };
        while i < cases.len() {
            if let Some(t) = self.prog.case(self.list_at(cases, i)).test {
                if self.aw_expr(t) {
                    if run.slots.len() == base as usize {
                        self.spush(run, Slot::V(d))?;
                    }
                    self.fpush(run, Fr::SwT { s, base, i: i as u32 })?;
                    return nx(Next::Eval(t));
                }
                if d.strict_equals(self.eval_expr(t)?) {
                    self.strunc(run, base as usize);
                    return self.sw_body(run, s, i);
                }
            }
            i += 1;
        }
        self.strunc(run, base as usize);
        match (0..cases.len()).find(|&k| self.prog.case(self.list_at(cases, k)).test.is_none()) {
            Some(k) => self.sw_body(run, s, k),
            None => val(Value::Undefined),
        }
    }

    /// Run the body of case `i` of switch `s`, then fall through to the next case.
    fn sw_body(&mut self, run: &mut Run, s: StmtId, i: usize) -> Res<Step> {
        let StmtKind::Switch { cases, .. } = self.prog.stmt(s).kind else { return Err(unsupported("switch", Span::default())) };
        if i >= cases.len() {
            return val(Value::Undefined);
        }
        self.fpush(run, Fr::SwB { s, i: i as u32 })?;
        self.list_from(run, self.prog.case(self.list_at(cases, i)).body, 0)
    }

    fn var_from(&mut self, run: &mut Run, s: StmtId, from: usize) -> Res<Step> {
        let prog = self.prog.clone();
        let StmtKind::Var { kind, decls } = prog.stmt(s).kind else { return Err(unsupported("var", Span::default())) };
        for k in from..decls.len() {
            let d = *prog.decl(self.list_at(decls, k));
            if let Some(i) = d.init
                && self.aw_expr(i)
            {
                self.fpush(run, Fr::VarD { s, d: k as u32 })?;
                return nx(Next::Eval(i));
            }
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
        val(Value::Undefined)
    }

    fn start_loop(&mut self, run: &mut Run, s: StmtId, lb: Lbl) -> Res<Step> {
        let prog = self.prog.clone();
        match prog.stmt(s).kind {
            StmtKind::While { .. } => self.while_iter(run, s, lb),
            StmtKind::DoWhile { .. } => self.do_iter(run, s, lb),
            StmtKind::For { init, .. } => {
                self.fpush(run, Fr::ForScope { saved: self.env })?;
                let mut per = false;
                match init {
                    ForInit::None => self.for_iter(run, s, lb, false),
                    ForInit::Expr(e) => {
                        self.fpush(run, Fr::ForInit { s, lb, per })?;
                        nx(Next::Eval(e))
                    }
                    ForInit::Var(v) => {
                        if let StmtKind::Var { kind, decls } = prog.stmt(v).kind
                            && kind != VarKind::Var
                        {
                            per = true;
                            let env = self.new_env(self.env, false)?;
                            self.env = env;
                            for d in prog.list(decls) {
                                self.declare_pat_lexical(prog.decl(d).target, if kind == VarKind::Let { MUT } else { 0 });
                            }
                        }
                        self.fpush(run, Fr::ForInit { s, lb, per })?;
                        nx(Next::Exec(v))
                    }
                }
            }
            StmtKind::ForOf { right, .. } => {
                self.fpush(run, Fr::OfR { s, lb })?;
                nx(Next::Eval(right))
            }
            _ => Err(unsupported("loop", Span::default())),
        }
    }

    fn while_iter(&mut self, run: &mut Run, s: StmtId, lb: Lbl) -> Res<Step> {
        let StmtKind::While { test, .. } = self.prog.stmt(s).kind else { return Err(unsupported("loop", Span::default())) };
        self.tick()?;
        self.fpush(run, Fr::WhileT { s, lb })?;
        nx(Next::Eval(test))
    }

    fn do_iter(&mut self, run: &mut Run, s: StmtId, lb: Lbl) -> Res<Step> {
        let StmtKind::DoWhile { body, .. } = self.prog.stmt(s).kind else { return Err(unsupported("loop", Span::default())) };
        self.tick()?;
        self.fpush(run, Fr::DoB { s, lb })?;
        nx(Next::Exec(body))
    }

    fn for_iter(&mut self, run: &mut Run, s: StmtId, lb: Lbl, per: bool) -> Res<Step> {
        let StmtKind::For { test, body, .. } = self.prog.stmt(s).kind else { return Err(unsupported("loop", Span::default())) };
        if per && self.envs[self.env as usize].captured {
            self.copy_env()?;
        }
        self.tick()?;
        match test {
            Some(t) => {
                self.fpush(run, Fr::ForT { s, lb, per })?;
                nx(Next::Eval(t))
            }
            None => {
                self.fpush(run, Fr::ForB { s, lb, per })?;
                nx(Next::Exec(body))
            }
        }
    }

    fn for_after_body(&mut self, run: &mut Run, s: StmtId, lb: Lbl, per: bool) -> Res<Step> {
        let StmtKind::For { update, .. } = self.prog.stmt(s).kind else { return Err(unsupported("loop", Span::default())) };
        if per && self.envs[self.env as usize].captured {
            self.copy_env()?;
        }
        match update {
            Some(u) => {
                self.fpush(run, Fr::ForU { s, lb, per })?;
                nx(Next::Eval(u))
            }
            None => self.for_iter(run, s, lb, per),
        }
    }

    /// The binding target of a for-of head and its `let`/`const` kind.
    fn of_target(&self, s: StmtId) -> Res<(PatId, Option<VarKind>, ExprId)> {
        let StmtKind::ForOf { left, right, .. } = self.prog.stmt(s).kind else { return Err(unsupported("for-of", Span::default())) };
        match left {
            ForLeft::Pat(p) => Ok((p, None, right)),
            ForLeft::Var(v) => {
                let StmtKind::Var { kind, decls } = self.prog.stmt(v).kind else { return Err(unsupported("for left", Span::default())) };
                let Some(d) = self.prog.list(decls).next().map(|d| *self.prog.decl(d)) else { return Err(unsupported("for left", Span::default())) };
                Ok((d.target, if kind == VarKind::Var { None } else { Some(kind) }, right))
            }
        }
    }

    fn of_iter(&mut self, run: &mut Run, s: StmtId, lb: Lbl, base: u32) -> Res<Step> {
        let Slot::It(mut it) = run.slots[base as usize] else { return Err(unsupported("for-of", Span::default())) };
        // `iter_next` marks the iteration done after the end or an error, so a throw from here is never closed.
        let step = self.iter_next(&mut it);
        run.slots[base as usize] = Slot::It(it);
        let Some(v) = step? else { return val(Value::Undefined) };
        self.of_body(run, s, v, Fr::OfB { s, lb, base }, false)
    }

    /// Bind the value `v` of a for-of iteration and run the body under the frame `fr`. `early`: push `fr` before the
    /// binding, so a binding error leaves the loop through it.
    fn of_body(&mut self, run: &mut Run, s: StmtId, v: Value, fr: Fr, early: bool) -> Res<Step> {
        let StmtKind::ForOf { body, .. } = self.prog.stmt(s).kind else { return Err(unsupported("for-of", Span::default())) };
        let (target, lexical, _) = self.of_target(s)?;
        let Some(&Fr::OfScope { outer, cur, .. }) = run.stack.last() else { return Err(unsupported("for-of", Span::default())) };
        if let Some(kind) = lexical {
            if cur != NONE {
                self.env = outer;
                self.release(cur);
                if let Some(Fr::OfScope { cur, .. }) = run.stack.last_mut() {
                    *cur = NONE;
                }
            }
            let e = self.new_env(outer, false)?;
            if let Some(Fr::OfScope { cur, .. }) = run.stack.last_mut() {
                *cur = e;
            }
            self.env = e;
            self.declare_pat_lexical(target, if kind == VarKind::Let { MUT } else { 0 });
        }
        if early {
            self.fpush(run, fr)?;
        }
        self.bind_pattern(target, v, if lexical.is_some() { Bind::Init } else { Bind::Assign })?;
        if !early {
            self.fpush(run, fr)?;
        }
        nx(Next::Exec(body))
    }

    /// A finally block starts. `abr` is the completion it must keep (`None`: normal).
    fn fin_start(&mut self, run: &mut Run, s: StmtId, env: u32, abr: Option<Abr>) -> Res<Step> {
        let StmtKind::Try { finalizer: Some(f), .. } = self.prog.stmt(s).kind else { return Err(unsupported("try", Span::default())) };
        self.env = env;
        let base = run.slots.len() as u32;
        let (kind, label) = match abr {
            None => (0, 0),
            Some(Abr::Break(l)) => (1, l),
            Some(Abr::Continue(l)) => (2, l),
            Some(Abr::Return(v)) => {
                self.spush(run, Slot::V(v))?;
                (3, 0)
            }
            Some(Abr::Throw(v)) => {
                self.spush(run, Slot::V(v))?;
                (4, 0)
            }
        };
        self.fpush(run, Fr::Fin { kind, label, base })?;
        nx(Next::Exec(f))
    }

    /// A normal completion arrives at the top frame.
    fn deliver(&mut self, run: &mut Run, v: Value) -> Res<Step> {
        let Some(fr) = self.fpop(run) else { return Err(unsupported("async machine", Span::default())) };
        match fr {
            Fr::Body => Ok(Step::Out(Out::Done(Value::Undefined))),
            Fr::BodyExpr => Ok(Step::Out(Out::Done(v))),
            Fr::Op { e, i, base } => match self.op_value(run, e, i, base, v) {
                Err(x) => {
                    self.strunc(run, base as usize);
                    Err(x)
                }
                r => r,
            },
            Fr::Discard => val(Value::Undefined),
            Fr::Deleg { base } if run.agen => self.adeleg_send(run, base, GenHow::Next(v)),
            Fr::Deleg { base } => self.deleg_step(run, base, GenHow::Next(v)),
            Fr::Ret if run.agen => {
                self.fpush(run, Fr::ARet)?;
                self.await_value(v)
            }
            Fr::Ret => nx(Next::Abr(Abr::Return(v))),
            Fr::AYield => Ok(Step::Out(Out::Yield(v, false))),
            Fr::ARet => nx(Next::Abr(Abr::Return(v))),
            Fr::ADeleg { base, ret } => self.adeleg_result(run, base, ret, v),
            Fr::ADelegClose { base } => {
                self.strunc(run, base as usize);
                if !matches!(v, Value::Object(_)) {
                    let t = self.display(v);
                    return Err(self.type_err(&format!("Iterator result {t} is not an object")));
                }
                Err(self.type_err("The iterator does not provide a 'throw' method"))
            }
            Fr::AOfN { s, lb, base } => self.aof_result(run, s, lb, base, v),
            Fr::AOfB { s, lb, base } => self.aof_next(run, s, lb, base),
            Fr::AClose { kind, label, base } => {
                let held = if kind >= 3 { sval(run, base as usize) } else { Value::Undefined };
                self.strunc(run, base as usize);
                if kind == 4 {
                    return nx(Next::Abr(Abr::Throw(held)));
                }
                if !matches!(v, Value::Object(_)) {
                    let t = self.display(v);
                    return Err(self.type_err(&format!("Iterator result {t} is not an object")));
                }
                match kind {
                    0 => val(Value::Undefined),
                    1 => nx(Next::Abr(Abr::Break(label))),
                    2 => nx(Next::Abr(Abr::Continue(label))),
                    _ => nx(Next::Abr(Abr::Return(held))),
                }
            }
            Fr::Thr => nx(Next::Abr(Abr::Throw(v))),
            Fr::List { l, i } => self.list_from(run, l, i as usize),
            Fr::Scope { saved, env } => {
                self.env = saved;
                self.release(env);
                val(Value::Undefined)
            }
            Fr::IfT { s } => {
                let StmtKind::If { cons, alt, .. } = self.prog.stmt(s).kind else { return Err(unsupported("if", Span::default())) };
                if self.truthy(v)? {
                    nx(Next::Exec(cons))
                } else if let Some(a) = alt {
                    nx(Next::Exec(a))
                } else {
                    val(Value::Undefined)
                }
            }
            Fr::VarD { s, d } => {
                let StmtKind::Var { kind, decls } = self.prog.stmt(s).kind else { return Err(unsupported("var", Span::default())) };
                let decl = *self.prog.decl(self.list_at(decls, d as usize));
                self.bind_pattern(decl.target, v, if kind == VarKind::Var { Bind::Assign } else { Bind::Init })?;
                self.var_from(run, s, d as usize + 1)
            }
            Fr::Lab { .. } => val(Value::Undefined),
            Fr::WhileT { s, lb } => {
                let StmtKind::While { body, .. } = self.prog.stmt(s).kind else { return Err(unsupported("loop", Span::default())) };
                if !self.truthy(v)? {
                    return val(Value::Undefined);
                }
                self.fpush(run, Fr::WhileB { s, lb })?;
                nx(Next::Exec(body))
            }
            Fr::WhileB { s, lb } => self.while_iter(run, s, lb),
            Fr::DoB { s, lb } => {
                let StmtKind::DoWhile { test, .. } = self.prog.stmt(s).kind else { return Err(unsupported("loop", Span::default())) };
                self.fpush(run, Fr::DoT { s, lb })?;
                nx(Next::Eval(test))
            }
            Fr::DoT { s, lb } => {
                if self.truthy(v)? { self.do_iter(run, s, lb) } else { val(Value::Undefined) }
            }
            Fr::ForScope { saved } => {
                let cur = self.env;
                self.env = saved;
                if cur != saved {
                    self.release(cur);
                }
                val(Value::Undefined)
            }
            Fr::ForInit { s, lb, per } | Fr::ForU { s, lb, per } => self.for_iter(run, s, lb, per),
            Fr::ForT { s, lb, per } => {
                let StmtKind::For { body, .. } = self.prog.stmt(s).kind else { return Err(unsupported("loop", Span::default())) };
                if !self.truthy(v)? {
                    return val(Value::Undefined);
                }
                self.fpush(run, Fr::ForB { s, lb, per })?;
                nx(Next::Exec(body))
            }
            Fr::ForB { s, lb, per } => self.for_after_body(run, s, lb, per),
            Fr::OfR { s, lb } => {
                let (_, _, right) = self.of_target(s)?;
                let outer = self.env;
                let base = run.slots.len() as u32;
                let is_await = matches!(self.prog.stmt(s).kind, StmtKind::ForOf { is_await: true, .. });
                let it = if is_await { self.aiter_open(v)? } else { self.iter_start(v, right)? };
                self.spush(run, Slot::It(it))?;
                self.fpush(run, Fr::OfScope { outer, cur: NONE, base })?;
                if is_await { self.aof_next(run, s, lb, base) } else { self.of_iter(run, s, lb, base) }
            }
            Fr::OfScope { outer, cur, base } => {
                self.env = outer;
                if cur != NONE {
                    self.release(cur);
                }
                self.strunc(run, base as usize);
                val(Value::Undefined)
            }
            Fr::OfB { s, lb, base } => self.of_iter(run, s, lb, base),
            Fr::TryB { s, env } | Fr::TryC { s, env } => {
                let StmtKind::Try { finalizer, .. } = self.prog.stmt(s).kind else { return Err(unsupported("try", Span::default())) };
                if finalizer.is_some() { self.fin_start(run, s, env, None) } else { val(Value::Undefined) }
            }
            Fr::SwD { s } => {
                let StmtKind::Switch { cases, .. } = self.prog.stmt(s).kind else { return Err(unsupported("switch", Span::default())) };
                let base = run.slots.len() as u32;
                self.fpush(run, Fr::SwS { base })?;
                let prog = self.prog.clone();
                if prog.list(cases).any(|c| self.has_lexical(prog.case(c).body)) {
                    let saved = self.env;
                    let env = self.new_env(saved, false)?;
                    self.env = env;
                    self.fpush(run, Fr::Scope { saved, env })?;
                    for c in prog.list(cases) {
                        self.declare_scope(prog.case(c).body, false)?;
                    }
                }
                self.sw_test(run, s, base, v, 0)
            }
            Fr::SwS { base } => {
                self.strunc(run, base as usize);
                val(Value::Undefined)
            }
            Fr::SwT { s, base, i } => {
                let d = sval(run, base as usize);
                if d.strict_equals(v) {
                    self.strunc(run, base as usize);
                    self.sw_body(run, s, i as usize)
                } else {
                    self.sw_test(run, s, base, d, i as usize + 1)
                }
            }
            Fr::SwB { s, i } => self.sw_body(run, s, i as usize + 1),
            Fr::Fin { kind, label, base } => {
                let held = if kind >= 3 { sval(run, base as usize) } else { Value::Undefined };
                self.strunc(run, base as usize);
                match kind {
                    0 => val(Value::Undefined),
                    1 => nx(Next::Abr(Abr::Break(label))),
                    2 => nx(Next::Abr(Abr::Continue(label))),
                    3 => nx(Next::Abr(Abr::Return(held))),
                    _ => nx(Next::Abr(Abr::Throw(held))),
                }
            }
        }
    }

    /// An abrupt completion arrives: pop frames until one handles it.
    fn unwind(&mut self, run: &mut Run, abr: Abr) -> Res<Step> {
        loop {
            let Some(fr) = self.fpop(run) else { return Err(unsupported("async machine", Span::default())) };
            match fr {
                Fr::Body | Fr::BodyExpr => {
                    return match abr {
                        Abr::Return(v) => Ok(Step::Out(Out::Done(v))),
                        Abr::Throw(v) => Ok(Step::Out(Out::Threw(v))),
                        _ => Err(unsupported("stray break or continue", Span::default())),
                    };
                }
                Fr::Op { base, .. } => self.strunc(run, base as usize),
                Fr::Scope { saved, env } => {
                    self.env = saved;
                    self.release(env);
                }
                Fr::ForScope { saved } => {
                    let cur = self.env;
                    self.env = saved;
                    if cur != saved {
                        self.release(cur);
                    }
                }
                Fr::OfScope { outer, cur, base } => {
                    self.env = outer;
                    if cur != NONE {
                        self.release(cur);
                    }
                    self.strunc(run, base as usize);
                }
                Fr::Lab { label } => {
                    if let Abr::Break(l) = abr
                        && l == label
                    {
                        return val(Value::Undefined);
                    }
                }
                Fr::WhileB { s, lb } => match abr {
                    Abr::Break(l) if lb.has(l) => return val(Value::Undefined),
                    Abr::Continue(l) if lb.has(l) => return self.while_iter(run, s, lb),
                    _ => {}
                },
                Fr::DoB { s, lb } => match abr {
                    Abr::Break(l) if lb.has(l) => return val(Value::Undefined),
                    Abr::Continue(l) if lb.has(l) => return self.deliver_do_body(run, s, lb),
                    _ => {}
                },
                Fr::ForB { s, lb, per } => match abr {
                    Abr::Break(l) if lb.has(l) => return val(Value::Undefined),
                    Abr::Continue(l) if lb.has(l) => return self.for_after_body(run, s, lb, per),
                    _ => {}
                },
                Fr::OfB { s, lb, base } => {
                    if matches!(abr, Abr::Continue(l) if lb.has(l)) {
                        return self.of_iter(run, s, lb, base);
                    }
                    // Any other abrupt completion leaves the loop: IteratorClose. An error of `return` replaces a
                    // break, continue or return and loses to a throw. A limit, Unsupported or a heap fault is final.
                    let Slot::It(mut it) = run.slots[base as usize] else { return Err(unsupported("for-of", Span::default())) };
                    match abr {
                        Abr::Throw(v) => {
                            let e = self.iter_close_throw(&mut it, Error::Throw(v));
                            if !matches!(e, Error::Throw(_)) {
                                return Err(e);
                            }
                        }
                        _ => self.iter_close(&mut it)?,
                    }
                    if matches!(abr, Abr::Break(l) if lb.has(l)) {
                        return val(Value::Undefined);
                    }
                }
                Fr::TryB { s, env } => {
                    let StmtKind::Try { handler, finalizer, .. } = self.prog.stmt(s).kind else { return Err(unsupported("try", Span::default())) };
                    match (abr, handler) {
                        (Abr::Throw(v), Some(c)) => {
                            self.env = env;
                            let c = *self.prog.catch(c);
                            self.fpush(run, Fr::TryC { s, env })?;
                            let saved = self.env;
                            let cenv = self.new_env(saved, false)?;
                            self.env = cenv;
                            self.fpush(run, Fr::Scope { saved, env: cenv })?;
                            if let Some(p) = c.param {
                                self.bind_pattern(p, v, Bind::Declare)?;
                            }
                            return nx(Next::Exec(c.body));
                        }
                        _ => {
                            if finalizer.is_some() {
                                return self.fin_start(run, s, env, Some(abr));
                            }
                        }
                    }
                }
                Fr::TryC { s, env } => {
                    let StmtKind::Try { finalizer, .. } = self.prog.stmt(s).kind else { return Err(unsupported("try", Span::default())) };
                    if finalizer.is_some() {
                        return self.fin_start(run, s, env, Some(abr));
                    }
                }
                Fr::Fin { base, .. } => self.strunc(run, base as usize),
                Fr::Deleg { base } if run.agen => match abr {
                    Abr::Throw(v) => return self.adeleg_send(run, base, GenHow::Throw(v)),
                    Abr::Return(v) => return self.adeleg_send(run, base, GenHow::Return(v)),
                    _ => self.strunc(run, base as usize),
                },
                Fr::ADeleg { base, .. } | Fr::ADelegClose { base } => self.strunc(run, base as usize),
                Fr::AOfB { s, lb, base } => {
                    if matches!(abr, Abr::Continue(l) if lb.has(l)) {
                        return self.aof_next(run, s, lb, base);
                    }
                    if let Some(step) = self.aof_close(run, lb, base, abr)? {
                        return Ok(step);
                    }
                    if matches!(abr, Abr::Break(l) if lb.has(l)) {
                        return val(Value::Undefined);
                    }
                }
                Fr::AClose { kind, base, .. } => {
                    // The await of the result of `return` failed: a throw that closed the loop wins.
                    let held = if kind >= 3 { sval(run, base as usize) } else { Value::Undefined };
                    self.strunc(run, base as usize);
                    if kind == 4 {
                        return nx(Next::Abr(Abr::Throw(held)));
                    }
                }
                Fr::Deleg { base } => match abr {
                    Abr::Throw(v) => return self.deleg_step(run, base, GenHow::Throw(v)),
                    Abr::Return(v) => return self.deleg_step(run, base, GenHow::Return(v)),
                    _ => self.strunc(run, base as usize),
                },
                Fr::SwS { base } => {
                    self.strunc(run, base as usize);
                    if matches!(abr, Abr::Break(0)) {
                        return val(Value::Undefined);
                    }
                }
                _ => {}
            }
        }
    }

    /// The body of a do-while ended (normally or by `continue`): evaluate its test.
    fn deliver_do_body(&mut self, run: &mut Run, s: StmtId, lb: Lbl) -> Res<Step> {
        let StmtKind::DoWhile { test, .. } = self.prog.stmt(s).kind else { return Err(unsupported("loop", Span::default())) };
        self.fpush(run, Fr::DoT { s, lb })?;
        nx(Next::Eval(test))
    }
}

enum CShape {
    Plain,
    Member(ExprId, ExprId),
    Index(ExprId, ExprId),
    /// `obj.#name(...)`: the member expression and its object.
    Private(ExprId, ExprId),
    /// `super.name(...)`: the member expression.
    Super(ExprId),
}
