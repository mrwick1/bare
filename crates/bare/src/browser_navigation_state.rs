//! Structured clone: `structuredClone(v)` and the `history` state, the page's side. The engine walks the graph and writes a record
//! (`Engine::clone_out`, `docs/design-host-clone.md`), and builds a new graph from it (`Engine::clone_in`). This file gives the engine
//! what it asks of the page ([`CloneHost`]): which objects are host objects, how a `MessagePort` of the transfer list moves, and the
//! `DataCloneError`. It also sets the limits. `pushState(data)` keeps the record (`Cloned`) and `history.state` builds the graph once per
//! entry (cached, so `history.state === history.state`). `structuredClone` runs both at once. It is not JSON: cycles, shared
//! references, `undefined`, `NaN`, `-0`, sparse arrays and lone surrogates survive.
//!
//! Supported (HTML "StructuredSerializeInternal"): primitives including BigInt; Boolean, Number, String and BigInt wrapper
//! objects; `Date`; `RegExp` (`lastIndex` is 0 in the copy); `Map`; `Set`; arrays and ordinary objects (own enumerable string
//! keys, copied through `[[Get]]` so getters run, in key order, with no host borrow; the prototype is lost, so a class instance
//! becomes a plain object); `ArrayBuffer`, every typed array and `DataView` (a buffer shared by two views is copied once);
//! `Error` and its six subtypes (`name`, `message`, `stack` and `cause`).
//! Transfer list (HTML "StructuredSerializeWithTransfer"): `clone_state_with` takes the listed objects (`ArrayBuffer` or `MessagePort`,
//! checked by `Dom::transfer_list`). A listed item is not copied. The engine moves a buffer (its bytes go in the record, the source is
//! detached) and asks [`CloneHost::transfer_out`] for a port: the key of a parked port, or (for a message to another page, after
//! `Dom::export_ports`) the end ids of a port in the hub. Both are a [`Transfer`] in the box of a `Moved::Host`. The record has no JS
//! object, so it can go to another heap. `materialize_with` builds the buffers and ports from it. A buffer in the list and in the graph
//! is one object in the result. The sources change only after the walk succeeded.
//! `DataCloneError`: a function, a symbol, a DOM wrapper, `URL`, `URLSearchParams`, an `AbortController` or `AbortSignal`, a Proxy, a
//! Promise, a WeakMap, a WeakSet, a WeakRef, an iterator, a generator, an `Intl` object, a Symbol wrapper, a detached buffer.
//! There is no internal Error slot in the engine: an object with an `Error.prototype` in its chain is an Error.
//! Bounds: `history` uses `NavLimits` (depth, nodes and bytes of one state, and the bytes of all states of a document). Over the bytes or nodes
//! cap, `pushState` throws a `DataCloneError`; over the depth cap it stops with a `Limit`.
//! `structuredClone` has its own: depth 256, 2^20 nodes, 64 MiB.

use crate::browser::*;
use bare_js::eval::{Error, Interp, LimitKind};
use bare_js::value::{ObjId, Value};
use bare_js_api::{CloneHost, CloneLimits, CloneOpts, Cloned, Engine, Local, Refused, Scope};
use std::any::Any;

/// A moved item of a record that is a host object (`Moved::Host`). It holds no JS object.
pub(crate) enum Transfer {
    /// A port that `Dom::park_port` took out of the port table. It stays inside one page.
    Port(u64),
    /// A port that moves to another page (`browser_port_hub.rs`): its end in the hub and the end of its peer, if the peer is in the hub.
    /// `Dom::export_ports` makes it from a `Port`. `Dom::import_ends` makes the new port.
    End { me: u64, remote: Option<u64> },
}

fn clone_err(d: &Dom, it: &mut Interp<'_>, what: &str) -> Error {
    d.dom_err(it, "DataCloneError", &format!("{what} could not be cloned."))
}

/// What the engine asks of this page during a clone.
impl<'q> CloneHost<Interp<'q>> for &Dom {
    fn is_host_object(&self, o: &Value) -> bool {
        matches!(o, Value::Object(o) if Dom::is_host_object(self, *o))
    }

    fn can_transfer(&self, o: &Value) -> bool {
        matches!(o, Value::Object(o) if self.port_movable(*o))
    }

    fn transfer_out(&mut self, o: &Value) -> Box<dyn Any> {
        let Value::Object(o) = o else { unreachable!("a listed object") };
        Box::new(Transfer::Port(self.park_port(*o)))
    }

    fn transfer_in<'s>(&mut self, s: &mut Scope<'s, Interp<'q>>, items: Vec<Box<dyn Any>>) -> Result<Vec<Local<'s>>, bare_js_api::Error<'s>> {
        let it = s.engine();
        match make_hosts(self, it, items) {
            Ok(os) => Ok(os.into_iter().map(|o| it.raw_local(&Value::Object(o))).collect()),
            Err(e) => Err(it.api_err(e)),
        }
    }

    fn data_clone_error<'s>(&mut self, s: &mut Scope<'s, Interp<'q>>, what: Refused) -> bare_js_api::Error<'s> {
        let it = s.engine();
        let what = match what {
            Refused::Function => "A function",
            Refused::Symbol => "Symbol",
            Refused::Object => "An object",
            Refused::DetachedBuffer => "An ArrayBuffer is detached and",
            Refused::DetachedTransfer => "A transferred item is detached and",
        };
        let e = clone_err(self, it, what);
        it.api_err(e)
    }
}

/// The new ports of the host items of a record, in list order.
fn make_hosts(d: &Dom, it: &mut Interp<'_>, items: Vec<Box<dyn Any>>) -> R<Vec<ObjId>> {
    let xf: Vec<Transfer> = items.into_iter().map(|b| *b.downcast::<Transfer>().expect("a port")).collect();
    let keys: Vec<u64> = xf.iter().filter_map(|t| if let Transfer::Port(k) = t { Some(*k) } else { None }).collect();
    let ports = d.make_ports(it, &keys)?;
    let ends: Vec<(u64, Option<u64>)> = xf.iter().filter_map(|t| if let Transfer::End { me, remote } = t { Some((*me, *remote)) } else { None }).collect();
    let moved = if ends.is_empty() { Vec::new() } else { d.import_ends(it, &ends)? };
    let (mut next, mut next_end) = (ports.iter(), moved.iter());
    Ok(xf.iter().map(|t| if let Transfer::Port(_) = t { next.next() } else { next_end.next() }.copied().expect("made")).collect())
}

/// The record of `v` with these limits and this transfer list. The listed buffers are detached and the listed ports are parked after
/// the walk succeeded.
fn out(d: &Dom, it: &mut Interp<'_>, v: Value, limits: CloneLimits, list: &[ObjId]) -> R<Cloned> {
    let base = it.stack_len();
    let root = it.raw_local(&v);
    let items: Vec<Local<'_>> = list.iter().map(|&o| it.raw_local(&Value::Object(o))).collect();
    let mut host = d;
    let r = it.clone_out(root, &CloneOpts { limits, transfer: &items }, &mut host).map_err(|e| it.api_unerr(e));
    it.stack_truncate(base);
    r
}

/// The state of `pushState`.
pub(crate) fn serialize(d: &Dom, it: &mut Interp<'_>, v: Value) -> R<Cloned> {
    let l = d.nav.lim.get();
    let limits = CloneLimits { bytes: l.max_state_bytes, depth: l.max_state_depth.min(128), nodes: l.max_state_nodes, names: ["history-state-bytes", "history-state-depth", "history-state-nodes"] };
    match out(d, it, v, limits, &[]) {
        // The size is the page's choice: a catchable error, as in Firefox. The depth cap stays a limit.
        Err(Error::Limit(LimitKind::Host("history-state-bytes" | "history-state-nodes"))) => Err(clone_err(d, it, "A state over the size cap")),
        r => r,
    }
}

/// The limits of `structuredClone` and of a message.
fn clone_limits() -> CloneLimits {
    CloneLimits { bytes: 64 << 20, depth: 256, nodes: 1 << 20, names: ["clone-bytes", "clone-depth", "clone-nodes"] }
}

/// `structuredClone(v, { transfer })` and a message: the copy, the new ports of the transfer list (in list order) and the byte cost.
/// `list` comes from `Dom::transfer_list`. With no list the engine copies in one pass and makes no record (`Engine::clone_copy`).
pub(crate) fn clone_with(d: &Dom, it: &mut Interp<'_>, v: Value, list: &[ObjId]) -> R<(Value, Vec<ObjId>, usize)> {
    if list.is_empty() {
        let base = it.stack_len();
        let root = it.raw_local(&v);
        let mut host = d;
        let r = it.clone_copy(root, &clone_limits(), &mut host).map(|(l, cost)| (it.raw(l), Vec::new(), cost)).map_err(|e| it.api_unerr(e));
        it.stack_truncate(base);
        return r;
    }
    let mut c = clone_state_with(d, it, v, list)?;
    let (val, ports) = materialize_with(d, it, &mut c)?;
    Ok((val, ports, c.cost()))
}

/// A record with structured-clone limits, and the transfer list as data. Resource limits remain uncatchable. The listed buffers
/// are detached and the listed ports are parked after the walk succeeded. A listed item that changed during the walk (a getter
/// detached it) throws `DataCloneError` before any item moves.
pub(crate) fn clone_state_with(d: &Dom, it: &mut Interp<'_>, v: Value, list: &[ObjId]) -> R<Cloned> {
    out(d, it, v, clone_limits(), list)
}

/// A new object graph for `c`. No JS runs (the objects are new, so no setter can be hit). A record with no moved item can be read again.
pub(crate) fn materialize(d: &Dom, it: &mut Interp<'_>, c: &mut Cloned) -> R<Value> {
    materialize_with(d, it, c).map(|r| r.0)
}

/// `materialize` with a transfer list: the graph and the new ports of the list, in list order. A listed buffer that no node uses is dropped.
/// The moved items leave `c`.
pub(crate) fn materialize_with(d: &Dom, it: &mut Interp<'_>, c: &mut Cloned) -> R<(Value, Vec<ObjId>)> {
    let base = it.stack_len();
    let mut host = d;
    let r = it.clone_in(c, &mut host).map(|ci| (it.raw(ci.value), ci.hosts.iter().filter_map(|&l| if let Value::Object(o) = it.raw(l) { Some(o) } else { None }).collect())).map_err(|e| it.api_unerr(e));
    it.stack_truncate(base);
    r
}
