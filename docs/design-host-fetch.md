# Fetch, XHR, streams and abort through the host interface

Status: proposal for the owner (plan step S0.4, fetch). Not decided. See "Open questions for the owner" at the end.

## Problem

`browser_fetch.rs` (2 981 lines), `browser_xhr.rs` (989), `browser_streams.rs` (954) and `browser_abort.rs` (428) keep engine values in their own tables. They give them to the old engine through four `roots`, four `trace` and four `prune` calls in `Dom`'s old `Host` impl (`browser.rs:898`, `:936`, `:963`). The old hook calls `trace` of each of them for every object that the collector reaches (`browser.rs:952` to `:960`).

After S0.2 and the events, timers and wrapper PRs, three kinds of reference exist: `Persistent` (a root), `Traced` (a reference that an owner holds) and `Local` (one scope). This page assigns each reference in these four files to one kind, lists what the old hook loses, and splits the move into PRs. Promises are on `docs/design-host-microtasks.md`; this page uses its methods.

## Current design

What the four files hold, by file (`crates/bare/src`):

| File    | State                                                                        | Engine values in it                                                                                                                                                                                                                 |
| ------- | ---------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| fetch   | `objs: IdMap<ObjId, F>` (`:485`, `F` at `:412`)                              | `Request`: headers, signal. `Response`: headers, stream. `Stream`: response. `Reader`: stream. `HeadersIter`: headers. 8 fields in 5 kinds.                                                                                         |
| fetch   | `queue`, `detached: Vec<Pending>` (`Pending` at `:424`)                      | `Pending::promise: Value`, `Pending::xhr: Option<(ObjId, u64)>`. Roots.                                                                                                                                                             |
| fetch   | `streams: Vec<OpenStream>` (`:517`)                                          | `OpenStream::x`, the XHR object of an open event stream. Root.                                                                                                                                                                      |
| fetch   | `Fetch` (`:551`): `fixed`, `protos`, `json_parse`, `json_stringify`, `drain` | Prototypes, constructors, two JSON functions and the `drain.js` function. Roots for the life of the realm.                                                                                                                          |
| xhr     | `Tables::objs: IdMap<ObjId, Xhr>` (`:115`, `Xhr` at `:82`)                   | `upload: ObjId`, `cache: Option<Value>`. `made`: 4 prototypes, 4 constructors (roots). `ups`, `prog`: sets and maps keyed by `ObjId`.                                                                                               |
| streams | `State { streams, ctls, readers }` (`:117`)                                  | `Stream`: `ctl`, `reader`, `St::Errored(Value)`. `Ctl`: `stream`, `source`, `start`, `pull`, `cancel`, `queue: VecDeque<(Value, usize)>`, `pull_fns`. `Rdr`: `stream`, `reads: VecDeque<Value>`. `Fns`: 9 template objects (roots). |
| abort   | `Tables { sigs, ctls }` (`:55`)                                              | `Sig`: `reason: Value`, `algos` (`Algo::Fetch(Value)`: a promise), `sources` and `deps` (weak `ObjId` lists). `ctls: ObjId -> ObjId`. `objs`: 4 objects (roots).                                                                    |

How they trace today (`browser.rs`):

- `roots` (`:898`): `fetch.roots`, `streams.roots`, `browser_abort::roots`, `browser_xhr::roots`.
- `trace` (`:936`): `fetch.trace`, `streams.trace`, `browser_abort::trace` and `browser_xhr::trace`. The last one is behind the flag `xhr.active`. Each call looks the owner up in its own map. Most calls find nothing.
- `prune` (`:963`): `fetch.prune`, `streams.prune`, `browser_abort::prune`, `browser_xhr::prune`. Each one runs `retain(|o| live(o))` over its maps. `Fetch::prune` then recounts the retained bytes and the readers over all entries (`recount`, `browser_fetch.rs:616`). `browser_abort::prune` also walks the weak lists of each signal and each `Algo::Remove` against the listener table.
- A response and its body stream hold each other (`Response::stream`, `F::Stream`). A reader and its stream hold each other. These are cycles that only tracing collects.
- No value is rooted during a call. A pending request leaves `queue` before its promise is settled (`run_network_tasks`, `browser_fetch.rs:2786`). The promise is then a raw `Value` in a Rust local across `finish`, which allocates. With a collection inside a task (S1.6) this is a bug. A `Persistent` or a `Local` fixes it.

Uses of the engine in these files (non-test sites; the full `Interp` method census is in `docs/design-host-interface.md`):

| File    | `Value` | `ObjId` | `it.` calls | Handler fns | Calls to this page's methods                                                                                                           |
| ------- | ------: | ------: | ----------: | ----------: | -------------------------------------------------------------------------------------------------------------------------------------- |
| fetch   |     306 |      46 |         135 |          68 | `host_new_promise` 7, `host_reject_promise` 3, `host_resolve_promise` 1, `host_checkpoint` 3, `host_iter_open` 2, `host_iter_next` 2   |
| xhr     |     116 |      27 |          32 |          39 | `host_checkpoint` 1                                                                                                                    |
| streams |     188 |      40 |          69 |          32 | `host_reject_promise` 10, `host_new_promise` 6, `host_bind` 6, `host_resolve_promise` 4, `host_promise_react` 2, `host_promise_then` 1 |
| abort   |      60 |      26 |          23 |          14 | `host_iter_open`, `host_iter_next`, `host_iter_fail` 1 each                                                                            |

The rest of the `it.` calls are `type_error` (57 in fetch, 11 in xhr, 13 in streams), `define_data`, `charge`, `get_value`, `call`, `bytes_of`, `new_array_buffer` and six `it.heap.*` reads in fetch. They belong to the core, errors, binary data and limits PRs, not to this one.

## Measurements

archMachine, `origin/main` 37df5bd1, release build with symbols, callgrind, one run for each row.

Cost of the old hook for these four subsystems, as the inclusive cost of the calls inside `Dom::trace` (probe plus real work), and `prune`:

| Row                      |   Total | `fetch`, `streams`, `abort`, `xhr` `trace` | `prune` of the four                                    |
| ------------------------ | ------: | -----------------------------------------: | ------------------------------------------------------ |
| `fetch_load objects 1`   |  95.5 M |                             0.68 M (0.71%) | 2.07 M (2.16%): `Fetch::prune`; the rest under 0.001 M |
| `streams_load churn 1`   |  31.2 M |                             0.40 M (1.29%) | 0.10 M (0.33%): `Streams::prune`                       |
| `streams_load flow 1`    | 215.7 M |                             0.70 M (0.32%) | under 0.01 M                                           |
| `xhr_load 1`             |  11.5 M |                             0.13 M (1.15%) | under 0.002 M                                          |
| `scheduler_load mixed 1` |  57.6 M |                             0.58 M (1.01%) | under 0.002 M                                          |
| `web_apis_load abort 1`  |  97.4 M |                             0.92 M (0.94%) | 0.53 M (0.55%): `browser_abort::prune`                 |
| `fetch_load post 1`      |  24.1 M |                             0.14 M (0.57%) | 0.045 M (0.19%): `Fetch::prune`                        |
| `loader_load dynamic 1`  |  18.6 M |                             0.05 M (0.27%) | under 0.001 M                                          |

Per call: `Fetch::trace` 53 instructions, `Streams::trace` 33, `browser_abort::trace` 31 (87 in `web_apis_load abort 1`, where signals have real edges), `browser_xhr::trace` 52. In `fetch_load objects 1` the collector calls the hook 5 820 times and the number of fetch objects is about 8 000 (1 000 rounds of 8 objects), so most calls find nothing.

Not measured: the cost of registering an owner for each object. Hypothesis, from the wrapper PR (+0.49% on `select_load churn`): 40 to 60 instructions for each object (`Owners::key`, `set_host_owner`). For 8 000 fetch objects that is 0.3 to 0.5 M (0.3 to 0.5% of `fetch_load objects 1`), against 0.68 M of hook calls and part of the 2.07 M of `Fetch::prune`. Hypothesis: net gain on that row, small loss on rows that make objects but never collect (`fetch_load post *`).

Moving a promise into a `Persistent`: about 70 instructions for create and drop (#695, `docs/progress.md`). `fetch_load post 1` makes 100 requests, so 7 K of 22.4 M (0.03%, hypothesis).

## How other engines do it

From memory of each engine's source. To verify before the build.

| Engine                  | Request, response, stream                                                                                                                                                                                                             | Abort signal                                                                                                                                   |
| ----------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------- |
| V8 with Blink           | `Request`, `Response`, `ReadableStream` are Oilpan objects. Members are `Member<T>`, traced by the object itself. A pending fetch keeps its resolver alive with `ScriptPromiseResolver` held by the loader (`ActiveScriptWrappable`). | `AbortSignal` is an Oilpan object with `abort_algorithms_` traced. `ActiveScriptWrappable` keeps it alive while it has listeners and may fire. |
| SpiderMonkey with Gecko | Cycle-collected C++ objects (`Request`, `Response`) with `NS_IMPL_CYCLE_COLLECTION` tables; pending fetches hold `Promise` with strong refs.                                                                                          | `AbortSignal` follows are cycle-collected; follower lists are weak.                                                                            |
| Servo                   | DOM structs with `Dom<T>` fields; traced by `JSTraceable`. Streams are DOM structs.                                                                                                                                                   | `AbortSignal` holds `DomRefCell<Vec<AbortAlgorithm>>`, traced.                                                                                 |
| Ladybird                | `GC::Ptr` fields in each web object; `visit_edges`. Fetch holds its promise capability in the fetch controller.                                                                                                                       | `AbortSignal` visits its algorithms; `source_signals` and `dependent_signals` are weak sets.                                                   |

The common rule: an object visits its own edges. A pending operation keeps its promise alive as a strong reference, not as an edge of an object. That matches the split that this page uses: `Persistent` for pending work, `Traced` for edges of an object.

## Options

### A. Split by lifetime: Persistent for pending work and constants, Traced with owners for edges (recommended)

Three classes:

1. Realm constants (prototypes, constructors, `Fns` templates, `json_parse`, `json_stringify`, `drain`, `objs` of abort, `made` of xhr). About 35 `Persistent` values, made once. They replace the four `roots` calls.
2. Pending work: `Pending::promise`, `Pending::xhr`, `OpenStream::x`. A `Persistent` is made when the request is queued and released when it settles. Whatever the script does with the promise, the request stays rooted. This is the current rule ("Root it while it waits").
3. Edges of an object: `Request::{headers, signal}`, `Response::{headers, stream}`, `F::Stream`, `F::Reader`, `F::HeadersIter`, `Xhr::{upload, cache}`, `Stream::{ctl, reader, Errored}`, `Ctl::{stream, source, start, pull, cancel, queue, pull_fns}`, `Rdr::{stream, reads}`, `Sig::{reason, algos}`, and `ctls`. They become `Traced`. Each object that has a table entry registers as an owner (`set_host_owner`) with a key. The key is an `Owner::Fetch(ObjId)`, `Owner::Stream(ObjId)`, `Owner::Xhr(ObjId)` or `Owner::Abort(ObjId)` in the owner table of `browser.rs:322`. `Host::trace(key)` finds the entry by that `ObjId` and visits its fields. `Host::prune(dead)` removes the entries of the dead keys and subtracts their bytes. No per-object probe remains in `Dom::trace`.

The weak lists of a signal (`sources`, `deps`) hold owner keys (`u32`) instead of `ObjId`. `prune(dead)` removes dead keys from the lists of the signals that survive.

- Cost: one `Persistent` for each pending request. One registration for each object with an entry. A trace call only for owners. `Fetch::prune` no longer retains over live entries (the totals change as entries go).
- Memory: `Traced` is 8 bytes, as `Value`. An owner key is 4 bytes in the entry and 16 bytes in the owner table.
- Risk: `Fetch::st`, the stream state and `Dom::st` are `RefCell`s. `trace` borrows them during a collection. A handler that holds `borrow_mut()` across an allocating engine call panics once collection can run in a task. Today it cannot. An audit of the borrow sites is part of each PR (not done: hypothesis that most sites are short).

### B. Everything persistent

- Each reference in the tables above becomes a `Persistent`, released when the owner entry is dropped.
- The owner entry is dropped only when the owner dies. A `Persistent` is a root, so the owner is reachable from its own entry through the cycle (response, stream, reader). It never dies. Fetch objects leak, and `retained` never drops.
- Rejected. It is correct only for pending work (class 2 of option A).

### C. State inside the engine object (V8 internal fields)

- The entry (`F`, `Xhr`, `Stream`, `Sig`) lives in the engine object as host data that the engine drops when the object dies and traces through a method of the data. The host keeps no map, no key and no `prune`.
- Cost: a read of the entry is one engine call (type check and downcast). A creation allocates host data inside the object. The engine needs a finalizer in the sweep and a trace hook for host data.
- Gain: no owner table, no registration, no `prune`. The new engine could keep the data in a cell kind.
- Against: it replaces the owner and key model that the owner decided on 2026-11-19 (`docs/design-host-tracing.md`), changes both engines, and moves every `objs.get(&this)` to the interface. Out of scope for S0.4. A candidate for S5.1.

### Comparison

|                             | A. Persistent and Traced               | B. All persistent                          | C. Host data in the object            |
| --------------------------- | -------------------------------------- | ------------------------------------------ | ------------------------------------- |
| Pending request             | 1 `Persistent` (about 70 instructions) | same                                       | same                                  |
| Object creation             | registration, 40 to 60 (hypothesis)    | `Persistent` for each edge (about 70 each) | host data allocation                  |
| Per collection              | 1 `trace` call per owner; no probes    | none, but leaks                            | 1 trace call per object with data     |
| Dead objects                | `prune(dead keys)`: O(dead)            | never                                      | the engine drops the data             |
| Detached cycles             | collected                              | never collected                            | collected                             |
| Hook calls that leave `Dom` | 4 roots, 4 trace, 4 prune              | 4 roots                                    | all                                   |
| Change for the old engine   | none beyond S0.2 and #697              | none                                       | large (finalizer, trace of host data) |
| Size                        | M, in 4 PRs                            | S                                          | XL                                    |

## Interface

One method is new for these four subsystems, and the owners and handles of S0.2 and the tracing page carry the rest:

```rust
/// A function that calls `target` with `this` fixed (host_bind). The engine traces both. For the reactions of a stream pull.
fn bind<'s>(&mut self, target: Local<'_>, this: Local<'_>) -> Result<Local<'s>, Error<'s>>;
```

Used by `browser_streams.rs` (6 sites in 3 functions). `host_bind` costs 342 instructions (`streams_load churn 1`, 1 000 calls, 1.09%). The templates are made once for each controller. An alternative that avoids the function object is a reaction that stores a host id and one value (`promise_then_host(p, ok_id, err_id, data)`). Not proposed: no row shows the cost.

Needed from other PRs, not added here: the promise, microtask and iteration methods (`design-host-microtasks.md`, PR M1); `host_scope` and the step budget (the calls PR); `bytes_of`, `new_array_buffer` and views (the binary PR); `type_error`, `define_data` and `charge` (the core and errors PRs).

## Old-engine calls that leave the old hook

Each PR removes its own lines from `browser.rs`:

| Hook    | Lines (`browser.rs`)                                                                                      | Leaves with |
| ------- | --------------------------------------------------------------------------------------------------------- | ----------- |
| `roots` | `self.fetch.roots`, `self.streams.roots`, `browser_abort::roots`, `browser_xhr::roots` (`:902` to `:916`) | F1          |
| `trace` | `browser_xhr::trace` and its `xhr.active` gate, `browser_abort::trace`                                    | F2          |
| `trace` | `self.fetch.trace`                                                                                        | F3          |
| `trace` | `self.streams.trace`                                                                                      | F4          |
| `prune` | `browser_xhr::prune`, `browser_abort::prune`                                                              | F2          |
| `prune` | `self.fetch.prune`                                                                                        | F3          |
| `prune` | `self.streams.prune`                                                                                      | F4          |

`browser_abort::prune` has an order rule: "Call after the listener table is pruned". With `prune(dead)` it runs in the same call as the listener removal (`Host::prune` of the interface), after the wrapper entries go.

## Risk to the cost rule

Rows that exercise the subsystem (`tools/js/cost_rows.txt`): `fetch_load post|bytes|objects|iterate` (scales 1, 10), `streams_load flow|many|pending|churn`, `xhr_load` (1, 100), `web_apis_load abort`, `loader_load mixed`, `scheduler_load mixed`.

- Rows that make many objects and few collections pay registration: `fetch_load objects *`, `streams_load churn *`, `web_apis_load abort *` (about 4 signals for each of 1 000 rounds at scale 1: hypothesis, from the script of the row). The gate (+0.5%) decides. The wrapper PR needed an exception (`select_load churn`, +0.49%) for the same reason.
- Rows that collect often gain: the hook share in the table above is the upper bound of the gain (0.3% to 1.3% for trace, up to 2.2% for `Fetch::prune`).
- `Persistent` of each request (F1): under 0.05% (hypothesis).
- F1 has no interface change, so it passes the gate unless a `Persistent` costs more than 70 instructions.

## Plan

Order: F1, then F2, F3, F4. F4 needs `bind` and PR M1 of the microtasks page.

1. F1, roots to `Persistent`. The four `roots` calls go. `Pending::promise`, `Pending::xhr` and `OpenStream::x` become `Persistent`. The constants (prototypes, constructors, templates, JSON functions, `drain`) become `Persistent`. Tests: a fetch whose promise the script dropped settles after a collection; a collection between `fetch()` and the reply keeps the XHR object; the constants survive 3 collections.
2. F2, XHR and abort as owners (smallest edge sets: 2 fields in `Xhr`, 3 in `Sig`). The owner variants, `trace`, `prune(dead)`, weak lists by key. The `xhr.active` gate goes. Tests: an XHR that the script dropped after `send()` is collected after it ends; a signal and its fetch promise (`Algo::Fetch`); `AbortSignal.any` sources and dependents after a collection; a key reused after prune answers for its new object only.
3. F3, fetch objects as owners. `F` entries get a key; totals (`retained`, `readers`) change as entries go. Tests: a detached response with a stream and a reader is collected; the existing `fetch_load objects` "zero retained entries" check; `Request::signal` keeps the signal.
4. F4, streams as owners, with `bind`. Pending reads are `Traced` in the reader. Tests: a stream whose controller holds queued chunks keeps them while the stream is reachable; a collection inside `pull` (needs the stress mode S0.x).
5. Each PR: Linux and the Windows VM; the cost matrix within +0.5% under both allocators; the rows of that PR measured again; progress.md entry.

## Open questions for the owner

1. Option A (proposed), B or C?
2. Fetch entries by key, or by object? Proposed: keep `IdMap<ObjId, F>` and give each entry an owner key; `trace(key)` goes through the owner table to the `ObjId`, then the map. Alternative: a slab indexed by key, plus `by_obj: IdMap<ObjId, u32>` for `this`. The slab makes trace and prune O(1) and adds one load to each handler call. Which first? (Proposed: the map, and the slab only if a row shows the extra lookup.)
3. Headers objects (4 of 8 objects in a round of `fetch_load objects`) hold no edges. They need a key only so that their entry is freed. Accept the registration for them (proposed), or free them by another rule (for example with their `Request` or `Response`)?
4. The handler bodies (about 150 functions in these four files): do they move to `Scope` and `Local` inside F1 to F4, or in a later PR? Same question as `design-host-microtasks.md`, question 1. This page assumes later.
