//! `AbortController` and `AbortSignal` (DOM Standard, "Aborting ongoing activities"). Logic follows Boa's `core/runtime/src/abort`
//! (Unlicense OR MIT) for the shape of the two classes, and the DOM Standard for everything Boa leaves out: the `abort` event
//! through the normal EventTarget dispatch, `onabort`, `AbortSignal.abort/timeout/any`, dependent signals and `{ signal }`.
//!
//! - A signal is a plain event target: an object in `State::by_obj` as `Target::Obj`, so `addEventListener`, `dispatchEvent` and
//!   the listener GC rules are the common ones. Its abort state lives in `Tables::sigs` (aborted, reason, dependents, abort
//!   algorithms). A controller is an object in `Tables::ctls` that keeps its signal.
//! - Signal abort (DOM Standard): set the reason of the signal and of its dependent signals, run the abort algorithms of the signal and
//!   then of the dependents, fire `abort` at the signal, then at each dependent. No event object is made when nobody listens.
//! - Abort algorithms are `Remove` (the `{ signal }` option of `addEventListener`: the listener is removed) and `Fetch` (a request
//!   still queued is rejected with the reason). A request that already runs its blocking I/O cannot be interrupted.
//! - `AbortSignal.timeout(ms)` queues a `Task::AbortTimeout` on the scheduler. A queued task roots its signal, so a timeout signal
//!   that nobody references still fires. `AbortSignal.any` keeps the dependents and sources as weak lists, as the standard says.
//! - The globals are accessors until the first read (`lazy`), so a realm that never reads them pays for two functions.
//! - Gates: a response body that is being read is not aborted (bodies are buffered). `request.signal` is a new signal that follows `init.signal`.

use crate::browser::*;
use bare_js::eval::Error;
use bare_js::eval::Interp;
use bare_js::heap::PropertyDescriptor;
use bare_js::value::{ObjId, PropertyKey, Value};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;

type V = R<Value>;

fn arg(a: &[Value], i: usize) -> Value {
    a.get(i).copied().unwrap_or(Value::Undefined)
}

pub(crate) enum Algo {
    /// Remove the listener `(target, id)`.
    Remove(Target, u64),
    /// Reject the fetch promise if its request is still queued.
    Fetch(Value),
}

pub(crate) struct Sig {
    aborted: bool,
    reason: Value,
    dependent: bool,
    /// Weak: the non-dependent signals that a dependent signal follows.
    sources: Vec<ObjId>,
    /// Weak: the dependent signals of a source.
    deps: Vec<ObjId>,
    algos: Vec<Algo>,
}

impl Sig {
    fn new() -> Self {
        Sig { aborted: false, reason: Value::Undefined, dependent: false, sources: Vec::new(), deps: Vec::new(), algos: Vec::new() }
    }
}

#[derive(Default)]
pub(crate) struct Tables {
    pub(crate) sigs: HashMap<ObjId, Sig>,
    ctls: HashMap<ObjId, ObjId>,
}

#[derive(Default)]
pub(crate) struct AbortHost {
    pub(crate) s: RefCell<Tables>,
    made: Cell<bool>,
    /// `(signal prototype, controller prototype, signal constructor, controller constructor)`.
    objs: RefCell<Option<[ObjId; 4]>>,
}

// ------------------------------------------------------------------------------------ GC hooks

pub(crate) fn roots(d: &Dom, out: &mut Vec<Value>) {
    if let Some(o) = *d.abort.objs.borrow() {
        out.extend(o.map(Value::Object));
    }
}

pub(crate) fn trace(d: &Dom, owner: ObjId, out: &mut Vec<Value>) {
    let t = d.abort.s.borrow();
    if let Some(s) = t.sigs.get(&owner) {
        out.push(s.reason);
        out.extend(s.algos.iter().filter_map(|a| if let Algo::Fetch(p) = a { Some(*p) } else { None }));
    } else if let Some(&s) = t.ctls.get(&owner) {
        out.push(Value::Object(s));
    }
}

/// Call after the listener table is pruned: an algorithm of a listener that is gone is dropped.
pub(crate) fn prune(d: &Dom, live: &dyn Fn(ObjId) -> bool) {
    let mut t = d.abort.s.borrow_mut();
    if t.sigs.is_empty() && t.ctls.is_empty() {
        return;
    }
    t.sigs.retain(|&o, _| live(o));
    t.ctls.retain(|&o, _| live(o));
    let st = d.st.borrow();
    for s in t.sigs.values_mut() {
        s.sources.retain(|&o| live(o));
        s.deps.retain(|&o| live(o));
        s.algos.retain(|a| match a {
            Algo::Remove(t, id) => st.listeners.get(t).is_some_and(|v| v.iter().any(|l| l.id == *id)),
            Algo::Fetch(_) => true,
        });
    }
}

// ------------------------------------------------------------------------------------ signals

impl Dom {
    pub(crate) fn is_signal(&self, o: ObjId) -> bool {
        self.abort.s.borrow().sigs.contains_key(&o)
    }

    /// Is `o` an `AbortController` (so not plain data for structured clone)?
    pub(crate) fn is_abort_controller(&self, o: ObjId) -> bool {
        self.abort.s.borrow().ctls.contains_key(&o)
    }

    fn signal_of(&self, it: &mut Interp<'_>, v: Value) -> R<ObjId> {
        match v {
            Value::Object(o) if self.is_signal(o) => Ok(o),
            _ => Err(it.type_error("Illegal invocation")),
        }
    }

    fn exc(&self, it: &mut Interp<'_>, name: &str, msg: &str) -> V {
        match self.dom_err(it, name, msg) {
            Error::Throw(v) => Ok(v),
            e => Err(e),
        }
    }

    fn new_signal(&self, it: &mut Interp<'_>) -> R<ObjId> {
        let proto = self.abort.objs.borrow().expect("made")[0];
        if self.st.borrow().by_obj.len() >= self.lim.max_wrappers {
            return Err(self.lim("dom-wrappers"));
        }
        let o = self.obj(it, proto)?;
        self.st.borrow_mut().by_obj.insert(o, Target::Obj(o));
        self.abort.s.borrow_mut().sigs.insert(o, Sig::new());
        Ok(o)
    }

    /// `(aborted, reason)` of a signal.
    pub(crate) fn signal_state(&self, o: ObjId) -> Option<(bool, Value)> {
        self.abort.s.borrow().sigs.get(&o).map(|s| (s.aborted, s.reason))
    }

    /// Add an abort algorithm. The signal must not be aborted (the caller checked).
    pub(crate) fn signal_add_algo(&self, o: ObjId, a: Algo) -> R<()> {
        let mut t = self.abort.s.borrow_mut();
        let st = self.st.borrow();
        let Some(s) = t.sigs.get_mut(&o) else { return Ok(()) };
        // Amortized compaction: drop the algorithms of listeners that `removeEventListener` took away.
        let n = s.algos.len();
        if n >= 32 && n.is_power_of_two() {
            s.algos.retain(|a| match a {
                Algo::Remove(t, id) => st.listeners.get(t).is_some_and(|v| v.iter().any(|l| l.id == *id)),
                Algo::Fetch(_) => true,
            });
        }
        if s.algos.len() >= self.lim.max_listeners {
            return Err(self.lim("abort-algorithms"));
        }
        s.algos.push(a);
        Ok(())
    }

    /// "Signal abort": the reason is `reason` or a new `AbortError`.
    pub(crate) fn signal_abort(&self, it: &mut Interp<'_>, o: ObjId, reason: Value) -> R<()> {
        let reason = match reason {
            Value::Undefined => None,
            v => Some(v),
        };
        if self.abort.s.borrow().sigs.get(&o).is_none_or(|s| s.aborted) {
            return Ok(());
        }
        let reason = match reason {
            Some(v) => v,
            None => self.exc(it, "AbortError", "signal is aborted without reason")?,
        };
        let (mut fire, mut algos): (Vec<ObjId>, Vec<Algo>) = (vec![o], Vec::new());
        {
            let mut t = self.abort.s.borrow_mut();
            let deps = {
                let s = t.sigs.get_mut(&o).expect("checked");
                (s.aborted, s.reason) = (true, reason);
                algos.extend(std::mem::take(&mut s.algos));
                s.deps.clone()
            };
            for dep in deps {
                if let Some(s) = t.sigs.get_mut(&dep)
                    && !s.aborted
                {
                    (s.aborted, s.reason) = (true, reason);
                    fire.push(dep);
                }
            }
        }
        // Abort steps first (the signal, then each dependent), then the events in the same order.
        for &s in &fire[1..] {
            algos.extend(std::mem::take(&mut self.abort.s.borrow_mut().sigs.get_mut(&s).expect("checked").algos));
        }
        for a in algos {
            match a {
                Algo::Remove(t, id) => self.remove_listener(t, id),
                Algo::Fetch(p) => crate::browser_fetch::abort_queued(self, it, p, reason)?,
            }
        }
        for s in fire {
            let listening = self.st.borrow().listeners.get(&Target::Obj(s)).is_some_and(|v| v.iter().any(|l| &*l.ty == "abort"));
            if listening {
                let ev = self.new_event(it, "abort", false, false, false, None)?;
                self.st.borrow_mut().events.get_mut(&ev).expect("just made").trusted = true;
                self.dispatch(it, Target::Obj(s), ev)?;
            }
        }
        Ok(())
    }

    /// The task of `AbortSignal.timeout`.
    pub(crate) fn abort_timeout_fired(&self, it: &mut Interp<'_>, o: ObjId) -> R<()> {
        let r = self.exc(it, "TimeoutError", "signal timed out")?;
        self.signal_abort(it, o, r)
    }
}

// ------------------------------------------------------------------------------------ methods

fn illegal(_: &Dom, it: &mut Interp<'_>, _: Value, _: &[Value]) -> V {
    Err(it.type_error("Illegal constructor"))
}

fn aborted(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let o = d.signal_of(it, this)?;
    Ok(Value::Bool(d.signal_state(o).is_some_and(|s| s.0)))
}

fn reason(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let o = d.signal_of(it, this)?;
    Ok(d.signal_state(o).map_or(Value::Undefined, |s| if s.0 { s.1 } else { Value::Undefined }))
}

fn throw_if_aborted(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let o = d.signal_of(it, this)?;
    match d.signal_state(o) {
        Some((true, r)) => Err(Error::Throw(r)),
        _ => Ok(Value::Undefined),
    }
}

fn static_abort(d: &Dom, it: &mut Interp<'_>, _: Value, a: &[Value]) -> V {
    let o = d.new_signal(it)?;
    d.signal_abort(it, o, arg(a, 0))?;
    Ok(Value::Object(o))
}

fn static_timeout(d: &Dom, it: &mut Interp<'_>, _: Value, a: &[Value]) -> V {
    if a.is_empty() {
        return Err(it.type_error("Failed to execute 'timeout' on 'AbortSignal': 1 argument required, but only 0 present."));
    }
    let n = it.to_js_number(a[0])?;
    if !n.is_finite() || n < 0.0 || n > 18446744073709551615.0 {
        return Err(it.type_error("Failed to execute 'timeout' on 'AbortSignal': Value is outside the range of an unsigned long long."));
    }
    let o = d.new_signal(it)?;
    d.queue_abort_timeout(o, n.trunc())?;
    Ok(Value::Object(o))
}

fn static_any(d: &Dom, it: &mut Interp<'_>, _: Value, a: &[Value]) -> V {
    let bad = "Failed to execute 'any' on 'AbortSignal': The provided value is not of type 'AbortSignal'.";
    let Some(mut iter) = it.host_iter_open(arg(a, 0))? else { return Err(it.type_error("Failed to execute 'any' on 'AbortSignal': The provided value cannot be converted to a sequence.")) };
    let mut inputs = Vec::new();
    loop {
        it.charge(1)?;
        match it.host_iter_next(&mut iter) {
            Ok(Some(Value::Object(o))) if d.is_signal(o) => inputs.push(o),
            Ok(Some(_)) => {
                let e = it.type_error(bad);
                return Err(it.host_iter_fail(&mut iter, e));
            }
            Ok(None) => break,
            Err(e) => return Err(e),
        }
        if inputs.len() > d.lim.max_listeners {
            return Err(d.lim("abort-any-sources"));
        }
    }
    Ok(Value::Object(d.any_of(it, inputs)?))
}

impl Dom {
    /// `AbortSignal.any(inputs)`.
    fn any_of(&self, it: &mut Interp<'_>, inputs: Vec<ObjId>) -> R<ObjId> {
        let result = self.new_signal(it)?;
        let mut t = self.abort.s.borrow_mut();
        if let Some(&first) = inputs.iter().find(|&&i| t.sigs[&i].aborted) {
            let r = t.sigs[&first].reason;
            let s = t.sigs.get_mut(&result).expect("just made");
            (s.aborted, s.reason) = (true, r);
            return Ok(result);
        }
        let mut sources: Vec<ObjId> = Vec::new();
        for i in inputs {
            let s = &t.sigs[&i];
            let from: Vec<ObjId> = if s.dependent { s.sources.clone() } else { vec![i] };
            for src in from {
                if !sources.contains(&src) {
                    sources.push(src);
                }
            }
        }
        for &src in &sources {
            t.sigs.get_mut(&src).expect("source is a signal").deps.push(result);
        }
        let s = t.sigs.get_mut(&result).expect("just made");
        (s.dependent, s.sources) = (true, sources);
        Ok(result)
    }

    /// A new signal that follows `s` (the `signal` of a `Request`).
    pub(crate) fn follow_signal(&self, it: &mut Interp<'_>, s: ObjId) -> R<ObjId> {
        self.any_of(it, vec![s])
    }

    /// A new signal that nothing aborts yet.
    pub(crate) fn fresh_signal(&self, it: &mut Interp<'_>) -> R<ObjId> {
        self.ensure_abort(it)?;
        self.new_signal(it)
    }

    fn ensure_abort(&self, it: &mut Interp<'_>) -> R<()> {
        if !self.abort.made.get() {
            make(self, it)?;
        }
        Ok(())
    }
}

fn controller_ctor(d: &Dom, it: &mut Interp<'_>, _: Value, _: &[Value]) -> V {
    let proto = d.abort.objs.borrow().expect("made")[1];
    let sig = d.new_signal(it)?;
    let c = d.obj(it, proto)?;
    d.abort.s.borrow_mut().ctls.insert(c, sig);
    Ok(Value::Object(c))
}

fn controller_of(d: &Dom, it: &mut Interp<'_>, this: Value) -> R<ObjId> {
    match this {
        Value::Object(o) => match d.abort.s.borrow().ctls.get(&o) {
            Some(&s) => Ok(s),
            None => Err(it.type_error("Illegal invocation")),
        },
        _ => Err(it.type_error("Illegal invocation")),
    }
}

fn controller_signal(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    Ok(Value::Object(controller_of(d, it, this)?))
}

fn controller_abort(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let s = controller_of(d, it, this)?;
    d.signal_abort(it, s, arg(a, 0))?;
    Ok(Value::Undefined)
}

// ------------------------------------------------------------------------------------ install

/// The two globals are accessors until one is read. A call with an argument is the setter.
fn lazy<const SIG: bool>(d: &Dom, it: &mut Interp<'_>, _: Value, a: &[Value]) -> V {
    let g = *d.global.get().expect("installed");
    let k = d.key(it, if SIG { "AbortSignal" } else { "AbortController" })?;
    if let Some(&v) = a.first() {
        it.define_data(g, k, v, true, false, true)?;
        return Ok(Value::Undefined);
    }
    d.ensure_abort(it)?;
    let ctor = d.abort.objs.borrow().expect("made")[if SIG { 2 } else { 3 }];
    it.define_data(g, k, Value::Object(ctor), true, false, true)?;
    Ok(Value::Object(ctor))
}

#[cold]
#[inline(never)]
fn make(d: &Dom, it: &mut Interp<'_>) -> R<()> {
    let g = *d.global.get().expect("installed");
    let (op, et) = (it.object_prototype(), d.p().et);
    let (sp, cp) = (d.obj(it, et)?, d.obj(it, op)?);
    let (kp, kc) = (d.key(it, "prototype")?, d.key(it, "constructor")?);
    let sc = d.add_fn(it, illegal, "AbortSignal", 0, true)?;
    let cc = d.add_fn(it, controller_ctor, "AbortController", 0, true)?;
    for (f, proto, tag) in [(sc, sp, "AbortSignal"), (cc, cp, "AbortController")] {
        it.define_data(f, kp, Value::Object(proto), false, false, false)?;
        it.define_data(proto, kc, Value::Object(f), true, false, true)?;
        let (ks, kt) = (d.key(it, "Symbol")?, d.key(it, "toStringTag")?);
        let sy = it.get_value(Value::Object(g), ks)?;
        if let Value::Symbol(s) = it.get_value(sy, kt)? {
            let t = d.js_str(it, tag)?;
            it.define_data(proto, PropertyKey::Symbol(s), t, false, false, true)?;
        }
    }
    d.def_get(it, sp, "aborted", aborted, None)?;
    d.def_get(it, sp, "reason", reason, None)?;
    d.def_method(it, sp, "throwIfAborted", 0, throw_if_aborted)?;
    crate::browser_handlers::install_onabort(d, it, sp)?;
    d.def_method(it, sc, "abort", 0, static_abort)?;
    d.def_method(it, sc, "timeout", 1, static_timeout)?;
    d.def_method(it, sc, "any", 1, static_any)?;
    d.def_get(it, cp, "signal", controller_signal, None)?;
    d.def_method(it, cp, "abort", 0, controller_abort)?;
    *d.abort.objs.borrow_mut() = Some([sp, cp, sc, cc]);
    d.abort.made.set(true);
    Ok(())
}

pub(crate) fn install(d: &Dom, it: &mut Interp<'_>) -> R<()> {
    let g = *d.global.get().expect("installed");
    for (name, f) in [("AbortController", lazy::<false> as H), ("AbortSignal", lazy::<true> as H)] {
        let key = d.key(it, name)?;
        let f = d.add_fn(it, f, name, 0, false)?;
        let desc = PropertyDescriptor { get: Some(Some(f)), set: Some(Some(f)), enumerable: Some(false), configurable: Some(true), ..Default::default() };
        if let Err(e) = it.heap.define_own_property(g, key, &desc) {
            return Err(it.heap_error(e));
        }
    }
    Ok(())
}
