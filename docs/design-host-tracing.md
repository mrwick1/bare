# Design: references that the host holds for an object

Status: proposal for owner review (plan step S0.4, before the events PR). No code is built.

## Problem

Design 6.8 gives the host two kinds of references: scoped (`Local`) and persistent (`Persistent`). A persistent handle is a root. That is correct for a timer: a timer must run even when no script can reach its callback. It is wrong for a listener. A listener lives only while its target is reachable. Design 6.8 also requires that a detached cycle of node, wrapper, listener and closure is collected. With a persistent handle for each listener, that cycle is never collected.

The S0.2 decisions moved "the edges that the host holds for an object" to the DOM wrapper PR. This page designs them. The events PR needs them first.

## Today (old engine)

- The host keeps `ObjId` and `Value` in its own tables: listeners (`DomState::listeners`, keyed by `Target`), event state, wrapper maps (`by_obj`, `by_node`) and 18 other subsystems with a `trace` function.
- `Host::trace(owner, out)` gives the values that `owner` keeps alive. The collector calls it for every object it reaches, except host functions (`eval.rs:1083`).
- `Dom::trace` then asks about 30 subsystems about that object. Most of them have nothing for it. So a collection costs about 30 probes for each reached object. Design 6.11 (round 2, item 4 and the fact on `Dom::trace`) already names this cost.
- `Host::prune(live)` runs once per collection. Each subsystem drops the entries of dead owners.
- Complexity: O(reached objects × subsystems) per collection, plus O(host entries) for `prune`. No cost on a store.

## How other engines do it

From memory of each engine's source. To verify before the build.

| Engine                  | The host keeps                                                 | The collector finds it through                                                                             |
| ----------------------- | -------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------- |
| V8 with Blink           | `TracedReference` in C++ objects of the Oilpan heap            | One unified heap: Oilpan traces the C++ object, and the C++ object visits its references. No root.         |
| SpiderMonkey with Gecko | `JS::Heap<T>` fields                                           | The trace hook of the reflector's class, plus the cycle collector for cycles across C++ and JS.            |
| Servo                   | `Heap<JSVal>` fields in DOM structs (`#[derive(JSTraceable)]`) | The trace hook of the reflector visits the DOM struct.                                                     |
| WebKit                  | Listeners as weak references from the wrapper                  | `visitChildren` of the wrapper, and opaque roots: a node subtree is live when one of its wrappers is live. |
| Ladybird                | `GC::Ptr` in web objects that are GC cells                     | Each cell visits its own edges (`visit_edges`).                                                            |

The common rule: the owner's trace visits the references. Only true roots (timers, pending tasks) are roots.

## Options

### A. Traced references and a tracer for each owner (recommended)

- `Traced`: an engine value that the host stores in its own structures. It is not a root. It is 8 bytes (the raw engine value). It is neither `Copy` nor `Clone`, so one store has one owner.
- An owner registers with the engine: `Scope::set_host_owner(obj: Local, key: u32)`. The key is a host number, for example the index of the target in the host's table. The engine keeps the key in the object (old engine: a side table; new engine: a header bit and a slot).
- When the collector marks an object with a host key, it calls `Host::trace(key, &mut Tracer)`. The host visits the `Traced` fields of that owner only. Objects without a key cost nothing.
- After marking, the engine calls `Host::prune(&Dead)` with the keys of dead owners. The host drops their entries.
- Reads and stores need a scope: `s.traced(&t) -> Local` and `s.store(&mut t, v)`. `store` is the place for the write barrier of a generational or incremental collector (design 6.8, barriers).
- Cost: one store per reference, no table entry, no reference count. The collector does one host call per owner, not one per object and subsystem. Memory: 8 bytes per reference, as today.
- Risk: the host runs code during a collection. It must not hold a `RefCell` borrow that `trace` needs. This is the same rule as today.

### B. Listener lists in the JavaScript heap

- The host keeps listener data in a hidden array of the wrapper (an internal slot). The engine traces it as normal data. The host keeps no engine values at all.
- Cost: each dispatch reads listener data through the interface (a `get` for each listener). Each `addEventListener` allocates in the heap. Hypothesis: dispatch is slower by several times, because today it reads a Rust `Vec`.
- Gain: no host code during a collection. This is the Ladybird model, but without native GC cells for the data.

### C. Owned persistent handles (ephemerons)

- `Scope::persist_owned(owner, v) -> OwnedPersistent`: the value is live while the owner is live. The engine keeps an ephemeron table of (owner, value) pairs and resolves it as it does for `WeakMap`.
- Cost: one table entry per reference (about 70 instructions for create and drop, as measured for persistent handles in #695), plus the ephemeron fixed point at each collection. The host still needs `prune` for its own tables.
- Gain: no host code during a collection.

### Comparison

|                           | A. Traced + owner tracer    | B. Lists in the JS heap            | C. Owned handles                       |
| ------------------------- | --------------------------- | ---------------------------------- | -------------------------------------- |
| Store of a reference      | 1 write                     | 1 heap write through the interface | table entry, about 70 instructions     |
| Per collection            | 1 call per reached owner    | none (normal tracing)              | ephemeron fixed point over all entries |
| Dispatch                  | Rust `Vec`, as today        | interface reads                    | Rust `Vec`, as today                   |
| Detached cycles           | collected                   | collected                          | collected                              |
| Host code during GC       | yes (`trace`, `prune`)      | no                                 | `prune` only                           |
| Change for the old engine | small: `Host::trace` exists | large                              | new ephemeron table                    |

A makes most of today's per-object probes unneeded (only owners are visited), and keeps the cost of a store at one write. B and C remove host code from the collection, but cost more on each listener.

## Proposed interface (option A)

```rust
pub struct Traced { raw: u64 }            // not Copy, not Clone

impl<'s, E: Engine> Scope<'s, E> {
    pub fn trace_ref(&mut self, v: Local<'_>) -> Traced;
    pub fn traced(&mut self, t: &Traced) -> Local<'s>;
    pub fn store(&mut self, t: &mut Traced, v: Local<'_>);
    pub fn set_host_owner(&mut self, obj: Local<'_>, key: u32);
}

pub trait Host<E: Engine> {
    // existing: call
    fn trace(&self, key: u32, t: &mut Tracer<'_, E>) {}
    fn prune(&self, dead: &[u32]) {}
}

pub struct Tracer<'a, E> { /* engine work list */ }
impl<E: Engine> Tracer<'_, E> {
    pub fn visit(&mut self, t: &Traced);
}
```

Rules:

- A `Traced` that is not visited by its owner's `trace` is a dangling reference after the next collection. Debug builds check this with a stress mode (plan step S0.x): collect at each allowed point and check that each read finds a live object.
- An object has at most one host key. A wrapper gets the key of its target.
- `Host::roots` goes away when the last subsystem uses persistent handles for its roots.

## Plan for the events PR

1. Add `Traced`, `Tracer`, `set_host_owner`, `Host::trace` and `Host::prune` to `bare-js-api`. Implement them in the old engine adapter. The old engine calls the interface `trace` only for objects with a host key.
2. Move `Listener::cb` and the values of `EventState` (`target`, `current`, `detail`, `related`) to `Traced`. Event targets register with their key.
3. Tests: a detached node with a listener whose closure holds the node is collected; a listener of a live node survives; a listener removed during dispatch; an event kept by a script after dispatch keeps its target.
4. Cost: the cost matrix with the gate of +0.5%. Expected: no change on dispatch rows. Hypothesis: fewer probes per collection on DOM-heavy rows, because only owners are visited. This needs the other subsystems to move before it shows in full.

## Open questions for the owner

1. Option A, B or C?
2. The key: a host number (proposed), or the engine object itself? A host number lets the host index its tables directly; an object key needs a hash map in the host, as today.
3. Debug check of unvisited `Traced` references: in the S0.x stress mode only (proposed), or in every debug build?
