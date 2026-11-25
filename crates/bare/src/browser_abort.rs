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
//!   that nobody references still fires. `AbortSignal.any` keeps the dependents and sources as weak lists of host keys, as the standard says. A signal and a controller are owners (`Owner::Abort`, `Owner::Ctl`): their edges are `Traced`, and `prune` drops the entries of dead keys.
//! - The globals are accessors until the first read (`lazy`), so a realm that never reads them pays for two functions.
//! - Gates: a response body that is being read is not aborted (bodies are buffered). `request.signal` is a new signal that follows `init.signal`.

use crate::eng::{Eng, Fault, Ops, Val};
use crate::browser::*;
use crate::browser_scoped::*;
use bare_js_api::{Args, Attrs, Engine, HostFn, Intrinsic, Local};
use std::cell::{Cell, RefCell};

type V<E: Eng> = R<E, E::Raw>;

pub(crate) enum Algo<E: Eng> {
    /// Remove the listener `(target, id)`.
    Remove(Target<E>, u64),
    /// Reject the fetch promise if its request is still queued. The signal holds the promise: it is an edge of the signal.
    Fetch(bare_js_api::Traced<E::Raw>),
}

pub(crate) struct Sig<E: Eng> {
    aborted: bool,
    reason: bare_js_api::Traced<E::Raw>,
    dependent: bool,
    /// The host key of the owner (`Owner::Abort`).
    key: u32,
    /// Weak, by host key: the non-dependent signals that a dependent signal follows.
    sources: Vec<u32>,
    /// Weak, by host key: the dependent signals of a source.
    deps: Vec<u32>,
    algos: Vec<Algo<E>>,
}

impl<E: Eng> Sig<E> {
    fn new(it: &E::Vm, key: u32) -> Self {
        Sig { aborted: false, reason: it.trace_value(E::undef()), dependent: false, key, sources: Vec::new(), deps: Vec::new(), algos: Vec::new() }
    }
}

#[derive(Default)]
pub(crate) struct Tables<E: Eng> {
    pub(crate) sigs: IdMap<<E as Eng>::Obj, Sig<E>>,
    /// A controller: its host key (`Owner::Ctl`) and its signal (an edge of the controller).
    ctls: IdMap<<E as Eng>::Obj, (u32, bare_js_api::Traced<<E as Eng>::Raw>)>,
}

#[derive(Default)]
pub(crate) struct AbortHost<E: Eng> {
    pub(crate) s: RefCell<Tables<E>>,
    made: Cell<bool>,
    /// `(signal prototype, controller prototype, signal constructor, controller constructor)`.
    objs: RefCell<Option<[<E as Eng>::Obj; 4]>>,
    /// Keep those objects alive for the life of the realm.
    held: RefCell<Vec<bare_js_api::Persistent>>,
}

// ------------------------------------------------------------------------------------ GC hooks

/// The references of the owner `Owner::Abort(owner)`, a signal.
pub(crate) fn trace<E: Eng>(d: &Dom<E>, owner: E::Obj, t: &mut bare_js_api::Tracer<'_, E::Raw>) {
    if let Some(s) = d.abort.s.borrow().sigs.get(&owner) {
        t.visit(&s.reason);
        for a in &s.algos {
            if let Algo::Fetch(p) = a {
                t.visit(p);
            }
        }
    }
}

/// The references of the owner `Owner::Ctl(owner)`, a controller.
pub(crate) fn trace_ctl<E: Eng>(d: &Dom<E>, owner: E::Obj, t: &mut bare_js_api::Tracer<'_, E::Raw>) {
    if let Some((_, s)) = d.abort.s.borrow().ctls.get(&owner) {
        t.visit(s);
    }
}

/// Call after the dead keys are released. One pass over the signals: the
/// entries of dead owners go (their key is free), a live signal drops the dead keys of its weak lists and the algorithms of a
/// listener that is gone.
pub(crate) fn prune<E: Eng>(d: &Dom<E>, st: &State<E>) {
    let mut t = d.abort.s.borrow_mut();
    let alive = |k: u32| st.owners.get(k).is_some();
    t.ctls.retain(|_, (k, _)| alive(*k));
    t.sigs.retain(|_, s| {
        if !alive(s.key) {
            return false;
        }
        s.sources.retain(|&k| alive(k));
        s.deps.retain(|&k| alive(k));
        s.algos.retain(|a| match a {
            Algo::Remove(t, id) => st.listeners.get(t).is_some_and(|v| v.iter().any(|l| l.id == *id)),
            Algo::Fetch(_) => true,
        });
        true
    });
}

/// The host key of `o` when it is a signal (`Dom::own`).
pub(crate) fn key_of<E: Eng>(d: &Dom<E>, o: E::Obj) -> Option<u32> {
    d.abort.s.borrow().sigs.get(&o).map(|s| s.key)
}

// ------------------------------------------------------------------------------------ signals

impl<E: Eng> Dom<E> {
    /// The entries of signals and controllers, and the keys in the weak lists of all signals.
    #[cfg(test)]
    pub(crate) fn abort_entries(&self) -> (usize, usize, usize) {
        let t = self.abort.s.borrow();
        (t.sigs.len(), t.ctls.len(), t.sigs.values().map(|s| s.sources.len() + s.deps.len()).sum())
    }

    pub(crate) fn is_signal(&self, o: E::Obj) -> bool {
        self.abort.s.borrow().sigs.contains_key(&o)
    }

    /// Is `o` an `AbortController` (so not plain data for structured clone)?
    pub(crate) fn is_abort_controller(&self, o: E::Obj) -> bool {
        self.abort.s.borrow().ctls.contains_key(&o)
    }

    fn exc(&self, it: &mut E::Vm, name: &str, msg: &str) -> V<E> {
        match self.dom_err(it, name, msg) {
            Fault::Throw(v) => Ok(v),
            e => Err(e),
        }
    }

    fn new_signal(&self, it: &mut E::Vm) -> R<E, E::Obj> {
        let proto = self.abort.objs.borrow().expect("made")[0];
        let o = self.obj(it, proto)?;
        let mut st = self.st.borrow_mut();
        st.by_obj.insert(o, Target::Obj(o));
        let key = self.abort_owner(&mut st, it, o);
        self.abort.s.borrow_mut().sigs.insert(o, Sig::new(it, key));
        Ok(o)
    }

    /// Make `o` an owner with a new key.
    fn abort_owner(&self, st: &mut State<E>, it: &mut E::Vm, o: E::Obj) -> u32 {
        let k = st.owners.key(Owner::Abort(o));
        it.set_host_owner_obj(o, k);
        k
    }

    /// `(aborted, reason)` of a signal.
    pub(crate) fn signal_state(&self, o: E::Obj) -> Option<(bool, E::Raw)> {
        self.abort.s.borrow().sigs.get(&o).map(|s| (s.aborted, *s.reason.raw()))
    }

    /// Add an abort algorithm. The signal must not be aborted (the caller checked).
    pub(crate) fn signal_add_algo(&self, o: E::Obj, a: Algo<E>) -> R<E, ()> {
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
        s.algos.push(a);
        Ok(())
    }

    /// "Signal abort": the reason is `reason` or a new `AbortError`.
    pub(crate) fn signal_abort(&self, it: &mut E::Vm, o: E::Obj, reason: E::Raw) -> R<E, ()> {
        let reason = match E::kind(reason) {
            Val::Undefined => None,
            _ => Some(reason),
        };
        if self.abort.s.borrow().sigs.get(&o).is_none_or(|s| s.aborted) {
            return Ok(());
        }
        let reason = match reason {
            Some(v) => v,
            None => self.exc(it, "AbortError", "signal is aborted without reason")?,
        };
        let (mut fire, mut algos): (Vec<E::Obj>, Vec<Algo<E>>) = (vec![o], Vec::new());
        {
            let mut t = self.abort.s.borrow_mut();
            let deps = {
                let s = t.sigs.get_mut(&o).expect("checked");
                s.aborted = true;
                it.store_value(&mut s.reason, reason);
                algos.extend(std::mem::take(&mut s.algos));
                s.deps.clone()
            };
            let st = self.st.borrow();
            for dep in deps {
                if let Some(Owner::Abort(dep)) = st.owners.get(dep)
                    && let Some(s) = t.sigs.get_mut(&dep)
                    && !s.aborted
                {
                    s.aborted = true;
                    it.store_value(&mut s.reason, reason);
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
                Algo::Fetch(p) => {
                    let p = it.traced_value(&p);
                    crate::browser_fetch::abort_queued(self, it, p, reason)?
                }
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
    pub(crate) fn abort_timeout_fired(&self, it: &mut E::Vm, o: E::Obj) -> R<E, ()> {
        let r = self.exc(it, "TimeoutError", "signal timed out")?;
        self.signal_abort(it, o, r)
    }
}

// ------------------------------------------------------------------------------------ methods

fn signal_illegal<'s, E: Eng>(_: &Dom<E>, s: &mut Sc<'s, E>, _: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    Err(s.type_error("Failed to construct 'AbortSignal': Illegal constructor"))
}

impl<E: Eng> Dom<E> {
    fn signal_of_s<'s>(&self, s: &mut Sc<'s, E>, v: Local<'s>) -> Res<'s, E::Obj> {
        match obj_id::<E>(s, v) {
            Some(o) if self.is_signal(o) => Ok(o),
            _ => Err(s.type_error("Illegal invocation")),
        }
    }
}

fn aborted<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let o = d.signal_of_s(s, this)?;
    Ok(s.boolean(d.signal_state(o).is_some_and(|s| s.0)))
}

fn reason<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let o = d.signal_of_s(s, this)?;
    match d.signal_state(o) {
        Some((true, r)) => Ok(s.engine().raw_local(&r)),
        _ => Ok(s.undefined()),
    }
}

fn throw_if_aborted<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let o = d.signal_of_s(s, this)?;
    match d.signal_state(o) {
        Some((true, r)) => Err(bare_js_api::Error::Throw(s.engine().raw_local(&r))),
        _ => Ok(s.undefined()),
    }
}

fn static_abort<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, _: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let o = old::<E, _>(s, |it| d.new_signal(it))?;
    let r = raw_arg::<E>(s, a, 0).unwrap_or(E::undef());
    old::<E, _>(s, |it| d.signal_abort(it, o, r))?;
    Ok(d.obj_local(s, o))
}

fn static_timeout<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, _: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let Some(a0) = a.get(0) else {
        return Err(s.type_error("Failed to execute 'timeout' on 'AbortSignal': 1 argument required, but only 0 present."));
    };
    let n = s.to_number(a0)?;
    if !n.is_finite() || !(0.0..=18446744073709551615.0).contains(&n) {
        return Err(s.type_error("Failed to execute 'timeout' on 'AbortSignal': Value is outside the range of an unsigned long long."));
    }
    let o = old::<E, _>(s, |it| d.new_signal(it))?;
    lift::<E, _>(s, d.queue_abort_timeout(o, n.trunc()))?;
    Ok(d.obj_local(s, o))
}

fn static_any<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, _: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let a0 = arg_s::<E>(s, a, 0);
    let a0 = s.engine().raw(a0);
    // The iterator of the old engine: one call for each item, with no handle for the iterator or the items.
    let o = old::<E, _>(s, |it| {
        let inputs = any_inputs(d, it, a0)?;
        d.any_of(it, inputs)
    })?;
    Ok(d.obj_local(s, o))
}

/// The signals of the iterable `v` of `AbortSignal.any`.
fn any_inputs<E: Eng>(d: &Dom<E>, it: &mut E::Vm, v: E::Raw) -> R<E, Vec<E::Obj>> {
    let bad = "Failed to execute 'any' on 'AbortSignal': The provided value is not of type 'AbortSignal'.";
    let Some(mut iter) = it.host_iter_open(v)? else { return Err(it.type_error_o("Failed to execute 'any' on 'AbortSignal': The provided value cannot be converted to a sequence.")) };
    let mut inputs = Vec::new();
    loop {
        it.charge_o(1)?;
        match it.host_iter_next(&mut iter) {
            Ok(Some(x)) => match E::kind(x) {
                Val::Object(o) if d.is_signal(o) => inputs.push(o),
                _ => {
                    let e = it.type_error_o(bad);
                    return Err(it.host_iter_fail(&mut iter, e));
                }
            },
            Ok(None) => break,
            Err(e) => return Err(e),
        }
    }
    Ok(inputs)
}

impl<E: Eng> Dom<E> {
    /// `AbortSignal.any(inputs)`.
    fn any_of(&self, it: &mut E::Vm, inputs: Vec<E::Obj>) -> R<E, E::Obj> {
        let result = self.new_signal(it)?;
        let mut t = self.abort.s.borrow_mut();
        if let Some(&first) = inputs.iter().find(|&&i| t.sigs[&i].aborted) {
            let r = *t.sigs[&first].reason.raw();
            let s = t.sigs.get_mut(&result).expect("just made");
            s.aborted = true;
            it.store_value(&mut s.reason, r);
            return Ok(result);
        }
        let mut sources: Vec<u32> = Vec::new();
        for i in inputs {
            let s = &t.sigs[&i];
            let from: Vec<u32> = if s.dependent { s.sources.clone() } else { vec![s.key] };
            for src in from {
                if !sources.contains(&src) {
                    sources.push(src);
                }
            }
        }
        let st = self.st.borrow();
        let key = t.sigs[&result].key;
        for &src in &sources {
            let Some(Owner::Abort(o)) = st.owners.get(src) else { unreachable!("a source is a signal") };
            t.sigs.get_mut(&o).expect("source is a signal").deps.push(key);
        }
        let s = t.sigs.get_mut(&result).expect("just made");
        (s.dependent, s.sources) = (true, sources);
        Ok(result)
    }

    /// A new signal that follows `s` (the `signal` of a `Request`).
    pub(crate) fn follow_signal(&self, it: &mut E::Vm, s: E::Obj) -> R<E, E::Obj> {
        self.any_of(it, vec![s])
    }

    /// A new signal that nothing aborts yet.
    pub(crate) fn fresh_signal(&self, it: &mut E::Vm) -> R<E, E::Obj> {
        self.ensure_abort(it)?;
        self.new_signal(it)
    }

    fn ensure_abort(&self, it: &mut E::Vm) -> R<E, ()> {
        if !self.abort.made.get() {
            make(self, it)?;
        }
        Ok(())
    }
}

fn controller_ctor<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, _: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let default = d.abort.objs.borrow().expect("made")[1];
    let c = old::<E, _>(s, |it| {
        let proto = it.host_proto_from_new_target(default)?;
        let sig = d.new_signal(it)?;
        let c = d.obj(it, proto)?;
        let k = d.st.borrow_mut().owners.key(Owner::Ctl(c));
        it.set_host_owner_obj(c, k);
        let sig = it.trace_value(E::obj_raw(sig));
        d.abort.s.borrow_mut().ctls.insert(c, (k, sig));
        Ok(c)
    })?;
    Ok(d.obj_local(s, c))
}

fn controller_of<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>) -> Res<'s, E::Obj> {
    if let Some(o) = obj_id::<E>(s, this)
        && let Some((_, sig)) = d.abort.s.borrow().ctls.get(&o)
        && let Val::Object(sig) = E::kind(*sig.raw())
    {
        return Ok(sig);
    }
    Err(s.type_error("Illegal invocation"))
}

fn controller_signal<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let o = controller_of(d, s, this)?;
    Ok(d.obj_local(s, o))
}

fn controller_abort<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let o = controller_of(d, s, this)?;
    let r = raw_arg::<E>(s, a, 0).unwrap_or(E::undef());
    old::<E, _>(s, |it| d.signal_abort(it, o, r))?;
    Ok(s.undefined())
}

// ------------------------------------------------------------------------------------ install

/// The two globals are accessors until one is read. A call with an argument is the setter.
fn lazy<'s, const SIG: bool, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, _: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let g = s.global();
    let name = if SIG { "AbortSignal" } else { "AbortController" };
    let attrs = Attrs { writable: true, enumerable: false, configurable: true };
    if let Some(v) = a.get(0) {
        s.define_str(g, name, v, attrs)?;
        return Ok(s.undefined());
    }
    old::<E, _>(s, |it| d.ensure_abort(it))?;
    let ctor = d.abort.objs.borrow().expect("made")[if SIG { 2 } else { 3 }];
    let ctor = d.obj_local(s, ctor);
    s.define_str(g, name, ctor, attrs)?;
    Ok(ctor)
}

#[cold]
#[inline(never)]
fn make<E: Eng>(d: &Dom<E>, it: &mut E::Vm) -> R<E, ()> {
    let (op, et) = (it.object_prototype(), d.p().et);
    let (sp, cp) = (d.obj(it, et)?, d.obj(it, op)?);
    let (sc, cc) = run::<E, _>(it, |s| {
        let sc = d.add_fn_s(s, signal_illegal, "AbortSignal", 0, HostFn::CtorNewTarget)?;
        let cc = d.add_fn_s(s, controller_ctor, "AbortController", 0, HostFn::CtorNewTarget)?;
        let attrs = |w, c| Attrs { writable: w, enumerable: false, configurable: c };
        let tag = s.intrinsic(Intrinsic::SymbolToStringTag);
        for (f, proto, name) in [(sc, sp, "AbortSignal"), (cc, cp, "AbortController")] {
            let proto = d.obj_local(s, proto);
            s.define_str(f, "prototype", proto, attrs(false, false))?;
            s.define_str(proto, "constructor", f, attrs(true, true))?;
            let t = s.string(name)?;
            s.define(proto, tag, t, attrs(false, true))?;
        }
        Ok((obj_id::<E>(s, sc).expect("a function"), obj_id::<E>(s, cc).expect("a function")))
    })?;
    d.def_get_s(it, sp, "aborted", aborted, None)?;
    d.def_get_s(it, sp, "reason", reason, None)?;
    d.def_method_s(it, sp, "throwIfAborted", 0, throw_if_aborted)?;
    crate::browser_handlers::install_onabort(d, it, sp)?;
    d.def_method_s(it, sc, "abort", 0, static_abort)?;
    d.def_method_s(it, sc, "timeout", 1, static_timeout)?;
    d.def_method_s(it, sc, "any", 1, static_any)?;
    d.def_get_s(it, cp, "signal", controller_signal, None)?;
    d.def_method_s(it, cp, "abort", 0, controller_abort)?;
    *d.abort.held.borrow_mut() = [sp, cp, sc, cc].map(|o| it.persist_value(E::obj_raw(o))).into();
    *d.abort.objs.borrow_mut() = Some([sp, cp, sc, cc]);
    d.abort.made.set(true);
    Ok(())
}

pub(crate) fn install<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>) -> Res<'s, ()> {
    let g = s.global();
    for (name, f) in [("AbortController", lazy::<false, _> as HS<E>), ("AbortSignal", lazy::<true, _> as HS<E>)] {
        let f = d.add_fn_s(s, f, name, 0, HostFn::Method)?;
        s.define_accessor_str(g, name, Some(f), Some(f), Attrs { writable: false, enumerable: false, configurable: true })?;
    }
    Ok(())
}
