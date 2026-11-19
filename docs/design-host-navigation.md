# Location, history, storage, frames and the session through the host interface

Status: decided by the owner, 2026-11-19 (plan step S0.4, navigation). See "Decisions" at the end. Base: origin/main `37df5bd1`.

## Problem

Ten files (6,186 lines) hold the navigation side of the browser: `browser_navigation.rs` (`location`, `history`, the queues), `_url.rs` (URL records, no engine), `_params.rs` (`URL`, `URLSearchParams`, iterators), `_state.rs` (structured clone: `history.state`, `structuredClone`), `_storage.rs` (`localStorage`, `sessionStorage`), `browser_frames.rs` (window names, iframe windows), `browser_cookie.rs`, `browser_session.rs` (the outer driver), `browser_child.rs` and `browser_remote_window.rs` (child pages). Four kinds of engine contact:

1. The `URL` family: objects that point at each other (`url.searchParams`), kept in three tables.
2. Structured clone: the largest engine surface of the subsystem (37 distinct engine calls in one file), and 24% of one cost row.
3. Frames: window objects found by node, and traced by scanning all of them for each marked object.
4. The session: it names the engine types, because it picks the engine and its limits.

## Scope and assumptions

- Step 3 of `docs/design-lazy-properties.md` is built by another agent. I assume it covers `Source::Style` and `Source::RuleList` only. `Source::Storage` and `Source::Frames` objects hold no engine references, so they need nothing from it. Their `named_*` reads and writes are already on the host interface.
- One later PR changes the handler type `H` to the interface types for all subsystems (forms design, open question 1). The base set of handler methods is in `docs/design-host-forms.md`. This page lists what is more.
- The event-loop design owns `host_scope`, `host_checkpoint` and the promise calls. Here they are 8 driver sites (`host_scope`) and 2 (`host_checkpoint`).
- The wrapper owner (PR #700) exists: an element wrapper is an owner of its node.

## Current design

- `Nav` (`browser_navigation.rs:200`): `NavState` (`:166`) holds `urls`, `params` and `iters` as `IdMap<ObjId, _>`, the history `entries`, the queues and `store: WebStorage`. `NavObjs` (`:190`) holds 6 permanent objects. `areas` holds the two `Storage` objects.
- `Entry.cache: Option<ObjId>` (`:149`) is the object that `history.state` returned. It is a root while the entry lives. The history has at most 50 entries (`max_history_entries`).
- Edges (`:221`): `UrlObj.sp` (the `searchParams` object), `ParamsObj.url` (the `URL` it belongs to), `IterObj.sp`. The pair is a cycle through host tables, so a dropped pair is collected together. `trace` probes 3 tables for each marked object. `prune` (`:232`) is a `retain` with `live` over all 3, then it sums the bytes of the list again.
- Cap: `max_objects` (`1 << 16`, `:83`) refuses a new `URL`, `URLSearchParams` or iterator at `browser_navigation_params.rs:85` when the live tables reach it. Tables shrink only at a collection, which the old engine runs between tasks. This is a limit that guards a table.
- `Dom::is_host_object` (`browser_navigation.rs`, near `:246`) tests 7 tables to find the objects that structured clone must refuse (`DataCloneError`).
- Frames (`browser_frames.rs:60` to `:76`): `contexts` and `docs` map a node to a window object, `owned` a node to a document, `locs` is a set of `Location` objects. `trace` (`:634`) loops over all `contexts` and `docs` for each marked object, and compares the wrapper of each with the object. `prune` (`:648`) is a `retain`. The guard `frames.active()` (`browser.rs:937`) is true whenever `install` made the state, and `install` makes the window-name objects for every page, so the guard saves nothing (hypothesis; `frames::trace` has cost on every measured row).
- Structured clone (`browser_navigation_state.rs:121`, `:367`): `serialize` copies a graph into a native `State`, `materialize` builds a new graph. No object is kept. It calls the engine through `CloneClass`, 8 `clone_new_*` and `clone_coll_*` calls (`crates/js/src/eval_clone.rs:31` to `:125`) and 27 other calls (`own_keys`, `get_own_property`, buffer, view and bigint calls, string units).
- Storage (`browser_navigation_storage.rs:93`): `WebStorage` is native and shared (`Rc`) by all documents of a session. The `Storage` objects are permanent.
- Session (`browser_session.rs:67`, `:112`, `:134`): `EvalLimits`, `Exec` and `HeapLimits` choose the engine mode (`BARE_JS_EXEC`). `exec_from("new")` returns `bare_js2::NOT_READY`. Every document is a new `Browser`.
- Child pages (`browser_child.rs:361`): `roots` gives the remote windows of child pages. A child page has its own heap, so no object is shared (`browser_child.rs`, head). `RemoteWindow` (`browser_remote_window.rs`) is the only link.

## Uses of the engine

Over the 10 files (import occurrences outside `use` lines, and `it.*` calls by name).

| Item           | Uses | Item                                           |    Uses |
| -------------- | ---: | ---------------------------------------------- | ------: |
| `value::Value` |  421 | `eval::CloneClass`                             |      13 |
| `eval::Interp` |  136 | `heap::HeapError`                              |      14 |
| `value::ObjId` |   85 | `value::PropertyKey`                           |      11 |
| `eval::Error`  |   32 | `eval::EvalLimits`, `Exec`, `heap::HeapLimits` | 5, 4, 4 |

| `it.*` call   | Sites | `it.*` call                                          |   Sites |
| ------------- | ----: | ---------------------------------------------------- | ------: |
| `heap_error`  |    30 | `heap.view_parts`, `own_keys`, `get_own_property`    | 2, 2, 4 |
| `charge`      |    28 | `host_iter_open`, `host_iter_next`, `host_iter_fail` | 2, 2, 3 |
| `type_error`  |    28 | `clone_class`, `clone_new_coll`                      |    2, 2 |
| `define_data` |    25 | `heap.intern_units`, `str_units`                     |    5, 4 |
| `get_value`   |    11 | `heap.buffer_len`, `buffer_detached`, `bytes`        | 3, 2, 2 |
| `host_scope`  |     8 | `new_array_buffer`, `array_from`, `wall_now_ms`      |  2 each |

Distinct engine calls by file: `_state.rs` 37, `_params.rs` 19, `browser_navigation.rs` 16. `_url.rs` has none. Shared helpers add 16 sites (`add_fn` 13, `def_get` 3).

## Measurements

archMachine, origin/main `37df5bd1`, callgrind on a release build with symbols, one run per row. Per-function numbers are the self cost of the function that the old hook calls.

| Row                         |   Total | `collect` (incl.) | Old `Host::trace` | `navigation::trace` | `frames::trace` | `navigation::prune` |
| --------------------------- | ------: | ----------------: | ----------------: | ------------------: | --------------: | ------------------: |
| `navigation_load url 1`     | 136.3 M |             16.7% |             2.22% |       673 K (0.49%) |   203 K (0.15%) |      1.62 M (1.19%) |
| `navigation_load history 1` |  91.0 M |             22.8% |             3.25% |       205 K (0.22%) |   239 K (0.26%) |                 450 |
| `navigation_load storage 1` |  58.7 M |             31.1% |             4.33% |       176 K (0.30%) |   205 K (0.35%) |                 450 |
| `form_load submit 1`        |  44.6 M |             43.6% |             6.36% |       196 K (0.44%) |   229 K (0.51%) |                 450 |
| `session_load chain 1`      |   7.8 M |     no collection |                   |                     |                 |                     |

Other numbers from the same runs:

- `history 1`: `push_replace` is 33.2% inclusive. Of that, `serialize` is 14.8% and `materialize_with` is 9.3%. URL `parse` is 7.2%. So the clone walk is the hot engine path of this subsystem: every interface call in it costs on the main row.
- `url 1`: `url_ctor` 17.2%, `commit_params` 12.1%, URL `parse` 13.6%. The row makes 1,000 `URL`, 1,000 `URLSearchParams` and 1,000 iterator objects (3,000 owners with option A).
- `browser_navigation::install` is 254 K instructions on every page (2.5% of the `sheets_load` rows). Not studied further.
- The measured gain of moving `trace` and `prune` of this subsystem is 0.4% to 1.7% of a row: `url 1` 1.68% (trace 0.49 + prune 1.19) and, with frames, 1.83%. `frames::trace` costs 0.15% to 0.51% on rows with no frame at all.
- Cost of an owner: about 90 instructions (`select_load churn 1000`, wrappers, `docs/progress.md`). For `url 1` that is 3,000 objects, 270 K instructions, 0.20% of the row. Hypothesis: the `trace` of the 3,000 owners adds about as much. Then the net is a gain of about 1.3%. Not measured.

## How other engines do it

From memory of each engine's source. To verify before the build.

| Engine        | `URL` and `searchParams`                                                                                                       | Frames and windows                                                               | Structured clone                                                                                                                |
| ------------- | ------------------------------------------------------------------------------------------------------------------------------ | -------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------- |
| Blink with V8 | `DOMURL` holds `url_search_params_` as a traced member; the search params object holds a back pointer. Oilpan frees the cycle. | The frame owner element traces its content frame; the window is reached from it. | In the engine: `v8::ValueSerializer` and `ValueDeserializer`, with a delegate for host objects (blink `SerializedScriptValue`). |
| WebKit        | Same shape with reference counts and weak back pointers.                                                                       | `HTMLFrameOwnerElement` owns the content frame.                                  | In WebCore (`SerializedScriptValue`), written against JSC internals.                                                            |
| Gecko         | Cycle-collected pair with wrapper caches.                                                                                      | The frame loader of the element holds the browsing context.                      | In SpiderMonkey: `JSStructuredCloneWriter`, callbacks for host objects.                                                         |
| Servo         | A DOM struct with `Dom<URLSearchParams>`.                                                                                      | The iframe element holds its browsing context.                                   | `JS_WriteStructuredClone` of SpiderMonkey, with callbacks.                                                                      |
| Ladybird      | GC cells that visit each other.                                                                                                | The navigable and the element visit each other.                                  | In LibWeb, written against the realm API.                                                                                       |

The common rules: the parent owns or visits the child, and no engine scans all frames for each marked object. For structured clone, three engines have the algorithm inside the engine. WebKit and Ladybird write it in the embedder, over a wide engine API.

## Options

### URL family and frames

#### A. Owners with `Traced` edges; frames in the node owner (recommended)

- `URL`, `URLSearchParams` and iterator objects get a host key at creation. `UrlObj.sp`, `ParamsObj.url` and `IterObj.sp` become `Traced`. `Host::trace(key)` visits them. `Host::prune(dead)` removes the entries and subtracts the bytes of a dead list. The three `retain` loops go.
- Frames: the trace of the node owner (`browser.rs:781`) visits `contexts[n]` and `docs[n]` by one `IdMap` lookup for its own node. The scan over all frames goes. `locs` objects become owners.
- `is_host_object` becomes a test on the owner variant (one `Vec` read) instead of 7 table probes, once the object has a key. Hypothesis: it is called only on a clone of an object, so the gain is small.
- Cost: about 90 instructions for each object made, 16 bytes in the owner table.

#### B. Edges in engine slots

- The `URL` object holds its `searchParams` object in an engine slot. The engine traces it. The host keeps no table for the edge. A slot needs a new engine feature (`Scope::set_slot`, `slot`). The 104-byte object header of the old engine has no room. The new engine can have it.

#### C. Leave them on the old hook, last

- Nothing to do now. `frames::trace` and the 3 table probes stay on every marked object of every page.

### Permanent objects and the history cache

#### D. Persistent handles

- `NavObjs` (6), `areas` (2), the 2 window-name objects of frames, and the remote windows of child pages become `Persistent` handles. `Entry.cache` becomes an `Option<Persistent>`: it lives exactly as long as its entry, which the host controls. A dropped entry frees its slot at once (the timer design does the same).
- Cost: about 70 instructions for each handle, once. At most 50 entry handles.

### Structured clone

#### E. The walker stays in the browser, on a clone sub-trait (recommended for S0)

- The interface gets a sub-trait (`CloneOps: Engine`) of about 20 methods (below). `serialize` and `materialize` are generic over it. The browser is compiled for each engine (S0.2 decision), so a call costs what a direct call costs (hypothesis, to check on `history 1` with the interface adapter).
- The new engine implements the 20 methods in S1 or S2. The 467 lines stay in the browser.

#### F. The algorithm in the engine (V8 and SpiderMonkey)

- The interface gets `Engine::clone_out(v, transfer: &[Local], host: &mut dyn CloneHost) -> Result<Cloned, Error>` and `clone_in(&Cloned, ...)`. `CloneHost` gives callbacks for host objects (`MessagePort`, wrappers) and the transfer list. `Cloned` is the native `State` of today (it has no engine value, so it can cross heaps: `browser_navigation_state.rs`, head).
- Each engine writes the walk (467 lines) and can use its own layout. Hypothesis: a faster walk for dense arrays on the new engine. A bigger S1 and S2. Two copies of the limits and the error rules (`DataCloneError`, depth 256, 2^20 nodes, 64 MiB).

### Session

#### G. The session names both engines (S0.2 decision)

- `Session` picks the engine at start (`BARE_JS_EXEC`) and stays on the concrete types: `EvalLimits`, `Exec`, `HeapLimits` for `bare-js`, and the types of `bare-js2` for the new engine. No interface method. The only change is `Browser<E>` instead of `Browser`, in 9 `bare_js` paths of `browser_session.rs` and 5 of `browser_child.rs`.

### Comparison

|     | Work per collection                              | Work per object                                    | New interface                      | Size |
| --- | ------------------------------------------------ | -------------------------------------------------- | ---------------------------------- | ---- |
| A   | one trace call for each owner; no scan of frames | about 90 instructions (hypothesis for these kinds) | none                               | M    |
| B   | none from the host                               | a slot write                                       | `set_slot`, `slot`; engine feature | L    |
| C   | 3 probes and a frame scan for each marked object | none                                               | none                               | none |
| D   | none                                             | about 70 once                                      | none                               | S    |
| E   | none                                             | none                                               | about 20 methods                   | M    |
| F   | none                                             | none                                               | 2 methods, `CloneHost`             | L    |
| G   | none                                             | none                                               | none                               | S    |

## New interface methods

Beyond the base set (`docs/design-host-forms.md`: `type_error`, `charge`, conversions, `new_array`, `intrinsic`, prototype calls, `define_accessor`, `proto_from_new_target`, `string_utf16`, `wall_now_ms`):

| Method                                                                                                                                                   | Sketch                                                          | Semantics                                                                                                          | Sites |
| -------------------------------------------------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------ | ----: |
| `own_keys`                                                                                                                                               | `(&mut self, obj: Local) -> Result<Vec<Local<'s>>, Error<'s>>`  | Own string keys in order.                                                                                          |     3 |
| `own_enumerable_data`                                                                                                                                    | `(&mut self, obj, key) -> Result<Option<Local<'s>>, Error<'s>>` | The own enumerable property or `None`. It runs getters, as `[[Get]]`.                                              |     5 |
| `is_callable`                                                                                                                                            | `(&self, v: Local) -> bool`                                     | Callable test.                                                                                                     |     3 |
| `iter_open`, `iter_next`, `iter_close`                                                                                                                   | `(&mut self, v: Local) -> Result<Iter, Error>` and so on        | The iteration protocol for host code. Belongs to the event-loop design if it has `host_iter_*`.                    |     7 |
| `CloneOps` (E): `clone_class`                                                                                                                            | `(&self, v: Local) -> CloneClass`                               | `Plain`, `Date(f64)`, `RegExp`, `Map`, `Set`, `Error`, `Wrapper`, `Buffer`, `View`, `Other`.                       |     2 |
| `clone_new_date`, `clone_new_regexp`, `clone_new_error`, `clone_new_coll`, `clone_coll_put`, `clone_coll_entries`, `clone_new_wrapper`, `clone_new_view` | as in `eval_clone.rs:82` to `:125`                              | Make the object of each class; read and fill map and set entries.                                                  |    11 |
| `new_array_buffer`, `buffer_len`, `buffer_bytes`, `buffer_detached`, `buffer_transfer`                                                                   | `(&mut self, ...)`                                              | Buffer data and detach. `buffer_bytes` borrows engine memory, so it takes `&Scope` (no collection while borrowed). |    10 |
| `view_parts`, `view_type`, `view_detached`, `bigint_parts`, `bigint_from_parts`                                                                          | `(&self, v: Local)`                                             | Typed array and BigInt data.                                                                                       |     8 |

The `Item` and `Pending` types and `Host::named_*` already cover `Storage` and the window names. `Abort::Limit(name)` covers `Limit(Host(name))`. The session needs no method (Option G). `Host::call` itself is not changed here (see forms, question 1).

## Leaves the old hook

At `37df5bd1`:

| Call                                                 | Line in `browser.rs` | After                                                  |
| ---------------------------------------------------- | -------------------- | ------------------------------------------------------ |
| `browser_navigation::roots`                          | `:905`               | persistent handles (`Entry.cache`, `NavObjs`, `areas`) |
| `browser_frames::trace` (guarded)                    | `:937`               | the node owner's trace                                 |
| `browser_navigation::trace`                          | `:939`               | `Host::trace(key)`                                     |
| `browser_navigation::prune`, `browser_frames::prune` | `:964`, `:965`       | `Host::prune(dead)`                                    |
| `self.kids.roots` (child pages)                      | about `:931`         | persistent handles                                     |

## Risk to the cost rule

Rows that touch the subsystem: `navigation_load` (12 rows: url, iterate, subclass, history, storage, route at scales 1 and 10), `session_load` (4), `type_load`, and every row with a collection (through `frames::trace`).

- `navigation_load url` and `iterate` make 3,000 and more objects for each scale unit. A registers an owner for each. Break-even is about 24,000 objects for `url 1` if the whole 1.7% is gained (hypothesis). Measure on callgrind first.
- `navigation_load history` is the row for E. It runs 24% in the clone walk. Any extra cost per engine call (a non-inlined trait call, a slot copy) shows at once. Gate: `history 1` and `10` within +0.5% under both allocators, with the adapter as the new engine.
- No row has an iframe, `contentWindow`, `window.frames[i]` or a `Location` of a frame (grep of `crates/bare/examples`, `tools/js/cost_rows.txt`). A change to the frame trace is not measured by the matrix. The first PR on frames must add a row (50 iframes, a collection with 100,000 marked objects).
- No row sets `location.href`, `hash` or `reload` outside `route`. `route` covers fragment, search and reload.

## Plan

1. Add a `frames` case to `navigation_load` (or a `frames_load` example) and rows. Record the base with callgrind. No browser change.
2. (If not done by another design.) The base set and the `own_keys`, `own_enumerable_data`, `is_callable` methods, with adapter tests.
3. Option A for the `URL` family, and persistent handles for `Entry.cache`, `NavObjs` and `areas` (D). Tests: a dropped `URL` and its `searchParams` are collected together; a held `searchParams` keeps its `URL`; `history.state === history.state` after a collection; an evicted entry frees its handle; a key that is reused answers for the new object only. Gates: `navigation_load` rows and the full matrix.
4. Option A for frames: the node owner visits `contexts[n]` and `docs[n]`; `locs` become owners. Tests: an iframe window lives while its element wrapper does; a window that a script holds survives the removal of the iframe.
5. Option E: the clone sub-trait, `serialize` and `materialize` generic over it. Gate: `history 1` and `10` on both allocators, and the 123 tests of the module (146 for forms).
6. Child pages and the session: persistent handles for the remote windows. No interface change.
7. Windows VM and Linux for each PR.

## Open questions for the owner

1. Option A (proposed), B or C for the `URL` family and the frames?
2. Structured clone: E (the walker stays in the browser, about 20 methods; proposed) or F (the algorithm in each engine)?
3. The cap `max_objects` (65,536 live `URL`, `URLSearchParams` and iterator objects): keep it in the old engine, and drop it when the engine can collect in a task (proposed)?
4. Who defines `iter_open`, `iter_next` and `iter_close`: this PR or the event-loop design?
5. A `frames` case and rows before any change to the frames (proposed: yes, PR 1)?
6. Order: the navigation PRs first, because the measured gain is the largest of the three subsystems (1.7% on `url 1`)?

## Decisions (owner, 2026-11-19)

The owner accepted every proposed answer.

1. Option A for the `URL` family and the frames.
2. Structured clone: option E. The walker stays in the browser behind the `CloneOps` sub-trait.
3. Keep the cap `max_objects` in the old engine. Drop it when the engine can collect in a task.
4. The event-loop design (M1 of `design-host-microtasks.md`) defines `iter_open`, `iter_next` and `iter_fail`. This page adds `iter_close` only if it has a call site.
5. Add a `frames` case and rows before any change to the frames, as PR 1.
6. The navigation PRs go first.
