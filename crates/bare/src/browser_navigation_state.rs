//! Structured clone: `structuredClone(v)` and the `history` state, the page's side. The engine walks the graph and writes a record
//! (`Engine::clone_out`, `docs/internal/design-host-clone.md`), and builds a new graph from it (`Engine::clone_in`). This file gives the engine
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

use crate::eng::{Eng, Val};
use crate::browser::*;
use crate::browser_scoped::*;
use bare_js_api::{Abort, CloneHost, CloneLimits, CloneOpts, Cloned, Engine, Local, Refused, Scope};
use std::any::Any;

/// A moved item of a record that is a host object (`Moved::Host`). It holds no JS object.
pub(crate) enum Transfer {
    /// A port that `Dom::park_port` took out of the port table. It stays inside one page.
    Port(u64),
    /// A port that moves to another page (`browser_port_hub.rs`): its end in the hub and the end of its peer, if the peer is in the hub.
    /// `Dom::export_ports` makes it from a `Port`. `Dom::import_ends` makes the new port.
    End { me: u64, remote: Option<u64> },
}

/// The clone destination can be storage or a live agent.
struct BrowserClone<'a, E: Eng> {
    dom: &'a Dom<E>,
    storage: bool,
}

impl<E: Eng> CloneHost<E::Vm> for BrowserClone<'_, E> {
    fn shared_buffer_cluster(&self) -> Option<&bare_js_api::clone::AgentCluster> {
        if self.storage { None } else { self.dom.agent_cluster.get() }
    }

    fn is_host_object(&self, o: &E::Raw) -> bool {
        matches!(E::kind(*o), Val::Object(o) if self.dom.is_host_object(o))
    }

    fn can_transfer(&self, o: &E::Raw) -> bool {
        matches!(E::kind(*o), Val::Object(o) if self.dom.port_movable(o))
    }

    fn transfer_out(&mut self, o: &E::Raw) -> Box<dyn Any + Send> {
        let Val::Object(o) = E::kind(*o) else { unreachable!("a listed object") };
        Box::new(Transfer::Port(self.dom.park_port(o)))
    }

    fn transfer_in<'s>(&mut self, s: &mut Scope<'s, E::Vm>, items: Vec<Box<dyn Any + Send>>) -> Result<Vec<Local<'s>>, bare_js_api::Error<'s>> {
        let os = make_hosts(self.dom, s, items)?;
        Ok(os.into_iter().map(|o| self.dom.obj_local(s, o)).collect())
    }

    fn data_clone_error<'s>(&mut self, s: &mut Scope<'s, E::Vm>, what: Refused) -> bare_js_api::Error<'s> {
        let what = match what {
            Refused::Function => "A function",
            Refused::Symbol => "Symbol",
            Refused::Object => "An object",
            Refused::DetachedBuffer => "An ArrayBuffer is detached and",
            Refused::DetachedTransfer => "A transferred item is detached and",
        };
        self.dom.dom_err_s(s, "DataCloneError", &format!("{what} could not be cloned."))
    }

    fn write_host_object(&mut self, o: &E::Raw) -> Option<(Box<dyn Any + Send>, usize)> {
        let Val::Object(o) = E::kind(*o) else { return None };
        crate::browser_web::clone_out(self.dom, o)
    }

    fn read_host_object<'s>(&mut self, s: &mut Scope<'s, E::Vm>, data: &dyn Any) -> Result<Local<'s>, bare_js_api::Error<'s>> {
        match data.downcast_ref::<crate::browser_web::WebClone>() {
            Some(w) => crate::browser_web::clone_in(self.dom, s, w),
            None => Err(self.data_clone_error(s, Refused::Object)),
        }
    }
}

/// The new ports of the host items of a record, in list order.
fn make_hosts<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, items: Vec<Box<dyn Any + Send>>) -> Res<'s, Vec<E::Obj>> {
    let xf: Vec<Transfer> = items.into_iter().map(|b| *b.downcast::<Transfer>().expect("a port")).collect();
    let keys: Vec<u64> = xf.iter().filter_map(|t| if let Transfer::Port(k) = t { Some(*k) } else { None }).collect();
    let ports = d.make_ports(s, &keys)?;
    let ends: Vec<(u64, Option<u64>)> = xf.iter().filter_map(|t| if let Transfer::End { me, remote } = t { Some((*me, *remote)) } else { None }).collect();
    let moved = if ends.is_empty() { Vec::new() } else { d.import_ends(s, &ends)? };
    let (mut next, mut next_end) = (ports.iter(), moved.iter());
    Ok(xf.iter().map(|t| if let Transfer::Port(_) = t { next.next() } else { next_end.next() }.copied().expect("made")).collect())
}

/// The record of `v` with these limits and this transfer list. The listed buffers are detached and the listed ports are parked after
/// the walk succeeded.
fn out<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, v: Local<'s>, limits: CloneLimits, list: &[E::Obj], storage: bool) -> Res<'s, Cloned> {
    let base = s.engine().stack_len();
    let items: Vec<Local<'s>> = list.iter().map(|&o| d.obj_local(s, o)).collect();
    let mut host = BrowserClone { dom: d, storage };
    let r = s.engine().clone_out(v, &CloneOpts { limits, transfer: &items }, &mut host);
    // An error is a local above `base`: it stays.
    if r.is_ok() {
        s.engine().stack_truncate(base);
    }
    r
}

/// The state of `pushState`.
pub(crate) fn serialize<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, v: Local<'s>) -> Res<'s, Cloned> {
    let l = d.nav.lim.get();
    let limits = CloneLimits { bytes: l.max_state_bytes, depth: l.max_state_depth.min(128), nodes: l.max_state_nodes, names: ["history-state-bytes", "history-state-depth", "history-state-nodes"] };
    match out(d, s, v, limits, &[], true) {
        // The size is the page's choice: a catchable error, as in Firefox. The depth cap stays a limit.
        Err(bare_js_api::Error::Abort(Abort::Limit("history-state-bytes" | "history-state-nodes"))) => Err(d.dom_err_s(s, "DataCloneError", "A state over the size cap could not be cloned.")),
        r => r,
    }
}

/// The record of an `indexedDB` value: `structuredClone` limits, no transfer.
pub(crate) fn serialize_storage<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, v: Local<'s>) -> Res<'s, Cloned> {
    out(d, s, v, clone_limits(), &[], true)
}

/// The limits of `structuredClone` and of a message.
fn clone_limits() -> CloneLimits {
    CloneLimits { bytes: 64 << 20, depth: 256, nodes: 1 << 20, names: ["clone-bytes", "clone-depth", "clone-nodes"] }
}

/// `structuredClone(v, { transfer })` and a message: the copy, the new ports of the transfer list (in list order) and the byte cost.
/// `list` comes from `Dom::transfer_list`. With no list the engine copies in one pass and makes no record (`Engine::clone_copy`).
pub(crate) fn clone_with<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, v: Local<'s>, list: &[E::Obj]) -> Res<'s, (Local<'s>, Vec<E::Obj>, usize)> {
    if list.is_empty() {
        let mut host = BrowserClone { dom: d, storage: false };
        let (copy, cost) = s.engine().clone_copy(v, &clone_limits(), &mut host)?;
        return Ok((copy, Vec::new(), cost));
    }
    let mut c = clone_state_with(d, s, v, list)?;
    let (val, ports) = materialize_with(d, s, &mut c)?;
    Ok((val, ports, c.cost()))
}

/// A record with structured-clone limits, and the transfer list as data. Resource limits remain uncatchable. The listed buffers
/// are detached and the listed ports are parked after the walk succeeded. A listed item that changed during the walk (a getter
/// detached it) throws `DataCloneError` before any item moves.
pub(crate) fn clone_state_with<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, v: Local<'s>, list: &[E::Obj]) -> Res<'s, Cloned> {
    out(d, s, v, clone_limits(), list, false)
}

/// A new object graph for `c`. No JS runs (the objects are new, so no setter can be hit). A record with no moved item can be read again.
pub(crate) fn materialize<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, c: &mut Cloned) -> Res<'s, Local<'s>> {
    let mut host = BrowserClone { dom: d, storage: true };
    s.engine().clone_in(c, &mut host).map(|result| result.value)
}

/// `materialize` with a transfer list: the graph and the new ports of the list, in list order. A listed buffer that no node uses is dropped.
/// The moved items leave `c`.
pub(crate) fn materialize_with<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, c: &mut Cloned) -> Res<'s, (Local<'s>, Vec<E::Obj>)> {
    let mut host = BrowserClone { dom: d, storage: false };
    let ci = s.engine().clone_in(c, &mut host)?;
    let ports = ci.hosts.iter().filter_map(|&l| obj_id::<E>(s, l)).collect();
    Ok((ci.value, ports))
}

impl<E: Eng> Dom<E> {
    /// Decode an arriving message. A clone exception produces a messageerror.
    pub(crate) fn receive_clone<'s>(&self, s: &mut Sc<'s, E>, c: &mut Cloned) -> Res<'s, (E::Raw, Vec<E::Obj>, bool)> {
        match materialize_with(self, s, c) {
            Ok((data, ports)) => Ok((s.engine().raw(data), ports, false)),
            Err(bare_js_api::Error::Throw(_)) => {
                if let Some(hub) = self.kids.hub() {
                    hub.release(c.moved());
                }
                Ok((E::null(), Vec::new(), true))
            }
            Err(error) => Err(error),
        }
    }
}
