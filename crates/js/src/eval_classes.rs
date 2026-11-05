// Class execution for the evaluator. Included by `eval.rs` (`include!`), so it shares that module's imports.
//
// What a class is at run time
// - The constructor is a function object with `Callee::Class`. Its code and fields live in `ClassData`
//   (`ClassState::classes`, keyed by the constructor `ObjId`). A call (not `new`) of it is a TypeError.
// - Methods, getters and setters are ordinary closures made by `make_closure`. They capture the class
//   environment and record the script that made them.
// - The class scope is one environment: the class-name binding (TDZ until the class is built) and one
//   binding per `#private` name. A private name is a fresh, unreachable heap object ("name object").
//   The binding flag says what it is: field, method (PRIV_M) or accessor (PRIV_A). A method or accessor
//   lives in properties of its name object, so the heap traces it.
// - `super.x` needs the home object. When a class uses `super.x` the class builds one more environment
//   per side (instance or static) with the hidden binding `%home`. Methods of that side capture it.
// - `new.target`, and the state of a derived constructor, are hidden bindings in the call environment
//   (`%nt`, `%this`, `%fn`), declared by `call_body` only for functions the first walk flagged. No
//   per-call cost for other functions. Arrows reach them through the environment chain.
// - Private slots of an object are Rust-side state: `ClassState::privs`, keyed by the owner `ObjId`.
//   They are traced only while the owner is reachable (`cls_trace` in `collect`), so an instance that
//   nothing reaches is freed even if a slot points back at it.
//
// Not supported, and found before anything runs: `super` outside a class element (object literal
// methods), `new.target` in a field initializer, BigInt keys. Static blocks and generators are parse
// errors. Async methods run on the machine of `eval_async.rs`, which keeps `this`, the class scope (private
// names, `%home`) and `%nt` in the environments it saves, so private reads and writes and `super.m()` work
// across an `await`. Class code is owned by a script: `ClassData::script` says which. Constructor bodies and field
// initializers run with that script selected (`in_script`), and the caller's script comes back on every exit.
// A subclass of Promise, of a host constructor, of a byte class (`ArrayBuffer`, `Uint8Array`, `TextEncoder`,
// `TextDecoder`) or of Map, Set, WeakMap or WeakSet is refused with `Unsupported` when it is constructed: those constructors do not
// take a `new.target` yet. Error, Array, Object, Function and RegExp parents work.

const F_NT: u8 = 1;
const F_DERIVED: u8 = 2;
const C_HOME_INST: u8 = 1;
const C_HOME_STATIC: u8 = 2;
/// Binding flags of a private name (see `env_declare`).
const PRIV_M: u8 = 8;
const PRIV_A: u8 = 16;
/// Property keys of a private-name object. Index keys: no string needs rooting.
const PN_METHOD: PropertyKey = PropertyKey::Index(0);
const PN_ACCESSOR: PropertyKey = PropertyKey::Index(1);
const PN_CLASS: PropertyKey = PropertyKey::Index(2);
const PN_STATIC: PropertyKey = PropertyKey::Index(3);
/// Default bound on private slots of all objects together (about 100 bytes each, worst case).
pub const DEFAULT_PRIVATE_SLOTS: usize = 1 << 21;

#[derive(Clone, Copy)]
enum FKey {
    Prop(PropertyKey),
    /// A private name: its name object and the interned `#name` id.
    Private(ObjId, u32),
}

struct Field {
    key: FKey,
    init: Option<ExprId>,
}

struct ClassData {
    /// The slot of the script that owns `class`, `ctor_fid` and every field initializer. Run them with it selected.
    script: u32,
    class: ClassId,
    /// The function name (class name or inferred), for error messages.
    name: StrId,
    ctor_fid: Option<FuncId>,
    derived: bool,
    /// Environment of the constructor body and parent of the field-initializer environment.
    env: u32,
    /// Private methods and accessors added to every instance (the brand), before the fields.
    inst_privs: Vec<(ObjId, u32)>,
    /// Slots one instance gets (private methods, accessors and private fields).
    n_slots: usize,
    fields: Vec<Field>,
}

impl ClassData {
    fn values(&self, out: &mut Vec<Value>) {
        out.push(Value::String(self.name));
        out.extend(self.inst_privs.iter().map(|&(o, _)| Value::Object(o)));
        for f in &self.fields {
            match f.key {
                FKey::Prop(PropertyKey::String(s)) => out.push(Value::String(s)),
                FKey::Prop(PropertyKey::Symbol(s)) => out.push(Value::Symbol(s)),
                FKey::Prop(PropertyKey::Index(_)) => {}
                FKey::Private(o, _) => out.push(Value::Object(o)),
            }
        }
    }
}

#[derive(Default)]
struct ClassState {
    n_home: u32,
    n_this: u32,
    n_nt: u32,
    n_fn: u32,
    /// Per function (`F_*`) and per class (`C_*`), set by the first walk.
    fflags: Vec<u8>,
    cflags: Vec<u8>,
    /// Some `super.x` or `super()` exists: `link_inner` checks for them.
    super_used: bool,
    classes: FxMap<ObjId, Rc<ClassData>>,
    privs: FxMap<ObjId, Vec<(ObjId, Value)>>,
    n_slots: usize,
    max_slots: usize,
    /// The `new.target` for the next `call_body` of a flagged function.
    pending_nt: Option<ObjId>,
    /// First walk: the class element (class, is_static) the code belongs to. `elem_ctx` is for the next function.
    cur: Option<(ClassId, bool)>,
    elem_ctx: Option<(ClassId, bool)>,
}

fn is_super(prog: &Program<'_>, e: ExprId) -> bool {
    matches!(prog.expr(e).kind, ExprKind::Super)
}

impl<'p> Interp<'p> {
    fn cls_init(&mut self) {
        let p = self.prog.clone();
        self.cls.fflags = vec![0; p.stats().funcs];
        self.cls.cflags = vec![0; p.stats().classes];
        self.cls.n_home = self.intern_name("%home");
        self.cls.n_this = self.intern_name("%this");
        self.cls.n_nt = self.intern_name("%nt");
        self.cls.n_fn = self.intern_name("%fn");
        self.cls.max_slots = DEFAULT_PRIVATE_SLOTS;
    }

    /// Change the bound on private slots (all objects together). `LimitKind::PrivateSlots` when it is reached.
    pub fn set_private_slot_limit(&mut self, n: usize) {
        self.cls.max_slots = n;
    }

    /// Private slots alive now (for tools and tests).
    pub fn private_slots(&self) -> usize {
        self.cls.n_slots
    }

    /// Classes alive now (for tools and tests).
    pub fn live_classes(&self) -> usize {
        self.cls.classes.len()
    }

    // ------------------------------------------------------------ the first walk

    fn private_id(&mut self, span: Span) -> u32 {
        let n = self.prog.name(span);
        self.intern_name(&format!("#{n}"))
    }

    fn w_class(&mut self, c: ClassId, fs: usize, ac: usize) -> Res<()> {
        self.descend(|it| it.w_class_inner(c, fs, ac))
    }

    fn w_class_inner(&mut self, c: ClassId, fs: usize, ac: usize) -> Res<()> {
        let prog = self.prog.clone();
        let class = *prog.class(c);
        if let Some(n) = class.name {
            self.name_at(n);
        }
        if let Some(s) = class.superclass {
            self.w_expr(s, fs, ac)?;
        }
        let saved = self.cls.cur;
        for el in prog.list(class.elems) {
            let e = *prog.elem(el);
            match e.key {
                // A computed key runs in the context around the class.
                Key::Computed(k) => self.w_expr(k, fs, ac)?,
                Key::Private(sp) => {
                    self.private_id(sp);
                }
                Key::BigInt(_) => return Err(unsupported("BigInt property key", e.span)),
                _ => {}
            }
            match e.kind {
                ElemKind::Method(f) | ElemKind::Getter(f) | ElemKind::Setter(f) => {
                    if prog.func(f).kind == FuncKind::DerivedConstructor {
                        self.cls.fflags[f.index()] |= F_DERIVED;
                    }
                    self.cls.elem_ctx = Some((c, e.is_static));
                    self.w_func(f, ac)?;
                }
                ElemKind::Field(Some(init)) => {
                    // A field initializer is not inside a function: `ac` is "none" (no `arguments`, no `new.target`).
                    self.cls.cur = Some((c, e.is_static));
                    let r = self.w_expr(init, fs, usize::MAX);
                    self.cls.cur = saved;
                    r?;
                }
                ElemKind::Field(None) => {}
            }
        }
        Ok(())
    }

    fn cls_w_this(&mut self, e: ExprId, ac: usize) {
        if ac != usize::MAX && self.prog.func(FuncId::from_index(ac)).kind == FuncKind::DerivedConstructor {
            self.expr_name[e.index()] = self.cls.n_this;
        }
    }

    fn cls_w_super(&mut self, span: Span) -> Res<()> {
        let Some((c, is_static)) = self.cls.cur else { return Err(unsupported("super outside a class element", span)) };
        self.cls.super_used = true;
        self.cls.cflags[c.index()] |= if is_static { C_HOME_STATIC } else { C_HOME_INST };
        Ok(())
    }

    fn cls_w_new_target(&mut self, span: Span, ac: usize) -> Res<()> {
        if ac == usize::MAX {
            return Err(unsupported("new.target in a class field initializer", span));
        }
        self.cls.fflags[ac] |= F_NT;
        Ok(())
    }

    fn cls_w_private(&mut self, e: ExprId, name: Span) {
        self.expr_name[e.index()] = self.private_id(name);
    }

    // ------------------------------------------------------------ collect and roots

    /// Rust-side class state of the object `o`, for `collect`'s reachability walk.
    fn cls_trace(&self, o: ObjId, work: &mut Vec<Value>, env_work: &mut Vec<u32>) {
        if let Some(cd) = self.cls.classes.get(&o) {
            env_work.push(cd.env);
            cd.values(work);
        }
        if let Some(slots) = self.cls.privs.get(&o) {
            for &(n, v) in slots {
                work.push(Value::Object(n));
                work.push(v);
            }
        }
    }

    /// Forget the state of objects the walk did not reach (they are about to be freed).
    fn cls_prune(&mut self, seen: &FxSet<ObjId>) {
        self.cls.classes.retain(|o, _| seen.contains(o));
        let mut dropped = 0;
        self.cls.privs.retain(|o, v| {
            let keep = seen.contains(o);
            if !keep {
                dropped += v.len();
            }
            keep
        });
        self.cls.n_slots -= dropped;
    }

    /// Every heap value held by class state. Complete, and strong: use `collect` for the precise set.
    fn cls_roots(&self, out: &mut Vec<Value>) {
        for cd in self.cls.classes.values() {
            cd.values(out);
        }
        for slots in self.cls.privs.values() {
            for &(n, v) in slots {
                out.push(Value::Object(n));
                out.push(v);
            }
        }
    }

    // ------------------------------------------------------------ calls and construction

    /// `new.target` for the next call of `fid`, only when that function reads it.
    fn cls_set_nt(&mut self, script: u32, fid: FuncId, nt: ObjId) {
        if self.fflags_of(script, fid) & (F_NT | F_DERIVED) != 0 {
            self.cls.pending_nt = Some(nt);
        }
    }

    /// Walk flags of function `fid` of script `script` (not necessarily the current one).
    fn fflags_of(&self, script: u32, fid: FuncId) -> u8 {
        if script as usize == self.cur { self.cls.fflags[fid.index()] } else { self.scripts[script as usize].fflags[fid.index()] }
    }

    /// Run `f` with script `script` selected, and give the caller's script back on every exit.
    fn in_script<T>(&mut self, script: u32, f: impl FnOnce(&mut Self) -> Res<T>) -> Res<T> {
        let old = self.cur;
        self.select(script as usize);
        let r = f(self);
        self.select(old);
        r
    }

    /// Called by `call_body` for a flagged function: declare `%nt` (and `%this`, `%fn` of a derived constructor).
    fn cls_frame(&mut self, env: u32, fid: FuncId, fobj: ObjId) {
        let nt = self.cls.pending_nt.take().map_or(Value::Undefined, Value::Object);
        if self.cls.fflags[fid.index()] & F_DERIVED != 0 {
            self.env_declare(env, self.cls.n_this, MUT, Value::Undefined);
            self.env_declare(env, self.cls.n_fn, INIT, Value::Object(fobj));
        }
        self.env_declare(env, self.cls.n_nt, INIT, nt);
    }

    /// Keep in sync with the cases of `construct_inner`.
    /// A native constructor that builds a branded object and cannot take another `new.target` (the byte classes, Map, Set).
    /// A prototype patch after the call would give the wrong brand, so a subclass of one is refused when it is constructed.
    fn native_no_nt(n: Nat) -> bool {
        matches!(n, Nat::Bin(b) if b.ctor_name().is_some() || b == BinFn::TaCtor) || matches!(n, Nat::Sc(f) if f.is_ctor()) || n == Nat::Date(DateFn::Ctor)
    }

    fn is_constructor(&self, mut o: ObjId) -> bool {
        loop {
            match self.funcs.get(&o) {
                Some(Callee::Class) => return true,
                Some(&Callee::Script { script, fid, .. }) => return self.func_of(script, fid).kind == FuncKind::Normal && !self.func_of(script, fid).is_async,
                Some(&Callee::Native(n)) => return matches!(n, Nat::ObjectCtor | Nat::FnCtor | Nat::ArrayCtor | Nat::ErrorCtor(_) | Nat::Ext(Bi::RegExpCtor)) || Self::native_no_nt(n),
                Some(&Callee::Host { ctor, .. }) => return ctor,
                Some(&Callee::Promise(pf)) => return matches!(pf, PFn::Ctor | PFn::AggErrCtor),
                Some(&Callee::Bound { target, .. }) => o = target,
                None => return false,
            }
        }
    }

    fn class_call_error(&mut self, o: ObjId) -> Error {
        let name = self.cls.classes.get(&o).map(|cd| cd.name).and_then(|s| self.heap.to_string_lossy(s).ok()).unwrap_or_default();
        if name.is_empty() {
            self.type_err("Class constructors cannot be invoked without 'new'")
        } else {
            self.type_err(&format!("Class constructor {name} cannot be invoked without 'new'"))
        }
    }

    /// GetPrototypeFromConstructor.
    fn proto_from_ctor(&mut self, nt: ObjId) -> Res<ObjId> {
        self.proto_from_ctor_or(nt, self.object_proto)
    }

    fn proto_from_ctor_or(&mut self, nt: ObjId, default: ObjId) -> Res<ObjId> {
        Ok(match self.get_prop(Value::Object(nt), self.k_prototype)? {
            Value::Object(p) => p,
            _ => default,
        })
    }

    /// `Construct(f, args, nt)`: like `new f(...args)` but `nt` is the `new.target`.
    fn construct_nt(&mut self, f: ObjId, args: &[Value], nt: ObjId, callee: ExprId) -> Res<Value> {
        self.descend(|it| it.construct_nt_inner(f, args, nt, callee))
    }

    fn construct_nt_inner(&mut self, f: ObjId, args: &[Value], nt: ObjId, callee: ExprId) -> Res<Value> {
        if f == nt {
            return self.construct_inner(Value::Object(f), args, callee);
        }
        enum D {
            Class,
            Script(u32, FuncId, u32, bool),
            /// A constructor that cannot take a different `new.target` yet (Promise, a host constructor).
            NoNt(&'static str),
            Native(Nat),
            Bound(ObjId, Vec<Value>),
        }
        let d = match self.funcs.get(&f) {
            Some(Callee::Class) => D::Class,
            Some(&Callee::Script { script, fid, env, self_binding }) if self.func_of(script, fid).kind == FuncKind::Normal && !self.func_of(script, fid).is_async => D::Script(script, fid, env, self_binding),
            Some(&Callee::Native(n)) if matches!(n, Nat::ObjectCtor | Nat::FnCtor | Nat::ArrayCtor | Nat::ErrorCtor(_) | Nat::Ext(Bi::RegExpCtor)) => D::Native(n),
            Some(&Callee::Native(n)) if Self::native_no_nt(n) => D::NoNt("subclassing a native constructor (new.target is not passed to the constructor)"),
            Some(&Callee::Promise(PFn::Ctor)) => D::NoNt("subclassing Promise (new.target is not passed to the constructor)"),
            Some(&Callee::Promise(PFn::AggErrCtor)) => D::NoNt("subclassing AggregateError (new.target is not passed to the constructor)"),
            Some(&Callee::Host { ctor: true, .. }) => D::NoNt("subclassing a host constructor (new.target is not passed to the constructor)"),
            Some(&Callee::Native(Nat::StringFn | Nat::NumberFn | Nat::BooleanFn)) => return Err(unsupported("wrapper object (new String/Number/Boolean)", Span::default())),
            Some(Callee::Bound { target, args, .. }) => D::Bound(*target, args.clone()),
            _ => {
                let d = self.src_text(callee);
                return Err(self.type_err(&format!("{d} is not a constructor")));
            }
        };
        match d {
            D::Class => {
                if self.depth >= self.limits.max_call_depth {
                    return Err(Error::Limit(LimitKind::CallDepth));
                }
                self.tick()?;
                self.depth += 1;
                let r = self.construct_class_body(f, args, nt);
                self.depth -= 1;
                r
            }
            D::NoNt(what) => Err(unsupported(what, Span::default())),
            D::Script(script, fid, env, sb) => {
                if self.depth >= self.limits.max_call_depth {
                    return Err(Error::Limit(LimitKind::CallDepth));
                }
                self.tick()?;
                self.depth += 1;
                let r: Res<Value> = (|| {
                    let proto = self.proto_from_ctor(nt)?;
                    let obj = h!(self, self.heap.new_object(Some(proto)));
                    self.cls_set_nt(script, fid, nt);
                    let r = self.call_script((script, fid), f, env, sb, Value::Object(obj), args);
                    self.cls.pending_nt = None;
                    let r = r?;
                    Ok(if matches!(r, Value::Object(_)) { r } else { Value::Object(obj) })
                })();
                self.depth -= 1;
                r
            }
            D::Native(n) => {
                if self.depth >= self.limits.max_call_depth { return Err(Error::Limit(LimitKind::CallDepth)); }
                self.tick()?;
                self.depth += 1;
                let r = (|| match n {
                    Nat::ObjectCtor => {
                        let proto = self.proto_from_ctor(nt)?;
                        Ok(Value::Object(h!(self, self.heap.new_object(Some(proto)))))
                    }
                    Nat::ArrayCtor => {
                        let proto = self.proto_from_ctor_or(nt, self.array_proto)?;
                        self.array_ctor(args, proto)
                    }
                    Nat::ErrorCtor(k) => {
                        let proto = self.proto_from_ctor_or(nt, self.error_protos[k as usize])?;
                        self.error_ctor(arg(args, 0), proto)
                    }
                    Nat::Ext(Bi::RegExpCtor) => self.regexp_ctor_nt(arg(args, 0), arg(args, 1), true, Some(nt)),
                    _ => self.call_native(n, Value::Undefined, args, true),
                })();
                self.depth -= 1;
                r
            }
            D::Bound(target, mut all) => {
                all.extend_from_slice(args);
                self.construct_nt(target, &all, nt, callee)
            }
        }
    }

    /// Run the body of a class constructor in a fresh environment. For a derived constructor also give
    /// back the `this` that `super()` bound (None: `super()` never ran).
    fn call_ctor(&mut self, f: ObjId, cd: &ClassData, fid: FuncId, this: Value, args: &[Value], nt: ObjId) -> Res<(Value, Option<Value>)> {
        let (saved_env, saved_strict) = (self.env, self.strict);
        let env = self.new_env(cd.env, true)?;
        self.env = env;
        self.cls_set_nt(cd.script, fid, nt);
        let r = self.call_body(f, fid, env, false, this, args);
        self.cls.pending_nt = None;
        let bound = if cd.derived { self.find_in(env, self.cls.n_this).map(|i| &self.envs[env as usize].bindings[i]).filter(|b| b.flags & INIT != 0).map(|b| b.value) } else { None };
        self.env = saved_env;
        self.strict = saved_strict;
        self.release(env);
        r.map(|v| (v, bound))
    }

    /// [[Construct]] of a class constructor. The caller has counted the call depth.
    fn construct_class_body(&mut self, f: ObjId, args: &[Value], nt: ObjId) -> Res<Value> {
        let Some(cd) = self.cls.classes.get(&f).cloned() else { return Err(unsupported("class constructor without class data", Span::default())) };
        self.in_script(cd.script, |it| it.construct_class_in(f, &cd, args, nt))
    }

    /// The body of `construct_class_body`, with the class's own script selected.
    fn construct_class_in(&mut self, f: ObjId, cd: &Rc<ClassData>, args: &[Value], nt: ObjId) -> Res<Value> {
        if !cd.derived {
            let proto = self.proto_from_ctor(nt)?;
            let obj = h!(self, self.heap.new_object(Some(proto)));
            self.init_instance(f, obj)?;
            let Some(fid) = cd.ctor_fid else { return Ok(Value::Object(obj)) };
            let (r, _) = self.call_ctor(f, cd, fid, Value::Object(obj), args, nt)?;
            return Ok(if matches!(r, Value::Object(_)) { r } else { Value::Object(obj) });
        }
        let Some(fid) = cd.ctor_fid else {
            // The default derived constructor is `constructor(...args) { super(...args); }`.
            let heritage = self.prog.class(cd.class).superclass.unwrap_or_else(|| ExprId::from_index(0));
            let parent = self.super_constructor(f)?;
            let r = self.construct_nt(parent, args, nt, heritage)?;
            if let Value::Object(ro) = r {
                self.init_instance(f, ro)?;
            }
            return Ok(r);
        };
        let (r, bound) = self.call_ctor(f, cd, fid, Value::Undefined, args, nt)?;
        if matches!(r, Value::Object(_)) {
            return Ok(r);
        }
        if !matches!(r, Value::Undefined) {
            return Err(self.type_err("Derived constructors may only return object or undefined"));
        }
        bound.ok_or_else(|| self.mk_err(REFERENCE_ERROR, "Must call super constructor in derived class before accessing 'this' or returning from derived constructor"))
    }

    /// `GetPrototypeOf(active)`, which must be a constructor.
    fn super_constructor(&mut self, active: ObjId) -> Res<ObjId> {
        match h!(self, self.heap.get_prototype_of(active)) {
            Some(p) if self.is_constructor(p) => Ok(p),
            other => {
                let t = other.map_or("null".to_string(), |_| "value".to_string());
                Err(self.type_err(&format!("Super constructor {t} of anonymous class is not a constructor")))
            }
        }
    }

    /// `super(...args)`: the `Call` expression `e` has `super` as its callee.
    fn super_call(&mut self, e: ExprId) -> Res<Option<(Value, Value)>> {
        let ExprKind::Call { callee, args, .. } = self.prog.expr(e).kind else { return Err(unsupported("super call", self.prog.expr(e).span)) };
        let (Some((fe, fi)), Some((ne, ni))) = (self.lookup(self.cls.n_fn), self.lookup(self.cls.n_nt)) else {
            return Err(unsupported("super() outside a derived constructor", self.prog.expr(e).span));
        };
        let (Value::Object(active), Value::Object(nt)) = (self.envs[fe as usize].bindings[fi].value, self.envs[ne as usize].bindings[ni].value) else {
            return Err(unsupported("super() outside a derived constructor", self.prog.expr(e).span));
        };
        let parent = self.super_constructor(active)?;
        let argv = self.eval_args(args)?;
        let result = self.construct_nt(parent, &argv, nt, callee)?;
        self.arg_pool.push(argv);
        // BindThisValue: once only.
        let Some((te, ti)) = self.lookup(self.cls.n_this) else { return Err(unsupported("super() outside a derived constructor", self.prog.expr(e).span)) };
        let b = &mut self.envs[te as usize].bindings[ti];
        if b.flags & INIT != 0 {
            return Err(self.mk_err(REFERENCE_ERROR, "Super constructor may only be called once"));
        }
        b.flags |= INIT;
        b.value = result;
        if let Value::Object(ro) = result {
            self.init_instance(active, ro)?;
        }
        Ok(Some((result, Value::Undefined)))
    }

    /// `this` of a derived constructor, or an arrow in it: the binding `super()` fills.
    fn this_binding(&mut self) -> Res<Value> {
        match self.lookup(self.cls.n_this) {
            Some((e, i)) => {
                let b = &self.envs[e as usize].bindings[i];
                if b.flags & INIT != 0 { Ok(b.value) } else { Err(self.this_tdz()) }
            }
            None => Ok(self.lookup_this()),
        }
    }

    fn this_tdz(&mut self) -> Error {
        self.mk_err(REFERENCE_ERROR, "Must call super constructor in derived class before accessing 'this' or returning from derived constructor")
    }

    /// `this` for `super.x`, checked: nearest of a derived-constructor binding or an ordinary `this`.
    fn this_dyn(&mut self) -> Res<Value> {
        let mut e = self.env;
        while e != NONE {
            if let Some(i) = self.find_in(e, self.cls.n_this) {
                let b = &self.envs[e as usize].bindings[i];
                return if b.flags & INIT != 0 { Ok(b.value) } else { Err(self.this_tdz()) };
            }
            if let Some(t) = self.envs[e as usize].this {
                return Ok(t);
            }
            e = self.envs[e as usize].parent;
        }
        Ok(Value::Undefined)
    }

    fn eval_new_target(&mut self) -> Res<Value> {
        Ok(match self.lookup(self.cls.n_nt) {
            Some((e, i)) => self.envs[e as usize].bindings[i].value,
            None => Value::Undefined,
        })
    }

    // ------------------------------------------------------------ super properties

    /// `[[HomeObject]].[[GetPrototypeOf]]()` of the running method (None is `null`).
    fn super_base(&mut self, span: Span) -> Res<Option<ObjId>> {
        let Some((e, i)) = self.lookup(self.cls.n_home) else { return Err(unsupported("super outside a class element", span)) };
        let Value::Object(home) = self.envs[e as usize].bindings[i].value else { return Err(unsupported("super outside a class element", span)) };
        Ok(h!(self, self.heap.get_prototype_of(home)))
    }

    fn super_get(&mut self, this: Value, key: PropertyKey) -> Res<Value> {
        let Some(base) = self.super_base(Span::default())? else {
            let k = self.key_text(key);
            return Err(self.type_err(&format!("Cannot read properties of null (reading '{k}')")));
        };
        match h!(self, self.heap.get(base, key)) {
            Get::Value(v) => Ok(v),
            Get::Accessor { getter: Some(g) } => self.call_fn(Value::Object(g), this, &[]),
            Get::Accessor { getter: None } => Ok(Value::Undefined),
        }
    }

    /// `super.key = v`: OrdinarySet on the home prototype, with `this` as the receiver.
    fn super_set(&mut self, this: Value, key: PropertyKey, v: Value) -> Res<()> {
        let base = self.super_base(Span::default())?;
        let found = match base {
            Some(b) => h!(self, self.heap.lookup(b, key)),
            None => None,
        };
        let reject = |s: &mut Self| {
            let k = s.key_text(key);
            Err(s.type_err(&format!("Cannot assign to read only property '{k}' of object")))
        };
        match found {
            Some((_, Property::Accessor { set: Some(s), .. })) => return self.call_fn(Value::Object(s), this, &[v]).map(|_| ()),
            Some((_, Property::Accessor { set: None, .. })) | Some((_, Property::Data { writable: false, .. })) => return reject(self),
            _ => {}
        }
        let Value::Object(r) = this else { return reject(self) };
        let d = match h!(self, self.heap.get_own_property(r, key)) {
            Some(Property::Accessor { .. }) | Some(Property::Data { writable: false, .. }) => return reject(self),
            Some(Property::Data { .. }) => PropertyDescriptor { value: Some(v), ..Default::default() },
            None => PropertyDescriptor::data(v),
        };
        if h!(self, self.heap.define_own_property(r, key, &d)) { Ok(()) } else { reject(self) }
    }

    /// The key of `super.name` or `super[expr]`, after `this` (the order of the specification).
    fn super_key(&mut self, e: ExprId) -> Res<PropertyKey> {
        match self.prog.expr(e).kind {
            ExprKind::Member { name, .. } => self.expr_key(e, name),
            ExprKind::Index { index, .. } => {
                let iv = self.eval_expr(index)?;
                self.coerce_key(iv)
            }
            _ => Err(unsupported("super reference", self.prog.expr(e).span)),
        }
    }

    fn super_link(&mut self, e: ExprId) -> Res<Option<(Value, Value)>> {
        let this = self.this_dyn()?;
        let key = self.super_key(e)?;
        Ok(Some((self.super_get(this, key)?, this)))
    }

    // ------------------------------------------------------------ private names

    /// The name object, the `#name` id and the binding flags of the private name used by expression `e`.
    fn priv_lookup(&mut self, e: ExprId) -> Res<(ObjId, u32, u8)> {
        let n = self.expr_name[e.index()];
        if let Some((env, i)) = self.lookup(n) {
            let b = &self.envs[env as usize].bindings[i];
            if let Value::Object(o) = b.value {
                return Ok((o, n, b.flags));
            }
        }
        Err(unsupported("private name is not in scope", self.prog.expr(e).span))
    }

    fn priv_slot(&self, owner: ObjId, pn: ObjId) -> Option<Value> {
        self.cls.privs.get(&owner)?.iter().find(|s| s.0 == pn).map(|s| s.1)
    }

    /// The error of a failed brand check.
    fn priv_missing(&mut self, pn: ObjId, n: u32, flags: u8, write: bool) -> Error {
        if flags & (PRIV_M | PRIV_A) != 0 {
            let is_static = matches!(self.heap.get_own_property(pn, PN_STATIC), Ok(Some(Property::Data { value: Value::Bool(true), .. })));
            let name = match self.heap.get_own_property(pn, PN_CLASS) {
                Ok(Some(Property::Data { value: Value::String(s), .. })) => self.heap.to_string_lossy(s).unwrap_or_default(),
                _ => String::new(),
            };
            let name = if name.is_empty() { "anonymous".to_string() } else { name };
            return self.type_err(&if is_static { format!("Receiver must be class {name}") } else { format!("Receiver must be an instance of class {name}") });
        }
        let n = self.names[n as usize].clone();
        let (verb, prep) = if write { ("write", "to") } else { ("read", "from") };
        self.type_err(&format!("Cannot {verb} private member {n} {prep} an object whose class did not declare it"))
    }

    fn priv_accessor(&mut self, pn: ObjId) -> Res<(Option<ObjId>, Option<ObjId>)> {
        match h!(self, self.heap.get_own_property(pn, PN_ACCESSOR)) {
            Some(Property::Accessor { get, set, .. }) => Ok((get, set)),
            _ => Ok((None, None)),
        }
    }

    fn priv_get(&mut self, base: Value, pn: ObjId, n: u32, flags: u8) -> Res<Value> {
        let o = match base {
            Value::Object(o) => o,
            Value::Undefined | Value::Null => {
                let t = self.display(base);
                let name = self.names[n as usize].clone();
                return Err(self.type_err(&format!("Cannot read properties of {t} (reading '{name}')")));
            }
            _ => return Err(self.priv_missing(pn, n, flags, false)),
        };
        let Some(slot) = self.priv_slot(o, pn) else { return Err(self.priv_missing(pn, n, flags, false)) };
        if flags & PRIV_M != 0 {
            return match h!(self, self.heap.get_own_property(pn, PN_METHOD)) {
                Some(Property::Data { value, .. }) => Ok(value),
                _ => Ok(Value::Undefined),
            };
        }
        if flags & PRIV_A != 0 {
            return match self.priv_accessor(pn)?.0 {
                Some(g) => self.call_fn(Value::Object(g), base, &[]),
                None => {
                    let name = self.names[n as usize].clone();
                    Err(self.type_err(&format!("'{name}' was defined without a getter")))
                }
            };
        }
        Ok(slot)
    }

    fn priv_put(&mut self, base: Value, pn: ObjId, n: u32, flags: u8, v: Value) -> Res<()> {
        let o = match base {
            Value::Object(o) => o,
            Value::Undefined | Value::Null => {
                let t = self.display(base);
                let name = self.names[n as usize].clone();
                return Err(self.type_err(&format!("Cannot set properties of {t} (setting '{name}')")));
            }
            _ => return Err(self.priv_missing(pn, n, flags, true)),
        };
        if self.priv_slot(o, pn).is_none() {
            return Err(self.priv_missing(pn, n, flags, true));
        }
        let name = self.names[n as usize].clone();
        if flags & PRIV_M != 0 {
            return Err(self.type_err(&format!("Private method '{name}' is not writable")));
        }
        if flags & PRIV_A != 0 {
            return match self.priv_accessor(pn)?.1 {
                Some(s) => self.call_fn(Value::Object(s), base, &[v]).map(|_| ()),
                None => Err(self.type_err(&format!("'{name}' was defined without a setter"))),
            };
        }
        if let Some(s) = self.cls.privs.get_mut(&o).and_then(|l| l.iter_mut().find(|s| s.0 == pn)) {
            s.1 = v;
        }
        Ok(())
    }

    /// Add a private slot. A second add of the same name is a TypeError. Bounded by `max_slots`.
    fn add_slot(&mut self, owner: ObjId, pn: ObjId, n: u32, v: Value, cap: usize) -> Res<()> {
        if self.priv_slot(owner, pn).is_some() {
            let name = self.names[n as usize].clone();
            return Err(self.type_err(&format!("Cannot initialize {name} twice on the same object")));
        }
        if self.cls.n_slots >= self.cls.max_slots {
            return Err(Error::Limit(LimitKind::PrivateSlots));
        }
        self.cls.n_slots += 1;
        self.cls.privs.entry(owner).or_insert_with(|| Vec::with_capacity(cap)).push((pn, v));
        Ok(())
    }

    fn link_private(&mut self, e: ExprId) -> Res<Option<(Value, Value)>> {
        let ExprKind::PrivateMember { obj, optional, .. } = self.prog.expr(e).kind else { return Err(unsupported("private name", self.prog.expr(e).span)) };
        let Some((base, _)) = self.link(obj)? else { return Ok(None) };
        if optional && is_nullish(base) {
            return Ok(None);
        }
        let (pn, n, fl) = self.priv_lookup(e)?;
        Ok(Some((self.priv_get(base, pn, n, fl)?, base)))
    }

    /// `#name in right`.
    fn eval_private_in(&mut self, e: ExprId) -> Res<Value> {
        let ExprKind::PrivateIn { right, .. } = self.prog.expr(e).kind else { return Err(unsupported("private name", self.prog.expr(e).span)) };
        let (pn, n, _) = self.priv_lookup(e)?;
        let r = self.eval_expr(right)?;
        let Value::Object(o) = r else {
            let t = self.display(r);
            let name = self.names[n as usize].clone();
            return Err(self.type_err(&format!("Cannot use 'in' operator to search for '{name}' in {t}")));
        };
        Ok(Value::Bool(self.priv_slot(o, pn).is_some()))
    }

    /// The reference of an assignment or update target that is a member: a property, a private name or a super property.
    fn member_target(&mut self, me: ExprId) -> Res<Ref> {
        let ex = *self.prog.expr(me);
        match ex.kind {
            ExprKind::PrivateMember { obj, .. } => {
                let base = self.eval_expr(obj)?;
                let (pn, n, fl) = self.priv_lookup(me)?;
                Ok(Ref::Priv(base, pn, n, fl))
            }
            ExprKind::Member { obj, .. } | ExprKind::Index { obj, .. } if self.cls.super_used && is_super(&self.prog, obj) => {
                let this = self.this_dyn()?;
                let key = self.super_key(me)?;
                Ok(Ref::Super(this, key))
            }
            ExprKind::Paren(i) => self.member_target(i),
            _ => {
                let (b, k) = self.member_ref(me)?;
                Ok(Ref::Prop(b, k))
            }
        }
    }

    // ------------------------------------------------------------ building a class

    fn eval_class(&mut self, c: ClassId, hint: Option<StrId>) -> Res<Value> {
        self.descend(|it| it.eval_class_inner(c, hint))
    }

    fn eval_class_inner(&mut self, c: ClassId, hint: Option<StrId>) -> Res<Value> {
        let saved = (self.env, self.strict);
        let env_c = self.new_env(saved.0, false)?;
        self.env = env_c;
        self.strict = true; // every part of a class is strict code
        let r = self.class_define(c, hint, env_c);
        (self.env, self.strict) = saved;
        self.release(env_c);
        r
    }

    /// An environment with the hidden `%home` binding.
    fn home_env(&mut self, parent: u32, home: ObjId) -> Res<u32> {
        let e = self.new_env(parent, false)?;
        self.env_declare(e, self.cls.n_home, INIT, Value::Object(home));
        Ok(e)
    }

    fn elem_key(&mut self, key: Key, env_c: u32) -> Res<FKey> {
        let prog = self.prog.clone();
        match key {
            Key::Computed(e) => {
                let v = self.eval_expr(e)?;
                Ok(FKey::Prop(self.coerce_key(v)?))
            }
            Key::Private(sp) => {
                let n = self.private_id(sp);
                let Some(i) = self.find_in(env_c, n) else { return Err(unsupported("private name is not in scope", sp)) };
                let Value::Object(o) = self.envs[env_c as usize].bindings[i].value else { return Err(unsupported("private name is not in scope", sp)) };
                Ok(FKey::Private(o, n))
            }
            k => {
                let s = self.key_string(k, &prog)?;
                Ok(FKey::Prop(h!(self, self.heap.key_from_string(s))))
            }
        }
    }

    /// The `name` of a method made from `key` (`prefix` is "get " or "set " for accessors).
    fn method_name(&mut self, key: FKey, prefix: &str) -> Res<StrId> {
        let base = match key {
            FKey::Private(_, n) => self.names[n as usize].clone(),
            FKey::Prop(PropertyKey::Symbol(s)) => match h!(self, self.heap.symbol_description(s)) {
                Some(d) => format!("[{}]", h!(self, self.heap.to_string_lossy(d))),
                None => String::new(),
            },
            FKey::Prop(PropertyKey::String(s)) if prefix.is_empty() => return Ok(s),
            FKey::Prop(k) => self.key_text(k),
        };
        Ok(h!(self, self.heap.intern_str(&format!("{prefix}{base}"))))
    }

    fn class_define(&mut self, c: ClassId, hint: Option<StrId>, env_c: u32) -> Res<Value> {
        let prog = self.prog.clone();
        let class = *prog.class(c);
        let cname = class.name.map(|s| self.name_at(s));
        if let Some(n) = cname {
            self.env_declare(env_c, n, 0, Value::Undefined); // a const, in its TDZ until the class is built
        }
        // The heritage runs in the class scope: the class name is still in its TDZ, the private names are not yet visible.
        let (proto_parent, ctor_parent) = match class.superclass {
            None => (Some(self.object_proto), self.function_proto),
            Some(se) => {
                let sv = self.eval_expr(se)?;
                match sv {
                    Value::Null => (None, self.function_proto),
                    Value::Object(so) if self.is_constructor(so) => match self.get_prop(sv, self.k_prototype)? {
                        Value::Object(p) => (Some(p), so),
                        Value::Null => (None, so),
                        other => {
                            let t = self.display(other);
                            return Err(self.type_err(&format!("Class extends value does not have valid prototype property {t}")));
                        }
                    },
                    other => {
                        let t = self.display(other);
                        return Err(self.type_err(&format!("Class extends value {t} is not a constructor or null")));
                    }
                }
            }
        };
        let name_str = match (cname, hint) {
            (Some(n), _) => self.name_str(n)?,
            (None, Some(s)) => s,
            (None, None) => h!(self, self.heap.intern_str("")),
        };
        // Private names: one fresh name object per declared `#name`. The binding flags tell the kind.
        for el in prog.list(class.elems) {
            let e = *prog.elem(el);
            let Key::Private(sp) = e.key else { continue };
            let n = self.private_id(sp);
            let flag = match e.kind {
                ElemKind::Method(_) => PRIV_M,
                ElemKind::Getter(_) | ElemKind::Setter(_) => PRIV_A,
                ElemKind::Field(_) => 0,
            };
            match self.find_in(env_c, n) {
                Some(i) => self.envs[env_c as usize].bindings[i].flags |= flag,
                None => {
                    let o = h!(self, self.heap.new_object(None));
                    if flag != 0 {
                        self.def(o, PN_CLASS, Value::String(name_str), false, false, false)?;
                        self.def(o, PN_STATIC, Value::Bool(e.is_static), false, false, false)?;
                    }
                    self.env_declare(env_c, n, INIT | flag, Value::Object(o));
                }
            }
        }
        let proto = h!(self, self.heap.new_object(proto_parent));
        let ctor = self.new_func_object(Callee::Class)?;
        h!(self, self.heap.set_prototype_of(ctor, Some(ctor_parent)));
        let flags = self.cls.cflags[c.index()];
        let env_inst = if flags & C_HOME_INST != 0 { self.home_env(env_c, proto)? } else { env_c };
        let env_static = if flags & C_HOME_STATIC != 0 { self.home_env(env_c, ctor)? } else { env_c };
        let derived = class.superclass.is_some();
        let mut ctor_fid = None;
        let mut len = 0;
        for el in prog.list(class.elems) {
            if let ElemKind::Method(f) = prog.elem(el).kind
                && matches!(prog.func(f).kind, FuncKind::Constructor | FuncKind::DerivedConstructor)
            {
                ctor_fid = Some(f);
                for p in prog.list(prog.func(f).params) {
                    if matches!(prog.pat(p).kind, PatKind::Assign { .. } | PatKind::Rest(_)) {
                        break;
                    }
                    len += 1;
                }
            }
        }
        self.def(ctor, self.k_length, Value::Number(len as f64), false, false, true)?;
        self.def(ctor, self.k_name, Value::String(name_str), false, false, true)?;
        self.def(ctor, self.k_prototype, Value::Object(proto), false, false, false)?;
        self.def(proto, self.k_constructor, Value::Object(ctor), true, false, true)?;
        // The constructor body and the field initializers run in the instance-side environment.
        self.env = env_inst;
        self.capture();
        self.env = env_c;

        let mut cd = ClassData { script: self.cur as u32, class: c, name: name_str, ctor_fid, derived, env: env_inst, inst_privs: Vec::new(), n_slots: 0, fields: Vec::new() };
        let mut static_privs: Vec<(ObjId, u32)> = Vec::new();
        let mut static_fields: Vec<Field> = Vec::new();
        for el in prog.list(class.elems) {
            self.tick()?;
            let e = *prog.elem(el);
            match e.kind {
                ElemKind::Method(f) | ElemKind::Getter(f) | ElemKind::Setter(f) => {
                    if Some(f) == ctor_fid {
                        continue;
                    }
                    let key = self.elem_key(e.key, env_c)?;
                    let prefix = match e.kind {
                        ElemKind::Getter(_) => "get ",
                        ElemKind::Setter(_) => "set ",
                        _ => "",
                    };
                    let fname = self.method_name(key, prefix)?;
                    self.env = if e.is_static { env_static } else { env_inst };
                    let made = self.make_closure(f, Some(fname), false);
                    self.env = env_c;
                    let Value::Object(fo) = made? else { return Err(unsupported("class method", e.span)) };
                    match key {
                        FKey::Prop(k) => {
                            let target = if e.is_static { ctor } else { proto };
                            let d = match e.kind {
                                ElemKind::Getter(_) => PropertyDescriptor { get: Some(Some(fo)), enumerable: Some(false), configurable: Some(true), ..Default::default() },
                                ElemKind::Setter(_) => PropertyDescriptor { set: Some(Some(fo)), enumerable: Some(false), configurable: Some(true), ..Default::default() },
                                _ => PropertyDescriptor { value: Some(Value::Object(fo)), writable: Some(true), enumerable: Some(false), configurable: Some(true), ..Default::default() },
                            };
                            h!(self, self.heap.define_own_property(target, k, &d));
                        }
                        FKey::Private(pn, n) => {
                            let d = match e.kind {
                                ElemKind::Getter(_) => PropertyDescriptor { get: Some(Some(fo)), enumerable: Some(false), configurable: Some(true), ..Default::default() },
                                ElemKind::Setter(_) => PropertyDescriptor { set: Some(Some(fo)), enumerable: Some(false), configurable: Some(true), ..Default::default() },
                                _ => PropertyDescriptor { value: Some(Value::Object(fo)), writable: Some(false), enumerable: Some(false), configurable: Some(false), ..Default::default() },
                            };
                            let at = if matches!(e.kind, ElemKind::Method(_)) { PN_METHOD } else { PN_ACCESSOR };
                            h!(self, self.heap.define_own_property(pn, at, &d));
                            let list = if e.is_static { &mut static_privs } else { &mut cd.inst_privs };
                            if !list.iter().any(|p| p.0 == pn) {
                                list.push((pn, n));
                            }
                        }
                    }
                }
                ElemKind::Field(init) => {
                    let key = self.elem_key(e.key, env_c)?;
                    let field = Field { key, init };
                    if e.is_static { static_fields.push(field) } else { cd.fields.push(field) }
                }
            }
        }
        cd.n_slots = cd.inst_privs.len() + cd.fields.iter().filter(|f| matches!(f.key, FKey::Private(..))).count();
        self.cls.classes.insert(ctor, Rc::new(cd));
        // The class name is bound before the static elements run.
        if let Some(n) = cname
            && let Some(i) = self.find_in(env_c, n)
        {
            let b = &mut self.envs[env_c as usize].bindings[i];
            b.flags |= INIT;
            b.value = Value::Object(ctor);
        }
        for (pn, n) in static_privs {
            self.add_slot(ctor, pn, n, Value::Undefined, 4)?;
        }
        if !static_fields.is_empty() {
            self.env = env_static;
            let r = self.run_fields(ctor, &static_fields, env_static, self.cur as u32);
            self.env = env_c;
            r?;
        }
        Ok(Value::Object(ctor))
    }

    /// Run the field initializers `fields` for `obj` in a new environment under `parent` where `this` is `obj`.
    /// The initializer nodes belong to `script`: it is selected while they run.
    fn run_fields(&mut self, obj: ObjId, fields: &[Field], parent: u32, script: u32) -> Res<()> {
        let saved = (self.env, self.strict);
        let env = self.new_env(parent, true)?;
        self.envs[env as usize].this = Some(Value::Object(obj));
        self.env = env;
        self.strict = true;
        let r = self.in_script(script, |it| {
            for f in fields {
                it.tick()?;
                it.define_field(obj, f)?;
            }
            Ok(())
        });
        (self.env, self.strict) = saved;
        self.release(env);
        r
    }

    fn define_field(&mut self, obj: ObjId, f: &Field) -> Res<()> {
        let v = match f.init {
            Some(x) => {
                let name = match f.key {
                    FKey::Prop(PropertyKey::Symbol(_)) => None,
                    FKey::Prop(k) => Some(self.key_str(k)?),
                    FKey::Private(_, n) => Some(self.name_str(n)?),
                };
                self.eval_named(x, name)?
            }
            None => Value::Undefined,
        };
        match f.key {
            FKey::Prop(k) => {
                if !h!(self, self.heap.define_own_property(obj, k, &PropertyDescriptor::data(v))) {
                    let k = self.key_text(k);
                    return Err(self.type_err(&format!("Cannot define property {k}, object is not extensible")));
                }
                Ok(())
            }
            FKey::Private(pn, n) => self.add_slot(obj, pn, n, v, 0),
        }
    }

    /// InitializeInstanceElements: private methods and accessors first, then the fields in order.
    fn init_instance(&mut self, ctor: ObjId, obj: ObjId) -> Res<()> {
        let Some(cd) = self.cls.classes.get(&ctor).cloned() else { return Ok(()) };
        if cd.n_slots == 0 && cd.fields.is_empty() {
            return Ok(());
        }
        for &(pn, n) in &cd.inst_privs {
            self.add_slot(obj, pn, n, Value::Undefined, cd.n_slots)?;
        }
        if cd.fields.is_empty() {
            return Ok(());
        }
        let cap = cd.n_slots;
        // Reserve the slot list once, with the exact size, so a field-heavy class does not grow it by doubling.
        if cap > 0 && !self.cls.privs.contains_key(&obj) && self.cls.n_slots + cap <= self.cls.max_slots {
            self.cls.privs.insert(obj, Vec::with_capacity(cap));
        }
        self.run_fields(obj, &cd.fields, cd.env, cd.script)
    }

    fn exec_class_decl(&mut self, c: ClassId) -> Res<()> {
        let v = self.eval_class(c, None)?;
        let Some(name) = self.prog.class(c).name else { return Err(unsupported("class declaration without a name", self.prog.class(c).span)) };
        let n = self.name_at(name);
        match self.find_in(self.env, n) {
            Some(i) => {
                let b = &mut self.envs[self.env as usize].bindings[i];
                b.flags |= INIT;
                b.value = v;
            }
            None => self.env_declare(self.env, n, INIT | MUT, v),
        }
        Ok(())
    }

    /// Hoist a class declaration like `let`: bound, uninitialized.
    fn declare_class(&mut self, c: ClassId) {
        if let Some(name) = self.prog.class(c).name {
            let n = self.name_at(name);
            self.env_declare(self.env, n, MUT, Value::Undefined);
        }
    }
}
