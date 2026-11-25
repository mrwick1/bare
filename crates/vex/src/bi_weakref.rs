//! `WeakRef` and `FinalizationRegistry`. Safe code over [`Realm`]; the collector hooks are three methods of [`WeakRefs`]. A page that makes neither
//! has no list to walk: each hook returns at its first test.
//!
//! **`WeakRef`.** A plain `Object` cell of the kind `K_WEAKREF` with one hidden slot, `\0wr`. The slot holds a [`WeakTab`] of one entry (32 bytes) whose
//! key is the target. The collector already reads such a table as an ephemeron table (`gc.rs`): the key is no edge, and a key that no one marked is
//! cleared after the marking. So `deref` reads the key, and the marker, the weak hook and the roots have no code for a ref. The cost of a ref is its
//! object and 32 bytes, and the cost of a collection is the one table that the marker traces.
//!
//! **KeepDuringJob.** `new WeakRef(t)` and `deref()` put the target in [`WeakRefs::kept`] (a set by address: a loop that reads a ref a million times
//! keeps one entry). The collector marks the set as roots, because a collection can run inside a task. [`Realm::end_job`] empties it. It runs at the end
//! of a microtask checkpoint (`Realm::drain_jobs`: the host calls one at the end of each task), and before a collection that the host asks for between
//! tasks (`raw_collect`, `collect_idle`).
//!
//! **`FinalizationRegistry`.** An object of the kind `K_FINREG` whose slot 0 is the index of its record in [`WeakRefs::regs`]. A record has the callback and
//! the cells of its `register` calls: the target and the token (both weak) and the held value. They are not in any cell of the heap, so the marker has
//! no code for them:
//!
//! - the owners hook ([`WeakRefs::trace_owners`], the same rounds as the owners of the host) marks the callback and the held values of each record whose
//!   registry is marked. A held value that names its target keeps the target; a registry that only another registry's held value reaches is found in
//!   a later round;
//! - the weak hook ([`WeakRefs::weak`]) runs after the marking. It drops the record of a registry that is not marked (it calls nothing, as the
//!   specification says), sets the token that is dead to `undefined`, and sets the target that is dead to `undefined` too. A cell with no target is due:
//!   it stays in its record, with its held value marked, until [`Realm::run_finalizers`] runs it or `unregister` removes it.
//!
//! **Cleanup.** A collection never runs script. [`Realm::finalizers_due`] says that a cell is due. The host calls [`Realm::run_finalizers`] as a task of its
//! own (`HostEnqueueFinalizationRegistryCleanupJob`): it takes the due cells of all records, in the order of the records and then of `register`, roots
//! them in [`WeakRefs::run`], and calls each callback once from the top with the held value. A throw goes to `on_error`, and the rest still run. The
//! budget ending stops the run and drops the rest.
//!
//! **Gates.** A target or a token must be an object. The specification (and Node 22) also takes a symbol that `Symbol.for` did not make. Vex does not,
//! as for `WeakMap`: it is a `TypeError`.
use crate::bi_symbol::TO_STRING_TAG;
use crate::builtins::arg;
use crate::cell::Layout;
use crate::gc::{AddrHasher, Marker};
use crate::interp::{STOP, Vm};
use crate::interp_rt::{Key, R, Realm};
use crate::lazy::{Ent, m};
use crate::object::{K_FINREG, K_WEAKREF, Obj};
use crate::promise::DrainStop;
use crate::shapes::{A_C, ShapeId};
use crate::string::Str;
use crate::value::Value;
use crate::weaktab::{WeakTab, weaktab_of};
use std::collections::HashMap;
use std::hash::BuildHasherDefault;

/// One `register` call. `target` and `token` are `undefined` once dead (and `token` when there was none).
struct Cell {
    target: Value,
    held: Value,
    token: Value,
}

/// A registry: its object, which the marker does not trace from here, its callback and its cells.
struct Reg {
    obj: Value,
    cb: Value,
    cells: Vec<Cell>,
}

/// The state of the two built-ins in a realm. The hooks take `live` (`gc::Live::value`, called in `Realm::collect`, which may hold `unsafe`): a pointer in a
/// record was a marked cell at the last collection, and that collection cleared it when it was not marked, so each one is a cell that is not free. The prototypes are roots (`keep`) and the shapes are pinned.
pub(crate) struct WeakRefs {
    protos: [Obj; 2],
    shapes: [ShapeId; 2],
    key: Str,
    /// The targets that the job keeps alive, by address.
    pub(crate) kept: HashMap<usize, Value, BuildHasherDefault<AddrHasher>>,
    /// The records of the registries, by the index in slot 0 of the registry. `None` is a free index.
    regs: Vec<Option<Reg>>,
    free: Vec<u32>,
    /// The records that are not `None`.
    live: usize,
    /// The records that the owner rounds of this collection have not reached yet.
    waiting: Vec<u32>,
    /// The cells that have no target.
    pending: usize,
    /// (callback, held value) pairs of the cleanup that runs now: roots.
    run: Vec<Value>,
}

impl WeakRefs {
    pub(crate) fn mark(&self, m: &mut Marker) {
        self.kept.values().for_each(|v| m.value(*v));
        m.values(&self.run);
    }

    pub(crate) fn trace_owners(&mut self, live: impl Fn(Value) -> bool, round: u32, out: &mut Vec<Value>) -> usize {
        if self.live == 0 {
            return 0;
        }
        if round == 0 {
            self.waiting.clear();
            self.waiting.extend(self.regs.iter().enumerate().filter(|(_, r)| r.is_some()).map(|(i, _)| i as u32));
        }
        let (regs, before) = (&self.regs, self.waiting.len());
        self.waiting.retain(|&i| {
            let r = regs[i as usize].as_ref().expect("a record");
            if live(r.obj) {
                out.push(r.cb);
                out.extend(r.cells.iter().map(|c| c.held));
                false
            } else {
                true
            }
        });
        before - self.waiting.len()
    }

    pub(crate) fn weak(&mut self, live: impl Fn(Value) -> bool) {
        self.waiting = Vec::new();
        if self.live == 0 {
            return;
        }
        let dead = |v: Value| !live(v);
        for (i, slot) in self.regs.iter_mut().enumerate() {
            let Some(r) = slot else { continue };
            if dead(r.obj) {
                self.pending -= r.cells.iter().filter(|c| c.target.as_pointer().is_none()).count();
                *slot = None;
                self.free.push(i as u32);
                self.live -= 1;
                continue;
            }
            for c in &mut r.cells {
                if dead(c.token) {
                    c.token = Value::UNDEFINED;
                }
                if dead(c.target) {
                    c.target = Value::UNDEFINED;
                    self.pending += 1;
                }
            }
        }
    }
}

/// The text of a value in the message of a `TypeError`, as V8 shows it.
fn show(rt: &Realm, v: Value) -> String {
    if let Some(s) = Realm::str_of(v) {
        return rt.text(s);
    }
    if let Some(s) = Realm::sym_of(v) {
        return format!("Symbol({})", rt.text(s));
    }
    match (v.as_int(), v.as_f64(), v.as_bool()) {
        (Some(i), ..) => i.to_string(),
        (_, Some(f), _) => Realm::num_to_string(f),
        (.., Some(b)) => b.to_string(),
        _ if v.is_undefined() => "undefined".into(),
        _ if v.is_null() => "null".into(),
        _ => "#<Object>".into(),
    }
}

impl Realm {
    fn wr(&mut self) -> &mut WeakRefs {
        self.wr.as_mut().expect("installed")
    }

    /// The end of a job: the targets of `WeakRef` need no keeping any more.
    #[inline]
    pub(crate) fn end_job(&mut self) {
        if let Some(w) = &mut self.wr
            && !w.kept.is_empty()
        {
            // A job that kept many targets does not leave the room.
            if w.kept.capacity() > 256 { w.kept = Default::default() } else { w.kept.clear() }
        }
    }

    fn keep_target(&mut self, v: Value) {
        if let Some((_, p)) = v.as_pointer() {
            self.wr().kept.insert(p.as_ptr().addr(), v);
        }
    }

    /// Is a cleanup callback due ([`Realm::run_finalizers`])?
    pub fn finalizers_due(&self) -> bool {
        self.wr.as_ref().is_some_and(|w| w.pending > 0)
    }

    /// Run the cleanup callbacks that are due, from the top (no native on the Rust stack), each once with its held value. The host calls it as a task, after
    /// a collection. A throw goes to `on_error`, and the run goes on. `Err(STOP)` is the budget; the callbacks that did not run are dropped. Gives the
    /// number of callbacks that were called.
    pub fn run_finalizers(&mut self, on_error: &mut dyn FnMut(&mut Realm, Value)) -> R<u32> {
        let Some(w) = self.wr.as_mut().filter(|w| w.pending > 0) else { return Ok(0) };
        let WeakRefs { regs, run, .. } = &mut **w;
        for r in regs.iter_mut().flatten() {
            let cb = r.cb;
            r.cells.retain(|c| {
                let due = c.target.as_pointer().is_none();
                if due {
                    run.extend([cb, c.held]);
                }
                !due
            });
        }
        w.pending = 0;
        let mut n = 0;
        let mut out = Ok(());
        while 2 * (n as usize) < self.wr().run.len() {
            let at = 2 * n as usize;
            let (cb, held) = (self.wr().run[at], self.wr().run[at + 1]);
            n += 1;
            match self.job_call(cb, Value::UNDEFINED, &[held]) {
                Ok(_) => {}
                Err(STOP) => {
                    out = Err(STOP);
                    break;
                }
                Err(e) => on_error(self, e),
            }
        }
        self.wr().run.clear();
        out.map(|()| n)
    }

    fn wr_shape(&mut self, proto: Obj, key: Str) -> ShapeId {
        let s = self.shapes.root(Layout::Object, Some(proto.cell()), 1);
        self.shapes.add(s, key, 0).expect("shape")
    }

    /// An object of `kind` over `proto` whose slot 0 is `v`.
    fn new_weak_obj(&mut self, kind: u32, proto: Obj, v: Value) -> Obj {
        let i = (kind - K_WEAKREF) as usize;
        let w = self.wr();
        let (shape, key) = if proto == w.protos[i] { (Some(w.shapes[i]), w.key) } else { (None, w.key) };
        let shape = shape.unwrap_or_else(|| self.wr_shape(proto, key));
        let o = Obj::new(&mut self.heap, &self.shapes, shape).expect("memory");
        o.set_kind(kind);
        o.set_slot(&self.shapes, 0, v);
        o
    }

    fn bad_receiver<T>(&mut self, what: &str, this: Value) -> R<T> {
        let t = show(self, this);
        self.type_error(&format!("Method {what} called on incompatible receiver {t}"))
    }

    fn this_kind(&self, this: Value, kind: u32) -> Option<Obj> {
        Realm::obj(this).filter(|o| o.is_kind(kind))
    }
}

fn weakref_ctor(rt: &mut Realm, _this: Value, args: &[Value]) -> R<Value> {
    crate::bi_collections::require_new(rt, "WeakRef")?;
    let target = arg(args, 0);
    if Realm::obj(target).is_none() {
        return rt.type_error("WeakRef: invalid target");
    }
    let proto = rt.new_proto()?;
    let tab = WeakTab::one(&mut rt.heap, target).expect("memory");
    let o = rt.new_weak_obj(K_WEAKREF, proto, tab.value());
    rt.keep_target(target);
    Ok(o.value())
}

fn weakref_deref(rt: &mut Realm, this: Value, _args: &[Value]) -> R<Value> {
    let Some(o) = rt.this_kind(this, K_WEAKREF) else { return rt.bad_receiver("WeakRef.prototype.deref", this) };
    let t = weaktab_of(o.slot(&rt.shapes, 0)).expect("a ref has its table").target();
    rt.keep_target(t);
    Ok(t)
}

fn registry_ctor(rt: &mut Realm, _this: Value, args: &[Value]) -> R<Value> {
    crate::bi_collections::require_new(rt, "FinalizationRegistry")?;
    let cb = arg(args, 0);
    if !Realm::obj(cb).is_some_and(|o| o.is_callable()) {
        return rt.type_error("FinalizationRegistry: cleanup must be callable");
    }
    let proto = rt.new_proto()?;
    let o = rt.new_weak_obj(K_FINREG, proto, Value::UNDEFINED);
    let w = rt.wr();
    let rec = Reg { obj: o.value(), cb, cells: Vec::new() };
    let i = match w.free.pop() {
        Some(i) => {
            w.regs[i as usize] = Some(rec);
            i
        }
        None => {
            w.regs.push(Some(rec));
            (w.regs.len() - 1) as u32
        }
    };
    w.live += 1;
    o.set_slot(&rt.shapes, 0, Value::int(i as i32));
    Ok(o.value())
}

/// The record of the registry `this`.
fn record<'a>(rt: &'a mut Realm, this: Value, what: &str) -> R<&'a mut Reg> {
    let Some(o) = rt.this_kind(this, K_FINREG) else { return rt.bad_receiver(what, this) };
    let i = o.slot(&rt.shapes, 0).as_int().expect("a registry has its index") as usize;
    Ok(rt.wr().regs[i].as_mut().expect("a record"))
}

fn token_error<T>(rt: &mut Realm, t: Value) -> R<T> {
    let s = show(rt, t);
    rt.type_error(&format!("Invalid unregisterToken ('{s}')"))
}

fn registry_register(rt: &mut Realm, this: Value, args: &[Value]) -> R<Value> {
    record(rt, this, "FinalizationRegistry.prototype.register")?;
    let (target, held, token) = (arg(args, 0), arg(args, 1), arg(args, 2));
    if Realm::obj(target).is_none() {
        return rt.type_error("FinalizationRegistry.prototype.register: invalid target");
    }
    if target == held {
        return rt.type_error("FinalizationRegistry.prototype.register: target and holdings must not be same");
    }
    if !token.is_undefined() && Realm::obj(token).is_none() {
        return token_error(rt, token);
    }
    record(rt, this, "")?.cells.push(Cell { target, held, token });
    Ok(Value::UNDEFINED)
}

fn registry_unregister(rt: &mut Realm, this: Value, args: &[Value]) -> R<Value> {
    record(rt, this, "FinalizationRegistry.prototype.unregister")?;
    let token = arg(args, 0);
    if Realm::obj(token).is_none() {
        return token_error(rt, token);
    }
    let r = record(rt, this, "")?;
    let before = r.cells.len();
    let mut dead = 0;
    r.cells.retain(|c| {
        let hit = c.token == token;
        dead += (hit && c.target.as_pointer().is_none()) as usize;
        !hit
    });
    let hit = r.cells.len() != before;
    rt.wr().pending -= dead;
    Ok(Value::bool(hit))
}

impl Realm {
    pub(crate) fn install_weakrefs(&mut self) {
        let key = self.intern("\0wr");
        self.keep(key.value());
        let protos: [Obj; 2] = std::array::from_fn(|_| Obj::new(&mut self.heap, &self.shapes, self.s_object).expect("memory"));
        protos.iter().for_each(|p| self.keep(p.value()));
        let shapes = protos.map(|p| self.wr_shape(p, key));
        self.pins.extend(shapes);
        self.wr = Some(Box::new(WeakRefs {
            protos,
            shapes,
            key,
            kept: Default::default(),
            regs: Vec::new(),
            free: Vec::new(),
            live: 0,
            waiting: Vec::new(),
            pending: 0,
            run: Vec::new(),
        }));
        let tag = Key::Name(self.wk[TO_STRING_TAG]);
        for (p, name) in protos.into_iter().zip(["WeakRef", "FinalizationRegistry"]) {
            let v = self.string(name);
            self.define_attrs(p, tag, A_C, v).expect("define");
        }
        self.ctor("WeakRef", 1, weakref_ctor, protos[0]);
        self.ctor("FinalizationRegistry", 1, registry_ctor, protos[1]);
        static WEAKREF_PROTO: &[Ent] = &[m("deref", 0, weakref_deref)];
        static REGISTRY_PROTO: &[Ent] = &[m("register", 2, registry_register), m("unregister", 1, registry_unregister)];
        self.lazy_table(protos[0], WEAKREF_PROTO);
        self.lazy_table(protos[1], REGISTRY_PROTO);
    }
}

impl Vm {
    /// `Realm::run_finalizers` with the budget of the machine, for tests and for a host that has no `Engine` (the browser uses `Realm::run_finalizers_host`).
    pub fn run_finalizers(&mut self, on_error: &mut dyn FnMut(&mut Realm, Value)) -> Result<u32, DrainStop> {
        self.realm.fuel = self.fuel;
        let r = self.realm.run_finalizers(on_error);
        self.fuel = self.realm.fuel;
        r.map_err(|_| DrainStop::Fuel)
    }
}
