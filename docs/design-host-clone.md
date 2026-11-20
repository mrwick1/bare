# Structured clone as a native engine algorithm

Status: design for owner review, 2026-11-20 (plan step S0.4, navigation step 5, option F). Base: origin/main `a8049517`. No code is changed. Every number in "Measurements" is measured. Every number in "Expected cost" is a hypothesis until the spike of plan step 1 measures it.

## Problem

`docs/design-host-navigation.md` chose option E for structured clone: the walk stays in the browser and calls the engine through a `CloneOps` sub-trait (about 20 methods). An agent built it (branch `origin/t3/clone-sub-trait`, commit `55594ea6`, not merged). Its 65 cases in `browser_clone_ops_tests.rs` give the same output as the old walk. It failed the cost gate:

| Row                          | Change over base, codegen-units=1, glibc | jemalloc |
| ---------------------------- | ---------------------------------------: | -------: |
| `navigation_load history 1`  |                                   +2.76% |   +2.46% |
| `navigation_load history 10` |                                   +3.00% |   +2.85% |

These numbers come from the task brief. They are not measured again here. The limit is +0.5%, which is about 430 instructions per state (below). The overrun is about 2.4 K instructions per state. Callgrind showed the cost spread over many small calls (`scope`, `escape_scope`, `define`, `as_str`, `type_of`, `charge`, `clone_class`, `clone_keys`, `Rc` allocations in `copy_units`). No one call is the cause. The cause is the design: a walk that makes about 10 interface calls for each object pays a fixed price for each call, and the old walk is already 21.7 K instructions for a state of 3 objects.

Owner decision (2026-11-20): build option F. The engine walks plain data itself. It calls the host only for host objects, for transfer, and for limits. This is how V8 (`ValueSerializer` with a `Delegate`) and SpiderMonkey (`JSStructuredCloneWriter` with callbacks) work.

This page gives the design that option F needs.

## Scope and assumptions

- Only structured clone. The `URL` family, frames and the session are in `docs/design-host-navigation.md` and are built or decided elsewhere.
- The interface is `bare-js-api` (`crates/js-api/src/lib.rs`). It must not name a type of the old engine (`ObjId`, `Value`, `StrId`, `Heap`, `Elem`).
- The old engine collects between tasks. The new engine (`crates/js2`) also collects inside a task, at an allocation (`docs/design-engine-rewrite.md`, 6.8 and 6.11). The design must be safe for both.
- The behavior of today's walk is frozen: the 65 cases and 2 cap tests of `browser_clone_ops_tests.rs` at `55594ea6`. The expected text in that file was made with the old walk (the brief says so). This page treats it as the oracle.

## Current design

Where the code lives: `crates/bare/src/browser_navigation_state.rs`, 467 lines, one file.

- `serialize` (`:121`, `history`), `clone_state_with` (`:142`, `structuredClone`, messages and transfer) and `walk` (`:164`) make a `State`. `ser` (`:190`) and `props` (`:319`) are the recursive walk.
- `materialize` and `materialize_with` (`:367`, `:372`) build a new object graph from a `State`.
- The engine side is `crates/js/src/eval_clone.rs` (125 lines): `CloneClass`, `clone_class`, `clone_coll_entries`, 6 `clone_new_*`, `clone_coll_put`. The walk also calls the heap directly: `own_keys`, `get_own_property`, `get_value`, `define_data`, `put_value`, `intern_units`, `str_units`, `bigint_parts`, `bigint_from_parts`, `buffer_*`, `view_*`, `new_object`, `new_array`.

Data structures:

- `State { root: SV, nodes: Vec<SNode>, bytes }`. `SNode { kind: SKind, props: Vec<(SKey, SV)> }`. `SV` is a value (24 bytes) with `Rc<[u16]>` strings and `Rc` BigInts. `SKey` is an index or an `Rc<[u16]>` (16 bytes).
- Sizes (measured with a copy of the types in a scratch program, `rustc -O`, x86_64): `SV` 24, `SKey` 16, `SKind` 88, `SNode` 112, one property pair 40 bytes.
- Identity: `HashMap<ObjId, u32>` (`seen`) with the default SipHash, and a second map for the transfer list.
- Order of work: the whole walk first (it runs getters), then the transfer checks and the moves, then, later and possibly in another page, `materialize_with`.

Time and memory:

- Time is O(nodes + properties + bytes) for both walks. The constants are large (below).
- The `State` of a small object is many allocations. The history state of `navigation_load history` (3 objects, 8 properties, 7 strings) takes about 11 allocations and about 1.1 KB (hypothesis: from the type sizes with a `Vec` capacity of 4; not measured). `history` keeps up to 50 of them.
- Limits (`Bounds`): `history` uses `NavLimits` (depth capped at 128, nodes, bytes: `history-state-bytes`, `-depth`, `-nodes`). `structuredClone` and messages use depth 256, 2^20 nodes, 64 MiB (`clone-depth`, `clone-nodes`, `clone-bytes`). The byte count is a cost model (48 per node, `2 * len + 16` per string, 16 per property and so on), not a real size. A message adds 16 and the queue caps (`max_queued_message_bytes`) use it.
- Over the history byte or node cap, `serialize` turns the limit into a catchable `DataCloneError`. Every other limit is an uncatchable `Limit(Host(name))`.

Host objects today: `Dom::is_host_object` (`browser_navigation.rs:264`) probes 7 tables, and `is_platform_object` 8 more. All these objects are refused with a `DataCloneError`: DOM wrappers, events, `URL`, `URLSearchParams`, `AbortController`, `AbortSignal`, `Location`, `History`, `Storage`, and the platform objects (media query lists, style and attribute objects, node lists, datasets, geometry). No host object is cloned as data. The only host objects that cross are `MessagePort`s in a transfer list (`transfer_list`, `port_movable`, `park_port`, `make_ports`, `export_ports`, `import_ends`). `ArrayBuffer`s in the list are moved by the walk itself. A `Blob`, `File` or `ImageData` object is not cloneable in bare today (a gap, not a decision).

Call sites (12 call sites in 8 files):

| Caller                                                                                           | Function                                         | What it needs                                                               |
| ------------------------------------------------------------------------------------------------ | ------------------------------------------------ | --------------------------------------------------------------------------- |
| `history.pushState`, `replaceState` (`browser_navigation.rs:695`)                                | `serialize`                                      | A `State` that lives in the entry (kept up to 50 entries).                  |
| `history.state`, `popstate` (`:299`, `entry_state`)                                              | `materialize`                                    | A new graph from the entry state, once per entry (cached in `Entry.cache`). |
| `structuredClone` (`browser_encoding.rs:39`), and the `indexedDB` shim (`shims/idb.js:6`, which calls `structuredClone`) | `clone_with` | The copy at once. It is serialize then materialize in one heap. |
| `window.postMessage` (`browser_basics.rs:320`) | `clone_with` | The same. |
| `MessagePort.postMessage` (`browser_scheduler.rs:1010`, `:1025`)                                 | `clone_with`; `clone_state_with` + `materialize` | The copy at post time, plus the byte cost.                                  |
| A message to another page (`browser_scheduler.rs:1315`, `:1489`, `browser_remote_window.rs:161`) | `clone_state_with`, then `export_ports`          | A `State` and a transfer list that cross a heap (`Wire`, `Out`).            |
| Receive from another page (`browser_scheduler.rs:1418`, `browser_child.rs:718`)                  | `materialize_with`                               | A new graph and new ports in the receiving heap.                            |
| Port hub (`browser_port_hub.rs`)                                                                 | holds `State`, `Transfer`                        | It stores `Wire` values.                                                    |

Other callers: `indexedDB` is a JavaScript shim (`browser_shims.rs`, `shims/idb.js`) that calls the global `structuredClone`, so it uses the first row. `BroadcastChannel` does not exist (`browser_scheduler.rs:65`). `Worker` is a stub that never runs a script and does not read a message (`browser_worker.rs`, head). A real worker needs a clone between heaps, which the record below gives. No form code calls the walk (grep of `browser_navigation_state` in `crates/bare/src`).

## Measurements

archMachine (AMD Ryzen 9 5900HX), origin/main `a8049517`, release build with symbols in `/data/bare-tmp/s0/design-clone/target` (`CARGO_PROFILE_RELEASE_DEBUG=1`, no strip). Instructions: `tools/js/instr.sh` (minimum of 3 runs, glibc with the tcache tunable). Per-function numbers: callgrind, one run for each row.

| Row                          | Instructions |                      Walk (`serialize` or `clone_state_with`) | `materialize_with` | Per state or clone                                                          |
| ---------------------------- | -----------: | ------------------------------------------------------------: | -----------------: | --------------------------------------------------------------------------- |
| `navigation_load history 1`  |   85,956,359 |                                               13.31 M (15.3%) |      8.38 M (9.6%) | 13.3 K + 8.4 K = 21.7 K                                                     |
| `navigation_load history 10` |  792,321,020 |                                               132.5 M (16.6%) |     83.6 M (10.5%) | 13.3 K + 8.4 K = 21.7 K                                                     |
| `web_apis_load clone 1`      |  182,540,980 |                                                53.3 M (28.9%) |     19.7 M (10.7%) | `clone_with` is 75.1 M (40.8%) for 1,000 clones and 1,000 refused functions |
| `scheduler_load messages 1`  |   31,125,968 | not measured (scalar messages skip the walk, `clone_payload`) |                    |                                                                             |

The callgrind totals of `history 1` and `history 10` are 86.90 M and 796.6 M. They differ from the `instr.sh` numbers by 1.1% and 0.5% (callgrind runs one process under another allocator state).

One `history` state is `{i, self, tags: ['a', 'b', i], nested: {k}}`: 3 objects, 8 properties, 9 values, 7 strings, one cycle. The +0.5% limit is 0.5% of 86.9 M, 434 K, so 434 instructions for each of the 1,000 states.

Cost of the engine calls inside the walk, per call (callgrind, calls counted over the 1,000 states, rounded; the walk makes these calls for each state):

| Call                                        | Per call | Calls per state |        Share of 21.7 K |
| ------------------------------------------- | -------: | --------------: | ---------------------: |
| `Heap::own_keys`                            |      750 |               3 |                  2.2 K |
| `Interp::get_value`                         |      232 |               8 |                  1.9 K |
| `Heap::get_own_property`                    |      138 |              10 |                  1.4 K |
| `HashMap::insert` (`seen`)                  |      346 |               3 | 1.0 K (+0.6 K SipHash) |
| `clone_class`                               |      229 |               3 |                  0.7 K |
| `Dom::is_host_object`                       |      199 |               3 |                  0.6 K |
| `Dom::key("length")`                        |      326 |               1 |                  0.3 K |
| `Interp::define_data`                       |      473 |               8 |                  3.8 K |
| `Interp::put_value` (`length` of the array) |    1,228 |               1 |                  1.2 K |
| `Heap::intern_units`                        |      155 |               7 |                  1.1 K |
| `Heap::new_object`, `new_array`             |      132 |               3 |                  0.4 K |
| `__rust_alloc` (walk side)                  |       63 |               8 |                  0.5 K |

The rows overlap (a call can contain another). The sum is a guide, not a split. Three facts follow:

1. The walk does 10 property reads and 8 property writes for 8 properties. Each one is a generic engine path (lookup by key, attribute checks, shape transition by key, generic `[[Get]]`).
2. `put_value(length)` and `define_data` are the generic `[[Set]]` and `[[DefineOwnProperty]]`. A new object that the walk makes needs neither.
3. About 4 K of the 21.7 K is `is_host_object`, `clone_class`, `HashMap` and `Rc` strings: work for classification and identity, not for the data.

Option E had to add its own call cost to all of this, because it left the generic paths in place and added a trait call in front of each.

## How other engines do it

Verified against the public headers on 2026-11-20 (`include/v8-value-serializer.h`, `js/public/StructuredClone.h`, WebKit `SerializedScriptValue.h`, Ladybird `StructuredSerialize.h`). A statement marked "from memory" was not verified.

| Engine                                                              | Where the walk lives                                                                                                | What the host gives                                                                                                                                                                                                                                                                                                                                                                                             | Record                                                                                                                                                                                                                                    |
| ------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| V8 (`ValueSerializer`, `ValueDeserializer`)                         | In the engine. The embedder calls `WriteValue(context, value)` and `ReadValue(context)`.                            | `Delegate`: `ThrowDataCloneError(message)`, `HasCustomHostObject` and `IsHostObject(isolate, object)`, `WriteHostObject(isolate, object)`, `GetSharedArrayBufferId`, `ReallocateBufferMemory`. Reading: `ReadHostObject`, `GetSharedArrayBufferFromId`. `TransferArrayBuffer(id, buffer)` on both sides. `WriteUint32` and `WriteRawBytes` are base-128 varints and raw bytes for use inside `WriteHostObject`. | One byte buffer (`Release()` gives the bytes). A header with a version. Integers are varints. From memory: tags are one byte; dense and sparse array tags; object back-references by id; an id map keeps visited objects strongly rooted. |
| SpiderMonkey (`JSStructuredCloneWriter`, `JSStructuredCloneReader`) | In the engine: `JS_WriteStructuredClone`, `JS_ReadStructuredClone`.                                                 | `JSStructuredCloneCallbacks`: `write` and `read` (host objects, with a tag in a user range), `reportError`, `canTransfer`, `writeTransfer`, `readTransfer`, `freeTransfer`, `sabCloned`. `freeTransfer` runs when a clone fails or the data is dropped unread.                                                                                                                                                  | A byte buffer (`JSStructuredCloneData`) of `uint64` pairs (from memory). `StructuredCloneScope::SameProcess` allows raw pointers and a move of buffer memory. `DifferentProcess` copies bytes.                                            |
| WebKit (`SerializedScriptValue`)                                    | In WebCore, written against JSC internals (from memory: it reads the `Structure` of an object for a fast path).     | The factory takes the transfer list and the `MessagePort`s: `create(globalObject, value, transfer, ports, ...)`. Blobs, image buffers and file system handles are in side lists of the value (`blobURLs()`, `transferredImageBufferIdentifiers()`).                                                                                                                                                             | `wireBytes()` is a `Vector<uint8_t>`. Shared buffer contents are a side list (`sharedBufferContentsArray()`).                                                                                                                             |
| Ladybird (LibWeb)                                                   | In LibWeb, over the realm API (`JS::Value`, `JS::Object`). From memory: each platform object has `serialize` steps. | `structured_serialize_with_transfer(realm, value, transfer_list)` and `structured_deserialize_with_transfer(record, realm)` return `Deserialized TransferRecord`. `AllowSharedArrayBuffers` is an argument.                                                                                                                                                                                                     | `IPCSerializationRecord` and `StorageSerializationRecord`. The layout is in another header (to verify).                                                                                                                                   |

The common rules:

- Three of four engines keep the record as bytes, and the bytes are opaque to the host.
- V8 and SpiderMonkey do the walk in the engine. The host sees only host objects, transfer and the error. WebKit and Ladybird do the walk in the embedder over a wide engine API. This is option E.
- The host never sees plain data. The host throws the error (`ThrowDataCloneError`, `reportError`). The DOM exception type is the host's choice.
- A transferred buffer is moved by the engine (`TransferArrayBuffer`). A transferred port is a host object, written by the host.
- A failed clone must free what the host moved (`freeTransfer`). Bare moves items only after the walk succeeded, so it has nothing to free.

## Options

All options keep the observable behavior of the old walk: same output, same errors, same getter order, same caps, same step charges. They differ in the interface, the record and who owns the work.

### E2. A coarse sub-trait (reference, not recommended)

- The walk stays in the browser. The interface gives one call for each object, not one for each property: `clone_class(v)`, `own_props(obj) -> Vec<(Key, Local)>` (a getter runs inside), `new_object_with(keys, values)`.
- Record: the old `State` tree.
- Cost: the number of calls drops from about 10 to about 3 for each object. The `State` allocations, the `HashMap` and the key conversions stay. Hypothesis: E2 saves about a third of E's overrun and does not reach the old walk. It does not remove the work. It is listed so that the owner sees the middle road.

### F1. A native walk with a tree record

- `clone_out(v, opts, host) -> Cloned` walks inside the engine. `Cloned` is the old `State` tree, moved to `bare-js-api` and made engine-neutral (no `Rc` of `u16` for keys: strings as `Box<[u16]>`).
- Owner of the walk: the engine. Host objects, transfer and the error go to a `CloneHost` trait.
- Record: nodes in a `Vec`, properties in a `Vec` for each node, strings in boxes. About 11 allocations for the `history` state.
- Gain: the generic engine paths go. The allocations stay.

### F2. A native walk with a byte record (recommended)

- The same interface as F1. The record is one byte buffer like V8's: a tag byte for each value, varints, one string table for property names, back-references by id. The engine never builds nodes.
- One allocation for a state (hypothesis: about 50 bytes for the `history` state, counted by hand from the format below, not measured).
- Materialize reads the buffer in order and builds each object with its final shape at once.

### F3. A fused copy with no record (add-on to F2, recommended)

- `clone_copy(v, limits, host) -> (value, cost)`: serialize and materialize in one pass for a clone inside one heap with no transfer list. It builds the new graph while it walks the old one. No record exists.
- Used by `structuredClone(v)` and same-heap `postMessage` with no transfer list (the callers that run `clone_with` with an empty list).
- It does not apply to `history` (the state must outlive the walk) or to a message for another page (it must cross a heap).
- Why it is correct: no script can reach the new objects, so no getter can see a half-built copy. The spec runs the whole serialize step before the deserialize step. The only visible difference is a heap-limit `Limit` error, which can now come before a later getter ran. A `Limit` ends the script, so no one sees the difference. A listed transfer item makes the call use F2.

### Comparison

|                   | Walk    | Interface calls into the host for a plain graph | Record                              | Allocations for the `history` state |  Expected `history 1` change (hypothesis) |
| ----------------- | ------- | ----------------------------------------------- | ----------------------------------- | ----------------------------------: | ----------------------------------------: |
| Old walk (base)   | browser | none (direct heap calls)                        | `State` tree                        |                            about 11 |                                         0 |
| E (built, failed) | browser | about 10 per object (from the brief)              | `State` tree                        |                            about 11 |           +2.4 M (+2.76%, from the brief) |
| E2                | browser | about 3 per object                              | `State` tree                        |                            about 11 |                      about +1 M to +1.5 M |
| F1                | engine  | 3 `is_host_object` calls per state              | tree, engine-neutral                |                            about 11 |                               about -12 M |
| F2                | engine  | the same 3                                      | bytes                               |                              1 or 2 |                               about -17 M |
| F2 + F3           | engine  | the same 3                                      | bytes, or none for a same-heap copy |                     1 or 2, or none | the same on `history`; `clone` rows lower |

The F1 and F2 numbers are from the cost budget below. They are not measured.

## Recommended design: F2 and F3

### Interface (`bare-js-api`)

The record and the host trait are engine-neutral. They name only `Local`, `Scope`, `Error`, `Abort`, `Engine::Raw` and plain data.

```rust
/// Limits of one walk, and the names of their `Abort::Limit` errors. The engine counts nodes, depth and bytes (the cost model below).
#[derive(Clone, Copy)]
pub struct CloneLimits {
    pub bytes: usize,
    pub depth: usize,
    pub nodes: usize,
    /// The name of the limit for bytes, depth and nodes.
    pub names: [&'static str; 3],
}

pub struct CloneOpts<'a, 's> {
    pub limits: CloneLimits,
    /// The transfer list. The host checked it (`Dom::transfer_list`): objects, no duplicates, none detached.
    pub transfer: &'a [Local<'s>],
}

/// What the engine refuses. The host makes the `DataCloneError` text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refused { Function, Symbol, Object, DetachedBuffer, DetachedTransfer }

/// A moved item of a record, in transfer-list order.
pub enum Moved {
    /// The bytes of an `ArrayBuffer`. The engine detached the source.
    Buffer(Vec<u8>),
    /// What `CloneHost::transfer_out` gave. The engine never looks inside.
    Host(Box<dyn std::any::Any>),
}

/// A structured clone as data. It holds no engine value, so it can wait in a history entry or cross to another heap.
pub struct Cloned { /* bytes: Box<[u8]>, moved: Vec<Moved>, cost: usize */ }
impl Cloned {
    pub fn cost(&self) -> usize;
    pub fn moved_mut(&mut self) -> &mut [Moved];   // `Dom::export_ports` rewrites the host items
}

pub struct CloneIn<'s> { pub value: Local<'s>, pub hosts: Vec<Local<'s>> }

pub trait CloneHost<E: Engine> {
    /// Is `o` an object that the host owns (a wrapper, an event, a `URL`, a platform object)? The engine refuses it, unless the transfer list holds it.
    fn is_host_object(&self, o: &E::Raw) -> bool;
    /// May the host move `o`, an item of the transfer list that is not an `ArrayBuffer`? A port that no transfer moved.
    fn can_transfer(&self, o: &E::Raw) -> bool;
    /// Move `o` out of this page. The engine calls it after the walk succeeded and the checks passed, once for each such item, in list order.
    fn transfer_out(&mut self, o: &E::Raw) -> Box<dyn std::any::Any>;
    /// New objects for the moved host items, in list order.
    fn transfer_in<'s>(&mut self, s: &mut Scope<'s, E>, items: Vec<Box<dyn std::any::Any>>) -> Result<Vec<Local<'s>>, Error<'s>>;
    /// The error to throw for a refused value (`DataCloneError`).
    fn data_clone_error<'s>(&mut self, s: &mut Scope<'s, E>, what: Refused) -> Error<'s>;
}

// Three methods on `Engine`:
fn clone_out<'s>(&mut self, v: Local<'_>, o: &CloneOpts<'_, '_>, h: &mut dyn CloneHost<Self>) -> Result<Cloned, Error<'s>>;
fn clone_in<'s>(&mut self, c: Cloned, h: &mut dyn CloneHost<Self>) -> Result<CloneIn<'s>, Error<'s>>;
fn clone_copy<'s>(&mut self, v: Local<'_>, l: &CloneLimits, h: &mut dyn CloneHost<Self>) -> Result<(Local<'s>, usize), Error<'s>>;
```

- `is_host_object` and `can_transfer` take `&E::Raw`, not a `Local`. A call pushes no handle. The old engine's `Raw` is `Value`.
- Methods that make a value or an error take a `Scope`. The engine builds it with `bare_js_api::enter` and `Scope::engine`, the way `api_call_host` does for `Host::call` (`crates/js/src/eval_api.rs:280`). The host methods run only for a host object, a transfer item or an error, never for plain data.
- `dyn CloneHost`: the engine walk is compiled once in the engine crate. A host call is one indirect call. A plain graph makes 3 of them per `history` state (hypothesis: about 15 instructions in all).
- `Box<dyn Any>` for host items: only a transferred port makes one. The browser keeps its own `Transfer` enum (`Port(u64)`, `End { me, remote }`) inside the box, and `export_ports` and `import_ends` downcast. The engine stays free of generics.
- The cost of the calls is not the limit. A call per host object is a few hundred instructions at most. Option E failed with a call per property.

Host side after the change. `State` becomes `Cloned`. `Wire` (`browser_port_hub.rs`), `Out` and the message struct of the child pages (`browser_child.rs:171`) keep a `Cloned` and read its `cost()`. `clone_with`, `clone_state_with`, `serialize` and `materialize*` shrink to the host glue: build `CloneOpts`, map the history limits to `DataCloneError`, `impl CloneHost for Dom`. Estimate: `browser_navigation_state.rs` goes from 467 lines to about 130 (hypothesis).

### Record format

Facts: the record is in memory only and never stored. No version is needed. The format is private to `bare-js-api`. The browser never reads the bytes.

One module of `bare-js-api` (`clone`, about 300 lines, safe code only, `#![forbid(unsafe_code)]` stays) holds the format for both engines:

- `RecordWriter` and `RecordReader`: the tags, the varints, the string table, the back-reference ids.
- The cost model: one function for each charge (`node`, `string(len)`, `key(len)`, `entry`, `buffer(n)`, `bigint(limbs)`, `moved(n)`), and the limit checks with the names. The constants are the ones of `browser_navigation_state.rs:108` to `:188`.
- The step charges: the same totals as today (1 for each value, 1 for each key, 1 for each map entry, `len / 32` for a string, `n / 64` for a buffer). An engine can add them for a whole object at once. The total is the same.

So the two engines do not have two copies of the limits, the cost model and the format. Each engine has only its own walk (about 300 lines): how to classify an object, how to list its properties, how to make an object. The 65 cases and the cap tests check both walks through `Browser<E>`.

The tags (all integers are LEB128 varints; a number is 8 bytes; from memory this follows V8's layout, but it is our own):

| Tag                              | Payload                                                                                                                     |
| -------------------------------- | --------------------------------------------------------------------------------------------------------------------------- |
| `Undef`, `Null`, `True`, `False` | none                                                                                                                        |
| `Int`                            | zigzag varint, for a number that is an integer in `i32` range and is not `-0`                                               |
| `Num`                            | 8 bytes, little endian                                                                                                      |
| `Str8`, `Str16`                  | length, then Latin-1 bytes or UTF-16 units. A lone surrogate needs `Str16`.                                                 |
| `Big`                            | sign, limb count, 8 bytes for each limb                                                                                     |
| `Ref`                            | id of an earlier object (a cycle or a shared object)                                                                        |
| `Obj`                            | properties, then `End`. A property is a key and a value.                                                                    |
| `ArrD`                           | length, then that many values (`Hole` for a hole), then non-index properties, then `End`. A dense array.                    |
| `Arr`                            | length, then (index key, value) pairs, then `End`. A sparse array.                                                          |
| key tags                         | `KNew` (a string, which takes the next number in the string table), `KRef` (a number of the table), `KIdx` (an array index) |
| `Date`, `RegExp`                 | the time; two strings                                                                                                       |
| `Map`, `Set`                     | entries, then `End`                                                                                                         |
| `Error`                          | the name string, then the optional message, stack and cause values                                                          |
| `Wrapper`                        | the primitive value                                                                                                         |
| `Buf`                            | length, then the bytes                                                                                                      |
| `View`                           | element type, byte offset, length, then the buffer (a `Ref` or an inline `Buf`). The buffer comes first.                    |
| `Moved`                          | the number of the item in `moved`                                                                                           |
| `Host`                           | reserved: a host tag and a payload. No host object is written today (see question 5).                                       |

An object takes its id when its tag is read, before its children, so a cycle works. A view takes its id after its buffer. A view cannot be the parent of a cycle, so this is safe, and it removes the "view first, buffer later" two-pass loop of `materialize_with`.

Why a string table for keys: a `history` state or an array of 1,000 objects repeats the same keys. A repeated key is a `KRef` of one or two bytes. At read time the engine interns each key once for the whole record. A dense array has no keys at all.

Size of the `history` state by hand: `Obj` (1) + `i`: `KNew` 4 + `Int` 2 + `self`: `KNew` 6 + `Ref` 2 + `tags`: `KNew` 6 + `ArrD` 2 + `Str8` 3 + `Str8` 3 + `Int` 2 + `End` 1 + `nested`: `KNew` 8 + `Obj` 1 + `k`: `KNew` 3 + `Int` 2 + `End` 1 + `End` 1 = 48 bytes. The old `State` is about 1.1 KB (hypothesis above). So 50 history entries drop from about 55 KB to about 2.4 KB.

### The walk: rules the engine must keep

These rules are the contract. Each one is covered by a case of the 65. The order matters, because the first failing check decides the error.

1. Order of checks for an object `o`: already seen (`Ref`); in the transfer list; callable (`Refused::Function`); `host.is_host_object` (`Refused::Object`); class `Other` (`Refused::Object`); depth cap; node cap; cost of the node. Then the node takes its id.
2. Plain object: snapshot the own string keys (indices first, then names in creation order; symbols skipped). For each key, in order: test that the property is still own and enumerable, then `[[Get]]` (a getter runs here, with no host borrow), then the value. A key that a getter deleted is skipped.
3. Map and Set: take the entries before the first recursion (a getter can change the collection). Keys and values are walked in order.
4. Error: `name` goes through `[[Get]]`. `message`, `stack` and `cause` are read as own data properties only, and `message` goes through `ToString`. An object with `Error.prototype` in its chain is an Error (there is no error slot in the old engine). The prototype of the copy is the one of the error type named by `name`, or `Error`.
5. Buffer: refuse if detached. A view: refuse if its buffer is detached, then walk the buffer first, then write the view.
6. Transfer list: after the walk, a listed item that no node used still costs 64 + its bytes. Then check every item (a buffer is not detached now; `host.can_transfer` for the rest). Only when all pass, move each one in list order: detach the buffer and keep its bytes (`Moved::Buffer`), or call `host.transfer_out`. A getter that detached a listed buffer gives `Refused::DetachedTransfer` before any item moves.
7. Limits: `Abort::Limit(names[k])` for bytes, depth and nodes. `serialize` for `history` maps the bytes and nodes names to a `DataCloneError` as today. The mapping stays in the browser, because the catchable choice is the page's.
8. The prototype is lost (a class instance becomes a plain object). `RegExp.lastIndex` is 0. `-0`, `NaN`, sparse arrays and lone surrogates survive.
9. Errors: `Throw` and `Abort` go up through the walk as `Error<'s>`. A thrown `DataCloneError` is the value that `data_clone_error` returned.

How getters and identity stay correct and fast:

- Identity table: a list of up to 8 `(object, id)` pairs scanned linearly, then an open-addressing table with a multiplicative hash of the object address or index. No SipHash (hypothesis: about 40 instructions for an insert, against 346 + 196 measured today).
- The visited objects are strong roots for the whole walk, as in V8's identity map. In the new engine, which collects inside a task, a visited object must not die and give its address to a new object. In the old engine nothing collects during a walk.
- The shape fast path (below) must survive a getter that changes an object while the walk is inside a child. Rule: after the walk returns from a child that is an object, compare the shape of the parent with the shape at the start. If it changed, fall back to the generic per-key path for the rest of the keys. A primitive child cannot run script, so it needs no check. A value that a getter overwrote in a slot is read at its own turn, so the order of `[[Get]]` stays the spec's.

### Memory layout and complexity

- Walk: O(nodes + properties + bytes). Native stack: one frame for each level of depth, at most 256 levels.
- Record: one `Vec<u8>`, grown by doubling from 128 bytes, then trimmed with `into_boxed_slice` (no realloc if full). For a large buffer the writer reserves its length first.
- Identity table: 8 pairs on the stack, then one heap allocation.
- Read: no intermediate nodes. The engine keeps a `Vec` of the objects it made (`id` to object), rooted as handles (new engine) or as plain ids (old engine).
- F3: no record. The identity table maps a source object to its copy.

### Fast paths in the old engine

These are inside `crates/js/src/eval_clone.rs` and `heap`, which can read the object layout. They are not in the interface.

- A plain object in shape mode with every property a default data property (`A_W | A_E | A_C`, no `A_ACC`), no `lazy` bit and no exotic kind: read the keys and the values straight from the shape entries and the slots (`Entry { key, slot, attrs }`, `Repr`). No `own_keys` `Vec`, no `get_own_property`, no `get_value`.
- A dense array: walk `elems: Vec<Option<Value>>` (a `None` is a hole). The `length` is one read.
- Make: for the first object of a key sequence, build the shape by transitions and keep it in a small per-call map from the key list (the table positions in the record) to the final shape. The next object with the same keys gets its final shape in one step and its slots from a slice. An array gets its `elems` filled directly and its `length` set once.
- A dictionary object, an accessor, an exotic object, an object with more than 128 properties: the generic native path. It reads through `own_keys` and `get_value`, as the old walk does, but without a `State` tree.
- `clone_class`: 16 side-table probes today (229 per call). Plan step 4 gives the object a header bit set when a side table gets the object (`date_new`, `coll_new`, `regexp_create`, promise, weak, intl, iterator, generator: about 14 sites). Only an object with the bit goes to the probes. Do it only if the profile of step 3 shows the probes. The `lazy` byte of `Object` uses 2 of its 8 bits (`heap.rs:395`), so a bit is free (hypothesis: not checked against the size budget of the struct).

### Expected cost on `history 1` and `history 10` (hypothesis)

All numbers here are estimates. Each is built from the per-call costs measured above, replaced by the cost of one direct slot read, one byte write or one new object. None is measured. The spike of plan step 1 measures them.

Serialize, per state (3 objects, 9 values, 8 properties, 7 strings):

| Item                                                  | Today |                        Native (hypothesis) |
| ----------------------------------------------------- | ---------------: | -----------------------------------------: |
| Classify 3 objects (`clone_class` + `is_host_object`) |            1.3 K | 0.7 K (probes kept, `is_host_object` kept) |
| Keys and values: 8 properties                         |            5.5 K |                                      0.5 K |
| Identity (3 inserts, 1 hit)                           |            1.6 K |                                      0.1 K |
| Strings (`units`: 5 keys and 2 values) | 0.4 K | 0.4 K |
| Allocations (`__rust_alloc`, `grow_one`) | 0.9 K | 0.2 K |
| Rest (charges, checks, recursion, `Dom::key`, `Result` plumbing): the balance | 3.6 K | 0.5 K |
| Total | 13.3 K | about 2.4 K |

Materialize, per state:

| Item                                       | Today | Native (hypothesis) |
| ------------------------------------------ | ---------------: | ------------------: |
| 3 new objects (`new_object`, `new_array`)  |            0.4 K |               0.4 K |
| 8 property writes (`define_data`)          |            3.8 K |               0.3 K |
| Array `length` (`put_value`)               |            1.2 K |               0.0 K |
| 5 keys and 2 strings interned (7 × 155)    |            1.1 K |               1.1 K |
| `SV` to `Value`, `Vec`s, `Result` plumbing: the balance |            1.9 K |               0.2 K |
| Total                                      |            8.4 K |         about 2.0 K |

"Today" is the measured per-call cost of the table above, except the rows marked "the balance", which hold what is left of the measured total. The native column is the estimate.

So about 4.4 K for a state against 21.7 K. That is about 17 M less on the 86 M of `history 1` (about -20%) and the same share on `history 10`. The interning of keys is now the largest part of the read. A small cache of the last keys, an engine detail, would cut it. It is not planned.

A miss of the fast path (a dictionary object, an accessor) costs more: 2 to 3 times the fast path, and still less than today (hypothesis).

`web_apis_load clone`: the graph has a `Map`, a `Set`, a `Date`, a `RegExp`, a typed array and an `Error`, which take the class paths. F3 skips the record. Hypothesis: `clone_with` falls from 75 M to between 25 M and 40 M for 1,000 clones. The refused-function case (1,000 times) costs a `DataCloneError` through the host and does not change.

Other rows: the walk code is not on any other row's path. The `postMessage` rows with scalar messages skip the walk (`clone_payload`). The engine binary grows by about 600 lines of code and the browser binary shrinks by about 340. Any row can move through code layout. The standing codegen-unit rule covers that.

## How js2 implements the same interface

`crates/js2` has the value and the heap allocator (`value.rs`, `heap.rs`, 609 lines in all) and no objects yet. `docs/design-engine-rewrite.md` (6.1 to 6.3) puts an object as a shape pointer and inline slots, and a plain object has an 8-byte header.

- The interface needs no old-engine type. `Local`, `Scope`, `Engine::Raw`, `Cloned`, `CloneHost` and `Moved` are in `bare-js-api` already or are plain data.
- js2 implements `clone_out`, `clone_in` and `clone_copy` over its own objects: read a shape and its slots, write a shape and slots. The shape fast path is the natural path there. The "side-table bit" is a kind in the header (hypothesis: the design already has kinds for Date, Map and the rest).
- `RecordWriter`, `RecordReader`, the cost model and the limit checks come from `bare-js-api`, so js2 has no new format to design.
- GC: js2 collects inside a task at an allocation (design 6.8, 6.11). The visited objects of a walk and the objects that `clone_in` and `clone_copy` make are rooted in a handle scope for the length of the call. No raw object pointer lives in a Rust `Vec` across an allocation.
- Strings: the record has `Str8` and `Str16`, so it does not depend on the string layout of the engine. The key table maps to js2 atoms.
- The browser tests run with `Browser<E>`, so the 65 cases run against js2 with no new test (S3 and later).

## Plan

Every PR has Linux and Windows VM tests. Every PR that touches the engine or the browser runs the cost matrix with the standing codegen-unit rule: no row over +0.5% under both allocators, and `history 1` and `10` at or below the base. Base: the commit that the PR rebases on, built fresh. The gate for identical behavior is `browser_clone_ops_tests.rs` from commit `55594ea6` (65 cases and 2 cap tests), renamed `browser_clone_tests.rs`, plus the existing 123 tests of the module.

1. Spike, in a scratch tree, never merged. Write `clone_out` and `clone_in` for the data classes only (primitives, strings, plain objects, dense arrays, the cycle) in the old engine, with the byte format and the shape fast path, and hand the rest to the old walk. Measure `history 1` and `10`. Gate: at least 8.5 M fewer instructions on `history 1` (half of the hypothesis of 17 M). If it fails, stop and report before any other step. Record the numbers in `docs/progress.md`.
2. `bare-js-api`: the `clone` module (format, cost model, limit checks) with unit tests: round trip of every tag, varint edges, string-table reuse, limits at each side, a record that is cut short returns an error and does not panic. No engine change. Gate: `cargo test --locked -p bare-js-api`.
3. Old engine and browser together: `clone_out`, `clone_in` with every class, the transfer list and `CloneHost for Dom`. The browser's `State`, `walk`, `ser`, `props`, `materialize*` go. Generic paths only (no fast path yet). Gates: the 65 cases and the cap tests give the same text; `state_bytes=17600` of `navigation_load history 1` is unchanged (it is the cost model, so it proves the model is the same); the frozen cost model test; the cost matrix.
4. The fast paths (shape objects, dense arrays, the shape map, the header bit if the profile asks). Add a differential test: random graphs with getters that delete, add and redefine keys, run on the fast path and with the fast path switched off (a test-only flag), and compare the dump. Gate: `history 1` and `10` at or below the base, and as near the hypothesis as the spike showed.
5. `clone_copy` for `structuredClone` and same-heap `postMessage`. Gate: `web_apis_load clone 1` and `10` and `scheduler_load messages` at or below the base.
6. Later, with the owner keys of the navigation plan: replace the probes of `is_host_object` with a header test (an object with a host owner key is a host object). It needs every host object to have a key, which the other S0.4 PRs build. Not part of this series.
7. js2: S3 gives it the three methods and the 65 cases pass on it.

Order and size (hypothesis, in lines): step 2 about 350, step 3 about 900 changed (600 new in the engine, 340 gone from the browser), step 4 about 300, step 5 about 120.

## Risks

- The spike misses the gate. The design then fails for a reason that this page cannot see: the generic paths may not be the real cost. Control: step 1 comes first, with a stop rule.
- The fast path reads a slot that a getter changed. Control: the shape check after each object child, and the differential test of step 4.
- A change of the cost model changes when a cap fires. Control: `state_bytes` of the `history` row, the cap tests, and one cost function in `bare-js-api` that both engines call.
- A fused copy changes when a heap limit fires relative to a getter. Control: the difference is only visible after the script has ended. The 65 cases do not cover it, so add one case with a getter after the limit.
- Visited objects not rooted in the new engine. Control: the stress mode that collects at every allowed safepoint (design 6.8) runs the 65 cases on js2.
- The walk is compiled once, in the engine, with `dyn CloneHost`. A host call is indirect. Cost: a few instructions for each host object. Not measured.
- Layout noise: the engine and the browser change size. Control: the standing codegen-unit rule.

## Not measured

- Every number of "Expected cost", the record sizes (the 48 bytes and the 1.1 KB), the allocation count of the old `State` (about 11) and the cost of `dyn` host calls.
- The cost of option E: the +2.76% and +3.00% come from the task brief. They were not rebuilt here.
- `jemalloc` and codegen-units=1 runs, the Windows VM and the new engine. This is a design page.
- Whether a `BroadcastChannel`, IndexedDB or form-state caller exists. The grep found none in the call path of the walk.
- The V8, WebKit and Ladybird record layouts, except what the headers show. The statements marked "from memory" must be checked against the sources before the build.

## Noticed and not changed

- `serialize` caps the depth of `history` at `min(max_state_depth, 128)`, and `structuredClone` uses 256. The two numbers differ by design in the code. Nothing documents why.
- A depth cap exists because the walk is recursive on the native stack. It protects the stack, not an algorithm, so it is not the kind of limit that `CLAUDE.md` warns about. An explicit stack would remove it. The behavior is frozen by the cap test (`clone-depth` at 300 levels), so this design keeps recursion.
- `Dom::is_host_object` runs 7 table probes for every object of every walk. The `is_platform_object` probes (8 more) run in only some paths. After the owner keys exist, both become one header test.
- `Dom::transfer_items` uses `list.contains` for the duplicate test. It is quadratic in the list length. A transfer list is short, so this is not a cost today.
- `materialize_with` makes two passes over the nodes ("views come second") only because a view's buffer can be a later node. The new format writes the buffer first, so the second pass goes.
- `clone_payload` (`browser_scheduler.rs:1008`) serializes and materializes a same-heap message at post time. The copy is then queued as live objects. This is the case that F3 covers.
- `docs/design-host-navigation.md` says structured clone has 37 distinct engine calls in one file. The count of the walk today is lower because some calls moved to `eval_clone.rs`. It does not change this design.

## Open questions for the owner

1. Record: bytes (F2, proposed), or the tree (F1)? Proposed: bytes. One allocation, no tree, and it matches V8, SpiderMonkey and WebKit.
2. Add the fused copy (F3) to this series? Proposed: yes, as step 5, after the gates of steps 3 and 4 pass.
3. Where does the format live? Proposed: in `bare-js-api` (`clone` module), shared by both engines, so the limits, the cost model and the tags have one copy. The alternative is one copy in each engine.
4. Put the three methods on `Engine`, or in a sub-trait? Proposed: on `Engine`, as option F in `design-host-navigation.md` said. Every engine must implement them anyway.
5. Keep a hook for a host object that is cloned as data (a `Blob`, a `File`, `ImageData`, a `DOMException`, geometry)? Proposed: reserve the `Host` tag in the format now, and add `write_host_object` and `read_host_object` to `CloneHost` (with default bodies) when the first such object is built. Today every host object is refused.
6. Keep the exact step charges and the byte cost model? Proposed: yes. The cap behavior is part of the tests, and the model also feeds the message queue caps. Do not change it in this series.
7. Keep recursion with the depth cap (256 and 128)? Proposed: yes. Revisit with an explicit stack when the new engine has one.
8. Old-engine header bit for the side-table classes (step 4) or keep the 16 probes? Proposed: decide by the profile after step 3. The probes are 0.7 K of 4.4 K in the estimate, so the bit is optional.
9. Should `Cloned` be `Send` for a future worker? Proposed: no. Pages share one thread today. A worker needs its own design, and `Box<dyn Any>` would then become `Box<dyn Any + Send>`.
10. The first PR after approval: the spike (step 1, a scratch tree, never merged) or the format module (step 2)? Proposed: the spike first. It costs one task and it can stop the series.
