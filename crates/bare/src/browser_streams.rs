//! A default `ReadableStream` made by script: `new ReadableStream({start, pull, cancel}, {highWaterMark})`, its
//! controller (`enqueue`, `close`, `error`, `desiredSize`) and the default reader (`read`, `cancel`, `releaseLock`).
//!
//! How it works
//! - The algorithms follow the Streams Standard for a default stream with the built-in "one chunk, size 1" strategy:
//!   queue, `started`, `pulling`, `pullAgain`, `closeRequested`, read requests, `ShouldCallPull`. The state of every stream,
//!   controller and reader is Rust data keyed by its JS object (`State`). `Host::trace` reports what each object keeps
//!   (queue chunks, source callbacks, pending read promises, the stream and reader links), `Host::prune` drops the entries of dead
//!   objects. A handle is an `ObjId`, which the heap checks by generation, so a dead owner never aliases a new one.
//! - Promise reactions use the evaluator's own job queue: `Interp::host_promise_react` does `PromiseResolve` and
//!   `PerformPromiseThen` with host functions bound to the controller (`Interp::host_bind`). There is no second microtask
//!   queue, and no script-visible `Promise`, `then` or `bind` is read, so a script that deletes or replaces them changes nothing.
//! - User code (`start`, `pull`, `cancel`, an object's `constructor` getter) runs with no `RefCell` borrow held. Every algorithm
//!   copies what it needs out of `State`, drops the borrow, then calls out, then borrows again and re-checks.
//! - A callback that throws follows the standard: `start` throws out of the constructor, `pull` and `cancel` turn into a
//!   rejected promise (`pull`: the stream errors; `cancel`: `cancel()` rejects).
//!
//! Limits (`StreamLimits`) end the call with `Error::Limit(LimitKind::Host(name))`, never a JS exception: live streams,
//! live readers, queued chunks of one stream, queued bytes of all streams (the length of `ArrayBuffer` and typed-array chunks;
//! other values count as zero bytes but one chunk), pending read requests of one reader, and source callbacks in the life of
//! the browser. Work is charged to the step budget (a chunk costs one step and one step for each 64 bytes). After any limit,
//! `Unsupported` or heap fault, the stream layer is failed for good: each later entry point returns the same error and no
//! callback runs. Objects count until the embedder collects them (`Browser::collect`): the VM never collects by itself.
//!
//! Gates (explicit `Unsupported`, nothing is faked): `type: "bytes"`, `autoAllocateChunkSize`, a `size` strategy function,
//! BYOB readers, `tee`, async iteration, `reader.closed` and subclassing. `pipeTo` and `pipeThrough` are in `shims/streams.js`, with
//! `WritableStream` and `TransformStream`. `new ReadableStreamDefaultReader(stream)` is an
//! illegal constructor (use `getReader()`). A stream whose `highWaterMark` is not 1 works (any number from 0 up to `Infinity`).
//! Deviation: a stream made without `pull` never starts a pull cycle (nothing could observe it).
//! `response.body` is a different object (see `browser_fetch.rs`): the methods on the prototypes dispatch on which one `this` is.
//!
//! Node oracle for the semantics: `tools/js/streams_oracle.mjs` over `tools/fixtures/js/streams-cases.txt`.

use crate::browser::{Browser, Dom, IdMap, Owner, R, illegal_constructor};
use crate::browser_scoped::*;
use bare_js::eval::Interp;
use bare_js::value::{ObjId, Value};
use bare_js::eval::Error as OldError;
use bare_js_api::{Args, Engine, Error, HostFn, Local, Traced, Type};
use std::cell::{Cell, OnceCell, RefCell};
use std::collections::VecDeque;

/// Hard bounds of the stream layer. Each is checked before the thing grows.
#[derive(Clone, Copy, Debug)]
pub struct StreamLimits {
    /// Live streams (each has one controller).
    pub max_streams: usize,
    /// Live readers, released ones too, until they are collected.
    pub max_readers: usize,
    /// Queued chunks of one stream.
    pub max_queue_chunks: usize,
    /// Bytes of queued `ArrayBuffer` and typed-array chunks, all streams together.
    pub max_queued_bytes: usize,
    /// Pending read requests of one reader.
    pub max_pending_reads: usize,
    /// Calls of `start`, `pull` and `cancel` in the life of the browser.
    pub max_callbacks: u64,
}

impl Default for StreamLimits {
    fn default() -> Self {
        StreamLimits { max_streams: 4096, max_readers: 4096, max_queue_chunks: 1 << 14, max_queued_bytes: 64 << 20, max_pending_reads: 1024, max_callbacks: 1 << 22 }
    }
}

/// Counters for tests and the load tool.
#[derive(Clone, Copy, Debug, Default)]
pub struct StreamStats {
    pub streams: usize,
    pub readers: usize,
    pub queued_chunks: usize,
    pub queued_bytes: usize,
    pub pending_reads: usize,
    pub callbacks: u64,
}

// ---------------------------------------------------------------------------------------------- state

enum St {
    Readable,
    Closed,
    Errored(Traced<Value>),
}

struct Stream {
    /// The host key of the stream object (`Owner::Stream`).
    key: u32,
    st: St,
    ctl: Traced<Value>,
    reader: Option<Traced<Value>>,
    /// A read or a cancel happened (the `[[disturbed]]` flag of the standard). `new Response(stream).bodyUsed` reads it.
    disturbed: bool,
}

struct Ctl {
    key: u32,
    stream: Traced<Value>,
    source: Traced<Value>,
    start: Traced<Value>,
    pull: Traced<Value>,
    cancel: Traced<Value>,
    /// Chunks with the byte cost that was charged for each.
    queue: VecDeque<(Traced<Value>, usize)>,
    qbytes: usize,
    hwm: f64,
    started: bool,
    pulling: bool,
    pull_again: bool,
    close_requested: bool,
    /// The bound reactions of `pull`, made on the first pull.
    pull_fns: Option<(Traced<Value>, Traced<Value>)>,
}

struct Rdr {
    key: u32,
    /// `None` after `releaseLock`.
    stream: Option<Traced<Value>>,
    /// Pending read promises, oldest first.
    reads: VecDeque<Traced<Value>>,
}

#[derive(Default)]
struct State {
    streams: IdMap<ObjId, Stream>,
    ctls: IdMap<ObjId, Ctl>,
    readers: IdMap<ObjId, Rdr>,
    queued_bytes: usize,
    callbacks: u64,
    fatal: Option<OldError>,
}

/// The templates of the host reactions. `this` of each bound copy is the controller, or the promise of a `cancel()` call.
#[derive(Clone, Copy)]
struct Fns {
    start_ok: ObjId,
    start_err: ObjId,
    pull_ok: ObjId,
    pull_err: ObjId,
    cancel_ok: ObjId,
    cancel_err: ObjId,
    stream_proto: ObjId,
    reader_proto: ObjId,
    ctl_proto: ObjId,
}

/// The stream state of one `Dom`.
#[derive(Default)]
pub(crate) struct Streams {
    names: Names,
    st: RefCell<State>,
    lim: Cell<StreamLimits>,
    fns: OnceCell<Fns>,
}

/// The object of a traced reference to an object.
fn oid(t: &Traced<Value>) -> ObjId {
    match *t.raw() {
        Value::Object(o) => o,
        _ => unreachable!("a stream edge is an object"),
    }
}

/// Is the traced value `undefined`? (Read in place: no local is pushed.)
fn undef(t: &Traced<Value>) -> bool {
    matches!(*t.raw(), Value::Undefined)
}

/// The references of the owner `Owner::Stream(owner)`: a stream, a controller or a reader.
pub(crate) fn trace(d: &Dom, owner: ObjId, t: &mut bare_js_api::Tracer<'_, Value>) {
    let st = d.streams.st.borrow();
    if let Some(s) = st.streams.get(&owner) {
        t.visit(&s.ctl);
        if let Some(r) = &s.reader {
            t.visit(r);
        }
        if let St::Errored(e) = &s.st {
            t.visit(e);
        }
    } else if let Some(c) = st.ctls.get(&owner) {
        for v in [&c.stream, &c.source, &c.start, &c.pull, &c.cancel] {
            t.visit(v);
        }
        for q in &c.queue {
            t.visit(&q.0);
        }
        if let Some((a, b)) = &c.pull_fns {
            t.visit(a);
            t.visit(b);
        }
    } else if let Some(r) = st.readers.get(&owner) {
        if let Some(s) = &r.stream {
            t.visit(s);
        }
        for p in &r.reads {
            t.visit(p);
        }
    }
}

/// Call after the dead keys are released. One pass over the entries: the entries of dead owners go, and the queued bytes of a dead
/// controller leave the total.
pub(crate) fn prune(d: &Dom, st: &crate::browser::State) {
    let mut s = d.streams.st.borrow_mut();
    let s = &mut *s;
    let live = |k: u32| st.owners.get(k).is_some();
    s.streams.retain(|_, e| live(e.key));
    s.readers.retain(|_, e| live(e.key));
    let queued = &mut s.queued_bytes;
    s.ctls.retain(|_, e| {
        let l = live(e.key);
        if !l {
            *queued -= e.qbytes;
        }
        l
    });
}

fn lim(d: &Dom) -> StreamLimits {
    d.streams.lim.get()
}

fn fns(d: &Dom) -> Fns {
    *d.streams.fns.get().expect("installed")
}

/// Run an entry point: refuse after a fatal error, and make a non-JS error fatal for good. The error is kept in the old form, so a
/// later call gives the same limit.
fn enter<'s, 'p, T>(d: &Dom, s: &mut Sc<'s, 'p>, f: impl FnOnce(&mut Sc<'s, 'p>) -> Res<'s, T>) -> Res<'s, T> {
    if let Some(e) = d.streams.st.borrow().fatal.clone() {
        return Err(s.engine().api_err(e));
    }
    match f(s) {
        Err(bare_js_api::Error::Abort(a)) => {
            let it = s.engine();
            let e = it.api_unerr(bare_js_api::Error::Abort(a));
            d.streams.st.borrow_mut().fatal = Some(e.clone());
            Err(it.api_err(e))
        }
        r => r,
    }
}

fn prop<'s>(s: &mut Sc<'s, '_>, o: Local<'s>, name: &str) -> Res<'s, Local<'s>> {
    s.get_str(o, name)
}

/// The names that the constructor reads on every call. `K_*` index it.
const NAMES: [&str; 7] = ["autoAllocateChunkSize", "cancel", "pull", "start", "type", "highWaterMark", "size"];
const K_AUTO: usize = 0;
const K_CANCEL: usize = 1;
const K_PULL: usize = 2;
const K_START: usize = 3;
const K_TYPE: usize = 4;
const K_HWM: usize = 5;
const K_SIZE: usize = 6;

/// `o[NAMES[i]]`, with no lookup of the text.
fn named<'s>(d: &Dom, s: &mut Sc<'s, '_>, o: Local<'s>, i: usize) -> Res<'s, Local<'s>> {
    let k = d.streams.names.local(s, &NAMES, i)?;
    s.get(o, k)
}

fn thrown<'s>(e: Error<'s>) -> Res<'s, Local<'s>> {
    match e {
        Error::Throw(v) => Ok(v),
        e => Err(e),
    }
}

fn type_value<'s>(s: &mut Sc<'s, '_>, msg: &str) -> Res<'s, Local<'s>> {
    let e = s.type_error(msg);
    thrown(e)
}

fn iter_result_raw(it: &mut Interp<'_>, value: Value, done: bool) -> R<Value> {
    it.charge(1)?;
    it.host_iter_result(value, done)
}

fn resolve_with<'s>(s: &mut Sc<'s, '_>, p: Local<'s>, value: Local<'s>, done: bool) -> Res<'s, ()> {
    let (p, value) = (s.engine().raw(p), s.engine().raw(value));
    old(s, |it| {
        let r = iter_result_raw(it, value, done)?;
        it.host_resolve_promise(p, r)
    })
}

/// Call a user callback of the source. `Ok(Err(v))` is a JS exception. Counted and charged.
fn invoke<'s>(d: &Dom, s: &mut Sc<'s, '_>, f: Local<'s>, this: Local<'s>, arg: Local<'s>) -> Res<'s, Result<Local<'s>, Local<'s>>> {
    {
        let mut st = d.streams.st.borrow_mut();
        if st.callbacks >= lim(d).max_callbacks {
            drop(st);
            return Err(d.lim_s(s, "stream-callbacks"));
        }
        st.callbacks += 1;
    }
    s.charge(2)?;
    let it = s.engine();
    let (f, this, arg) = (it.raw(f), it.raw(this), it.raw(arg));
    match it.call(f, this, &[arg]) {
        Ok(v) => Ok(Ok(it.raw_local(&v))),
        Err(OldError::Throw(v)) => Ok(Err(it.raw_local(&v))),
        Err(e) => Err(it.api_err(e)),
    }
}

/// React to the outcome of a promise-returning callback. A throw becomes a rejected promise, as the standard says.
fn react<'s>(s: &mut Sc<'s, '_>, r: Result<Local<'s>, Local<'s>>, ok: Local<'s>, err: Local<'s>) -> Res<'s, ()> {
    let x = match r {
        Ok(v) => v,
        Err(e) => {
            let p = s.new_promise()?;
            s.reject_promise(p, e)?;
            p
        }
    };
    s.promise_react(x, ok, err)
}

// ---------------------------------------------------------------------------------------------- algorithms

fn should_pull(st: &State, c: &Ctl) -> bool {
    let Some(s) = st.streams.get(&oid(&c.stream)) else { return false };
    if !matches!(s.st, St::Readable) || c.close_requested || !c.started {
        return false;
    }
    if let Some(r) = s.reader.as_ref().and_then(|r| st.readers.get(&oid(r)))
        && !r.reads.is_empty()
    {
        return true;
    }
    c.hwm - c.queue.len() as f64 > 0.0
}

/// `ReadableStreamDefaultControllerCallPullIfNeeded`.
fn pull_if_needed<'s>(d: &Dom, s: &mut Sc<'s, '_>, ctl: ObjId) -> Res<'s, ()> {
    let (pull, source, cached) = {
        let mut st = d.streams.st.borrow_mut();
        let Some(c) = st.ctls.get(&ctl) else { return Ok(()) };
        // A stream with no `pull` can never start a cycle that anyone could see.
        if undef(&c.pull) || !should_pull(&st, c) {
            return Ok(());
        }
        let c = st.ctls.get_mut(&ctl).expect("checked");
        if c.pulling {
            c.pull_again = true;
            return Ok(());
        }
        c.pulling = true;
        (s.traced(&c.pull), s.traced(&c.source), c.pull_fns.as_ref().map(|(a, b)| (s.traced(a), s.traced(b))))
    };
    let ctl_l = d.obj_local(s, ctl);
    let (ok, err) = match cached {
        Some(p) => p,
        None => {
            let f = fns(d);
            let p = (bind_obj(s, f.pull_ok, ctl)?, bind_obj(s, f.pull_err, ctl)?);
            let edges = (s.trace_ref(p.0), s.trace_ref(p.1));
            if let Some(c) = d.streams.st.borrow_mut().ctls.get_mut(&ctl) {
                c.pull_fns = Some(edges);
            }
            p
        }
    };
    let r = invoke(d, s, pull, source, ctl_l)?;
    match react(s, r, ok, err) {
        Err(Error::Throw(v)) => controller_error(d, s, ctl, v),
        r => r,
    }
}

fn clear_algorithms(s: &mut Sc<'_, '_>, c: &mut Ctl) {
    let u = s.undefined();
    for t in [&mut c.source, &mut c.start, &mut c.pull, &mut c.cancel] {
        s.store(t, u);
    }
    c.pull_fns = None;
}

fn reset_queue(st: &mut State, ctl: ObjId) {
    if let Some(c) = st.ctls.get_mut(&ctl) {
        st.queued_bytes -= c.qbytes;
        c.qbytes = 0;
        c.queue.clear();
    }
}

/// Make the stream closed and take its pending read requests.
fn close_stream(st: &mut State, stream: ObjId) -> Vec<Traced<Value>> {
    let Some(strm) = st.streams.get_mut(&stream) else { return Vec::new() };
    strm.st = St::Closed;
    match strm.reader.as_ref().and_then(|r| st.readers.get_mut(&oid(r))) {
        Some(r) => r.reads.drain(..).collect(),
        None => Vec::new(),
    }
}

/// `ClearAlgorithms`, then `ReadableStreamClose`, then settle the pending reads with `done`.
fn finish_close<'s>(d: &Dom, s: &mut Sc<'s, '_>, ctl: ObjId) -> Res<'s, ()> {
    let reads = {
        let mut st = d.streams.st.borrow_mut();
        let Some(c) = st.ctls.get_mut(&ctl) else { return Ok(()) };
        clear_algorithms(s, c);
        let strm = oid(&c.stream);
        close_stream(&mut st, strm)
    };
    for p in reads {
        let (p, u) = (s.traced(&p), s.undefined());
        resolve_with(s, p, u, true)?;
    }
    Ok(())
}

/// `ReadableStreamDefaultControllerError`.
fn controller_error<'s>(d: &Dom, s: &mut Sc<'s, '_>, ctl: ObjId, e: Local<'s>) -> Res<'s, ()> {
    let reads = {
        let mut st = d.streams.st.borrow_mut();
        let Some(c) = st.ctls.get(&ctl) else { return Ok(()) };
        let sid = oid(&c.stream);
        if !st.streams.get(&sid).is_some_and(|x| matches!(x.st, St::Readable)) {
            return Ok(());
        }
        reset_queue(&mut st, ctl);
        if let Some(c) = st.ctls.get_mut(&ctl) {
            clear_algorithms(s, c);
        }
        let strm = st.streams.get_mut(&sid).expect("checked");
        strm.st = St::Errored(s.trace_ref(e));
        let rid = strm.reader.as_ref().map(oid);
        match rid.and_then(|r| st.readers.get_mut(&r)) {
            Some(r) => r.reads.drain(..).collect::<Vec<_>>(),
            None => Vec::new(),
        }
    };
    for p in reads {
        let p = s.traced(&p);
        s.reject_promise(p, e)?;
    }
    Ok(())
}

/// `ReadableStreamCancel`. Returns the promise `cancel()` gives.
fn cancel_stream<'s>(d: &Dom, s: &mut Sc<'s, '_>, stream: ObjId, reason: Local<'s>) -> Res<'s, Local<'s>> {
    enum C<'s> {
        Closed,
        Errored(Local<'s>),
        Go { cancel: Local<'s>, source: Local<'s>, reads: Vec<Traced<Value>> },
    }
    let c = {
        let mut st = d.streams.st.borrow_mut();
        if let Some(x) = st.streams.get_mut(&stream) {
            x.disturbed = true;
        }
        match st.streams.get(&stream).map(|x| (&x.st, &x.ctl)) {
            None => C::Closed,
            Some((St::Closed, _)) => C::Closed,
            Some((St::Errored(e), _)) => C::Errored(s.traced(e)),
            Some((St::Readable, ctl)) => {
                let ctl = oid(ctl);
                let reads = close_stream(&mut st, stream);
                reset_queue(&mut st, ctl);
                let c = st.ctls.get_mut(&ctl).expect("a stream has a controller");
                let (cancel, source) = (s.traced(&c.cancel), s.traced(&c.source));
                clear_algorithms(s, c);
                C::Go { cancel, source, reads }
            }
        }
    };
    let p = s.new_promise()?;
    match c {
        C::Closed => {
            let u = s.undefined();
            s.resolve_promise(p, u)?
        }
        C::Errored(e) => s.reject_promise(p, e)?,
        C::Go { cancel, source, reads } => {
            for r in reads {
                let (r, u) = (s.traced(&r), s.undefined());
                resolve_with(s, r, u, true)?;
            }
            let r = if s.type_of(cancel) == Type::Undefined { Ok(s.undefined()) } else { invoke(d, s, cancel, source, reason)? };
            let f = fns(d);
            let (co, ce) = (d.obj_local(s, f.cancel_ok), d.obj_local(s, f.cancel_err));
            let (ok, err) = (s.bind(co, p)?, s.bind(ce, p)?);
            match react(s, r, ok, err) {
                Err(Error::Throw(v)) => s.reject_promise(p, v)?,
                r => r?,
            }
        }
    }
    Ok(p)
}

// ---------------------------------------------------------------------------------------------- reactions

fn start_ok<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    enter(d, s, |s| {
        let Some(ctl) = obj_id(s, this) else { return Ok(s.undefined()) };
        if let Some(c) = d.streams.st.borrow_mut().ctls.get_mut(&ctl) {
            c.started = true;
        }
        pull_if_needed(d, s, ctl)?;
        Ok(s.undefined())
    })
}

fn error_reaction<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    enter(d, s, |s| {
        if let Some(ctl) = obj_id(s, this) {
            let e = arg_s(s, a, 0);
            controller_error(d, s, ctl, e)?;
        }
        Ok(s.undefined())
    })
}

fn pull_ok<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    enter(d, s, |s| {
        let Some(ctl) = obj_id(s, this) else { return Ok(s.undefined()) };
        let again = match d.streams.st.borrow_mut().ctls.get_mut(&ctl) {
            Some(c) => {
                c.pulling = false;
                std::mem::take(&mut c.pull_again)
            }
            None => false,
        };
        if again {
            pull_if_needed(d, s, ctl)?;
        }
        Ok(s.undefined())
    })
}

fn cancel_ok<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    enter(d, s, |s| {
        let u = s.undefined();
        s.resolve_promise(this, u)?;
        Ok(u)
    })
}

fn cancel_err<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    enter(d, s, |s| {
        let e = arg_s(s, a, 0);
        s.reject_promise(this, e)?;
        Ok(s.undefined())
    })
}

// ---------------------------------------------------------------------------------------------- constructor

fn callback_option<'s>(d: &Dom, s: &mut Sc<'s, '_>, src: Local<'s>, i: usize) -> Res<'s, Local<'s>> {
    let v = named(d, s, src, i)?;
    if s.type_of(v) != Type::Undefined && !s.is_callable(v) {
        return Err(s.type_error(&format!("Failed to construct 'ReadableStream': The {} member is not a function", NAMES[i])));
    }
    Ok(v)
}

pub(crate) fn ctor<'s>(d: &Dom, s: &mut Sc<'s, '_>, _: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    enter(d, s, |s| construct(d, s, a))
}

/// A new object with the prototype `proto`, registered as the owner of its entry. Gives the object and its host key.
fn own<'s>(d: &Dom, s: &mut Sc<'s, '_>, proto: ObjId) -> Res<'s, (ObjId, u32)> {
    let o = d.obj_s(s, proto)?;
    let k = d.st.borrow_mut().owners.key(Owner::Stream(o));
    s.engine().set_host_owner_obj(o, k);
    Ok((o, k))
}

/// `bind` of a host function and an object, with no handle for either.
fn bind_obj<'s>(s: &mut Sc<'s, '_>, target: ObjId, this: ObjId) -> Res<'s, Local<'s>> {
    old_v(s, |it| it.host_bind(target, Value::Object(this)).map(Value::Object))
}

fn construct<'s>(d: &Dom, s: &mut Sc<'s, '_>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let (src, strategy) = (arg_s(s, a, 0), arg_s(s, a, 1));
    let u = s.undefined();
    let (mut start, mut pull, mut cancel) = (u, u, u);
    if s.type_of(src) != Type::Undefined {
        if !is_object(s, src) {
            return Err(s.type_error("Failed to construct 'ReadableStream': The underlying source is not an object"));
        }
        // Dictionary members are read in alphabetical order, as WebIDL does.
        let auto = named(d, s, src, K_AUTO)?;
        if s.type_of(auto) != Type::Undefined {
            return Err(unsupp("ReadableStream autoAllocateChunkSize (byte streams)"));
        }
        cancel = callback_option(d, s, src, K_CANCEL)?;
        pull = callback_option(d, s, src, K_PULL)?;
        start = callback_option(d, s, src, K_START)?;
        let ty = named(d, s, src, K_TYPE)?;
        if s.type_of(ty) != Type::Undefined {
            let t = s.to_string(ty)?;
            let text = s.as_str(t).map(|x| x.to_string_lossy()).unwrap_or_default();
            if text == "bytes" {
                return Err(unsupp("ReadableStream type: \"bytes\" (byte streams and BYOB)"));
            }
            return Err(s.type_error("Failed to construct 'ReadableStream': The provided value is not a valid enum value of type ReadableStreamType"));
        }
    }
    let mut hwm = 1.0;
    if !matches!(s.type_of(strategy), Type::Undefined | Type::Null) {
        if !is_object(s, strategy) {
            return Err(s.type_error("Failed to construct 'ReadableStream': The queuing strategy is not an object"));
        }
        let h = named(d, s, strategy, K_HWM)?;
        if s.type_of(h) != Type::Undefined {
            hwm = s.to_number(h)?;
            if hwm.is_nan() || hwm < 0.0 {
                return Err(s.range_error("Failed to construct 'ReadableStream': Invalid highWaterMark"));
            }
        }
        let size = named(d, s, strategy, K_SIZE)?;
        if s.type_of(size) != Type::Undefined {
            if !s.is_callable(size) {
                return Err(s.type_error("Failed to construct 'ReadableStream': The size member is not a function"));
            }
            return Err(unsupp("ReadableStream strategy.size (custom queuing strategy)"));
        }
    }
    let l = lim(d);
    if d.streams.st.borrow().streams.len() >= l.max_streams {
        return Err(d.lim_s(s, "stream-objects"));
    }
    s.charge(4)?;
    let f = fns(d);
    let ((strm, sk), (c, ck)) = (own(d, s, f.stream_proto)?, own(d, s, f.ctl_proto)?);
    let (cl, sl) = (d.obj_local(s, c), d.obj_local(s, strm));
    let (ok, err) = (bind_obj(s, f.start_ok, c)?, bind_obj(s, f.start_err, c)?);
    {
        let (ctl_edge, stream_edge) = (s.trace_ref(cl), s.trace_ref(sl));
        let ctl = Ctl { key: ck, stream: stream_edge, source: s.trace_ref(src), start: s.trace_ref(start), pull: s.trace_ref(pull), cancel: s.trace_ref(cancel), queue: VecDeque::new(), qbytes: 0, hwm, started: false, pulling: false, pull_again: false, close_requested: false, pull_fns: None };
        let mut st = d.streams.st.borrow_mut();
        st.streams.insert(strm, Stream { key: sk, st: St::Readable, ctl: ctl_edge, reader: None, disturbed: false });
        st.ctls.insert(c, ctl);
    }
    let started = if s.type_of(start) == Type::Undefined { Ok(u) } else { invoke(d, s, start, src, cl)? };
    match started {
        // A controller can escape through start() even when that callback throws. Its stream state stays valid
        // until collection proves it unreachable.
        Err(e) => return Err(Error::Throw(e)),
        Ok(v) => {
            // start returns any, not Promise<void>. Resolve a fresh promise with it, so assimilation is a job.
            let p = s.new_promise()?;
            s.resolve_promise(p, v)?;
            s.promise_then(p, ok, err)?;
        }
    }
    Ok(sl)
}

// ---------------------------------------------------------------------------------------------- stream methods

fn is_stream(d: &Dom, o: Option<ObjId>) -> Option<ObjId> {
    o.filter(|o| d.streams.st.borrow().streams.contains_key(o))
}

fn is_reader(d: &Dom, o: Option<ObjId>) -> Option<ObjId> {
    o.filter(|o| d.streams.st.borrow().readers.contains_key(o))
}

/// `(locked, disturbed)` of a stream made by script, or `None` if `o` is not one.
pub(crate) fn lock_state(d: &Dom, o: ObjId) -> Option<(bool, bool)> {
    d.streams.st.borrow().streams.get(&o).map(|s| (s.reader.is_some(), s.disturbed))
}

pub(crate) fn locked<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    enter(d, s, |s| {
        let Some(strm) = is_stream(d, obj_id(s, this)) else { return crate::browser_fetch::stream_locked(d, s, this, a) };
        let locked = d.streams.st.borrow().streams.get(&strm).is_some_and(|x| x.reader.is_some());
        Ok(s.boolean(locked))
    })
}

pub(crate) fn get_reader<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    enter(d, s, |s| {
        let options = arg_s(s, a, 0);
        let mut byob = false;
        if !matches!(s.type_of(options), Type::Undefined | Type::Null) {
            if !is_object(s, options) {
                return Err(s.type_error("Reader options must be a dictionary"));
            }
            let mode = prop(s, options, "mode")?;
            if s.type_of(mode) != Type::Undefined {
                let mode = s.to_string(mode)?;
                if s.as_str(mode).map(|x| x.to_string_lossy()).unwrap_or_default() != "byob" {
                    return Err(s.type_error("Invalid readable stream reader mode"));
                }
                byob = true;
            }
        }
        let Some(strm) = is_stream(d, obj_id(s, this)) else {
            if byob {
                crate::browser_fetch::stream_locked(d, s, this, Args::from_slots(0, 0))?;
                return Err(unsupp("ReadableStream BYOB reader"));
            }
            return crate::browser_fetch::stream_get_reader(d, s, this, Args::from_slots(0, 0));
        };
        if byob {
            return Err(unsupp("ReadableStream BYOB reader"));
        }
        {
            let locked = d.streams.st.borrow().streams.get(&strm).is_some_and(|x| x.reader.is_some());
            if locked {
                return Err(s.type_error("ReadableStream is locked"));
            }
            if d.streams.st.borrow().readers.len() >= lim(d).max_readers {
                return Err(d.lim_s(s, "stream-readers"));
            }
        }
        s.charge(2)?;
        let (r, key) = own(d, s, fns(d).reader_proto)?;
        let rl = d.obj_local(s, r);
        let sl = d.obj_local(s, strm);
        let (stream_edge, reader_edge) = (s.trace_ref(sl), s.trace_ref(rl));
        let mut st = d.streams.st.borrow_mut();
        st.readers.insert(r, Rdr { key, stream: Some(stream_edge), reads: VecDeque::new() });
        if let Some(x) = st.streams.get_mut(&strm) {
            x.reader = Some(reader_edge);
        }
        Ok(rl)
    })
}

pub(crate) fn cancel<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    enter(d, s, |s| {
        let Some(strm) = is_stream(d, obj_id(s, this)) else { return crate::browser_fetch::stream_cancel(d, s, this, a) };
        if d.streams.st.borrow().streams.get(&strm).is_some_and(|x| x.reader.is_some()) {
            let e = type_value(s, "Cannot cancel a stream that already has a reader")?;
            let p = s.new_promise()?;
            s.reject_promise(p, e)?;
            return Ok(p);
        }
        let r = arg_s(s, a, 0);
        cancel_stream(d, s, strm, r)
    })
}

// ---------------------------------------------------------------------------------------------- reader methods

pub(crate) fn read<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    enter(d, s, |s| {
        let Some(rdr) = is_reader(d, obj_id(s, this)) else { return crate::browser_fetch::reader_read(d, s, this, a) };
        enum Out<'s> {
            Released,
            Done,
            Err(Local<'s>),
            Chunk(Local<'s>, ObjId, bool),
            Pending(ObjId),
            Full,
        }
        s.charge(1)?;
        let p = s.new_promise()?;
        let max = lim(d).max_pending_reads;
        let out = {
            let mut st = d.streams.st.borrow_mut();
            let st = &mut *st;
            match st.readers.get(&rdr).and_then(|r| r.stream.as_ref().map(oid)) {
                None => Out::Released,
                Some(sid) => match st.streams.get_mut(&sid).map(|x| {
                    x.disturbed = true;
                    &*x
                }) {
                    None => Out::Released,
                    Some(Stream { st: St::Closed, .. }) => Out::Done,
                    Some(Stream { st: St::Errored(e), .. }) => Out::Err(s.traced(e)),
                    Some(Stream { ctl, .. }) => {
                        let ctl = oid(ctl);
                        let c = st.ctls.get_mut(&ctl).expect("a stream has a controller");
                        if let Some((chunk, cost)) = c.queue.pop_front() {
                            c.qbytes -= cost;
                            st.queued_bytes -= cost;
                            Out::Chunk(s.traced(&chunk), ctl, c.close_requested && c.queue.is_empty())
                        } else {
                            let r = st.readers.get_mut(&rdr).expect("checked");
                            if r.reads.len() >= max {
                                Out::Full
                            } else {
                                r.reads.push_back(s.trace_ref(p));
                                Out::Pending(ctl)
                            }
                        }
                    }
                },
            }
        };
        match out {
            Out::Released => {
                let e = type_value(s, "This readable stream reader has been released and cannot be used to read from its previous owner stream")?;
                s.reject_promise(p, e)?;
            }
            Out::Done => {
                let u = s.undefined();
                resolve_with(s, p, u, true)?
            }
            Out::Err(e) => s.reject_promise(p, e)?,
            Out::Full => return Err(d.lim_s(s, "stream-reads")),
            Out::Chunk(chunk, ctl, close) => {
                if close {
                    finish_close(d, s, ctl)?;
                } else {
                    pull_if_needed(d, s, ctl)?;
                }
                resolve_with(s, p, chunk, false)?;
            }
            Out::Pending(ctl) => pull_if_needed(d, s, ctl)?,
        }
        Ok(p)
    })
}

pub(crate) fn reader_cancel<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    enter(d, s, |s| {
        let Some(rdr) = is_reader(d, obj_id(s, this)) else { return crate::browser_fetch::reader_cancel(d, s, this, a) };
        let stream = d.streams.st.borrow().readers.get(&rdr).and_then(|r| r.stream.as_ref().map(oid));
        match stream {
            Some(strm) => {
                let r = arg_s(s, a, 0);
                cancel_stream(d, s, strm, r)
            }
            None => {
                let e = type_value(s, "This readable stream reader has been released and cannot be used to cancel its previous owner stream")?;
                let p = s.new_promise()?;
                s.reject_promise(p, e)?;
                Ok(p)
            }
        }
    })
}

pub(crate) fn release<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    enter(d, s, |s| {
        let Some(rdr) = is_reader(d, obj_id(s, this)) else { return crate::browser_fetch::reader_release(d, s, this, a) };
        let reads = {
            let mut st = d.streams.st.borrow_mut();
            let Some(r) = st.readers.get_mut(&rdr) else { return Ok(s.undefined()) };
            let Some(sid) = r.stream.take().map(|x| oid(&x)) else { return Ok(s.undefined()) };
            let reads: Vec<Traced<Value>> = r.reads.drain(..).collect();
            if let Some(x) = st.streams.get_mut(&sid) {
                x.reader = None;
            }
            reads
        };
        for p in reads {
            let p = s.traced(&p);
            let e = type_value(s, "Reader was released")?;
            s.reject_promise(p, e)?;
        }
        Ok(s.undefined())
    })
}

// ---------------------------------------------------------------------------------------------- controller methods

fn brand_ctl<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>) -> Res<'s, ObjId> {
    match obj_id(s, this).filter(|o| d.streams.st.borrow().ctls.contains_key(o)) {
        Some(o) => Ok(o),
        None => Err(s.type_error("Illegal invocation")),
    }
}

/// `CanCloseOrEnqueue`.
fn can_close_or_enqueue(st: &State, c: &Ctl) -> bool {
    !c.close_requested && st.streams.get(&oid(&c.stream)).is_some_and(|x| matches!(x.st, St::Readable))
}

fn ctl_enqueue<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    enter(d, s, |s| {
        let ctl = brand_ctl(d, s, this)?;
        let chunk = arg_s(s, a, 0);
        let bytes = s.bytes(chunk).map_or(0, |b| b.len());
        s.charge(1 + (bytes / 64) as u64)?;
        let l = lim(d);
        let direct = {
            let mut st = d.streams.st.borrow_mut();
            let st = &mut *st;
            let c = st.ctls.get(&ctl).expect("checked");
            if !can_close_or_enqueue(st, c) {
                None
            } else {
                let waiting = st.streams.get(&oid(&c.stream)).and_then(|x| x.reader.as_ref().map(oid)).and_then(|r| st.readers.get_mut(&r)).and_then(|r| r.reads.pop_front());
                match waiting {
                    Some(p) => Some(Some(s.traced(&p))),
                    None => {
                        if c.queue.len() >= l.max_queue_chunks {
                            return Err(d.lim_s(s, "stream-queue"));
                        }
                        if st.queued_bytes.saturating_add(bytes) > l.max_queued_bytes {
                            return Err(d.lim_s(s, "stream-bytes"));
                        }
                        let c = st.ctls.get_mut(&ctl).expect("checked");
                        c.queue.push_back((s.trace_ref(chunk), bytes));
                        c.qbytes += bytes;
                        st.queued_bytes += bytes;
                        Some(None)
                    }
                }
            }
        };
        match direct {
            None => return Err(s.type_error("Invalid state: Controller is already closed")),
            Some(Some(p)) => resolve_with(s, p, chunk, false)?,
            Some(None) => {}
        }
        pull_if_needed(d, s, ctl)?;
        Ok(s.undefined())
    })
}

fn ctl_close<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    enter(d, s, |s| {
        let ctl = brand_ctl(d, s, this)?;
        s.charge(1)?;
        let empty = {
            let mut st = d.streams.st.borrow_mut();
            let c = st.ctls.get(&ctl).expect("checked");
            if !can_close_or_enqueue(&st, c) {
                None
            } else {
                let c = st.ctls.get_mut(&ctl).expect("checked");
                c.close_requested = true;
                Some(c.queue.is_empty())
            }
        };
        match empty {
            None => return Err(s.type_error("Invalid state: Controller is already closed")),
            Some(true) => finish_close(d, s, ctl)?,
            Some(false) => {}
        }
        Ok(s.undefined())
    })
}

fn ctl_error<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    enter(d, s, |s| {
        let ctl = brand_ctl(d, s, this)?;
        s.charge(1)?;
        let e = arg_s(s, a, 0);
        controller_error(d, s, ctl, e)?;
        Ok(s.undefined())
    })
}

fn ctl_desired_size<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    enter(d, s, |s| {
        let ctl = brand_ctl(d, s, this)?;
        let st = d.streams.st.borrow();
        let c = st.ctls.get(&ctl).expect("checked");
        Ok(match st.streams.get(&oid(&c.stream)).map(|x| &x.st) {
            Some(St::Errored(_)) | None => s.null(),
            Some(St::Closed) => s.number(0.0),
            Some(St::Readable) => s.number(c.hwm - c.queue.len() as f64),
        })
    })
}

fn gate_closed<'s>(_: &Dom, _: &mut Sc<'s, '_>, _: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    Err(unsupp("ReadableStreamDefaultReader.closed"))
}

pub(crate) fn reader_closed<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    enter(d, s, |s| {
        if is_reader(d, obj_id(s, this)).is_some() {
            return gate_closed(d, s, this, a);
        }
        crate::browser_fetch::gate_reader_closed(d, s, this, a)
    })
}

pub(crate) fn gate<'s>(d: &Dom, s: &mut Sc<'s, '_>, _: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    enter(d, s, |_| Err(unsupp("ReadableStream tee and async iteration")))
}

/// `pipeTo` and `pipeThrough` are JavaScript (`shims/streams.js`), as `WritableStream` and `TransformStream` are. The first call makes
/// the shim, which replaces both methods on the prototype. This method then calls the method that the prototype now has.
fn pipe<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, a: Args<'s>, name: &str) -> Res<'s, Local<'s>> {
    enter(d, s, |s| {
        old(s, |it| crate::browser_shims::define(d, it, "WritableStream"))?;
        let proto = d.obj_local(s, fns(d).stream_proto);
        let f = prop(s, proto, name)?;
        if !s.is_callable(f) {
            return Err(s.type_error(&format!("ReadableStream.prototype.{name} is not a function")));
        }
        let args: Vec<Local<'s>> = a.iter().collect();
        s.call(f, this, &args)
    })
}

pub(crate) fn pipe_to<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    pipe(d, s, this, a, "pipeTo")
}

pub(crate) fn pipe_through<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    pipe(d, s, this, a, "pipeThrough")
}

// ---------------------------------------------------------------------------------------------- install

/// Make the host reactions and the controller prototype. `fixed` gets every object that must live as long as the realm.
/// `stream_proto` and `reader_proto` are the prototypes of `ReadableStream` and the default reader.
pub(crate) fn install<'s>(d: &Dom, s: &mut Sc<'s, '_>, stream_proto: ObjId, reader_proto: ObjId, fixed: &mut Vec<ObjId>) -> Res<'s, ()> {
    let f = |s: &mut Sc<'s, '_>, h: HS, name: &str| -> Res<'s, ObjId> {
        let l = d.add_fn_s(s, h, name, 1, HostFn::Method)?;
        Ok(obj_id(s, l).expect("a function"))
    };
    let start_ok_f = f(s, start_ok, "startOk")?;
    let start_err = f(s, error_reaction, "startErr")?;
    let pull_ok_f = f(s, pull_ok, "pullOk")?;
    let pull_err = f(s, error_reaction, "pullErr")?;
    let cancel_ok_f = f(s, cancel_ok, "cancelOk")?;
    let cancel_err_f = f(s, cancel_err, "cancelErr")?;
    let ctl_proto = d.plain_s(s)?;
    for (name, h, len) in [("enqueue", ctl_enqueue as HS, 1), ("close", ctl_close, 0), ("error", ctl_error, 1)] {
        d.def_method_at(s, ctl_proto, name, len, h)?;
    }
    d.def_get_at(s, ctl_proto, "desiredSize", ctl_desired_size, None)?;
    let ctor = d.interface_s(s, ctl_proto, "ReadableStreamDefaultController", illegal_constructor, 0, HostFn::Method)?;
    fixed.extend([ctl_proto, ctor, start_ok_f, start_err, pull_ok_f, pull_err, cancel_ok_f, cancel_err_f]);
    let _ = d.streams.fns.set(Fns { start_ok: start_ok_f, start_err, pull_ok: pull_ok_f, pull_err, cancel_ok: cancel_ok_f, cancel_err: cancel_err_f, stream_proto, reader_proto, ctl_proto });
    Ok(())
}

impl Browser {
    /// Replace the limits of the stream layer. Call it before scripts run.
    pub fn configure_streams(&mut self, limits: StreamLimits) {
        self.dom.streams.lim.set(limits);
    }

    pub fn stream_stats(&self) -> StreamStats {
        let st = self.dom.streams.st.borrow();
        StreamStats {
            streams: st.streams.len(),
            readers: st.readers.len(),
            queued_chunks: st.ctls.values().map(|c| c.queue.len()).sum(),
            queued_bytes: st.queued_bytes,
            pending_reads: st.readers.values().map(|r| r.reads.len()).sum(),
            callbacks: st.callbacks,
        }
    }
}
