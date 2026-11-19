# Form controls and input through the host interface

Status: decided by the owner, 2026-11-19 (plan step S0.4, forms). See "Decisions" at the end. Base: origin/main `37df5bd1`.

## Problem

Forms are the largest subsystem of S0.4 by lines, and the smallest by engine state. Eleven files (7,687 lines) use the engine: `browser_controls.rs`, `browser_select.rs`, `browser_form.rs`, `browser_submit.rs`, `browser_selection.rs`, `browser_textarea.rs`, `browser_focus.rs`, `browser_editing.rs`, `browser_ui.rs`, `browser_uievent.rs`, `browser_drive.rs`. Almost all of them are host functions of type `H` (`browser.rs:257`) that convert arguments and call native code. The control values, the selection, the validity flags and the focus are native data in `bare-html` and in `FocusState`. No engine value is kept for them.

Two questions remain. Which engine operations must the interface give so that these handlers run on `bare-js-api`? And how do the few engine objects that the forms keep (`ValidityState`, `labels`, the `select` collections, the shared prototypes and functions) leave `Host::roots` and `Host::prune`?

The brief also asks about input events. The trusted events of the window and the driver (`browser_ui.rs`: pointer, key, `InputEvent`, `change`, `submit`) go through the dispatch of `browser_events.rs`. Their engine references are `Traced` already (PR #697). Forms add no new reference there.

## Scope and assumptions

- Step 3 of `docs/design-lazy-properties.md` (host-filled objects become owners) is built by another agent now. I assume it gives: an `Owner` variant for an object, `set_host_owner` at creation, and `Host::trace(key)` and `Host::prune(dead)` that dispatch on the variant. I assume `browser_nodelist.rs` is in it, because `labels`, `form.elements` and the `select` lists are `NodeList` kind objects (`Source::NodeList`). I do not cover that file.
- The handler type `H` (`browser.rs:257`) and `Host::call` (`browser.rs:777`, today a stub that aborts) are not changed by a subsystem PR. I assume one later PR changes `H` to the interface types for all subsystems. This PR only adds what that PR needs and moves the references. Open question 1.
- `Host::lazy_member` (the lazy `on*` and prototype members, lazy-properties decision 2) exists before this PR. `proto_for` (`browser_controls.rs:746`, `browser_select.rs:556`) and 13 `lazy_global` sites depend on it.

## Current design

- `CtlHost` (`browser_controls.rs:339`): `St` holds `state: HashMap<NodeId, ObjId>` and `owner: HashMap<ObjId, NodeId>` (one `ValidityState` for each element), `labels: HashMap<NodeId, ObjId>`, the `setCustomValidity` texts and the forms in `reset()`. Four `OnceCell`s hold 7 + 2 + 2 + 2 shared objects (accessors, methods, `ValidityState` and `HTMLLabelElement` prototypes and constructors).
- `SelHost` (`browser_select.rs:84`): `Made` holds 6 prototypes and 7 constructors. `Cache` (`:68`) keeps `options: Option<ObjId>` and `chosen: Option<ObjId>` for each `select`, so `select.options === select.options`.
- Hooks in the old per-object hook (`browser.rs:898` to `:1001`): `roots` at `:913` (`browser_select`) and `:922` (`browser_controls`); `prune` at `:978` and `:981`. Forms have no `trace`. A `ValidityState` does not keep its element alive.
- `prune` is a `retain` with the `live` query over each map (`browser_controls.rs:360`, `browser_select.rs:1146`).
- The arena reclaim has its own hooks (`held`, `forget`: `browser_reclaim.rs:130`, `:183`, `:185`). They use `NodeId` only. This PR does not touch them.
- Patterns use the engine's regular expression: one site, `browser_controls.rs:406` (`bare_js::regexp::{Limits, Regex, Scratch}`). `regexp.rs` imports only `bare_unicode`, so it has no engine state.

## Uses of the engine

Counted over the 11 files (name occurrences of each imported item, outside `use` lines; calls of `it.*` by name).

| Item           | Uses | Item                               |  Uses |
| -------------- | ---: | ---------------------------------- | ----: |
| `value::Value` |  497 | `eval::Error`                      |    11 |
| `eval::Interp` |  310 | `value::to_uint32` / `to_int32`    | 7 / 2 |
| `value::ObjId` |   38 | `regexp::{Regex, Limits, Scratch}` |     3 |

| `it.*` call    | Sites | `it.*` call                                                      | Sites |
| -------------- | ----: | ---------------------------------------------------------------- | ----: |
| `type_error`   |    30 | `to_js_bool`                                                     |     6 |
| `charge`       |    27 | `store_value` / `traced_value`                                   | 6 / 3 |
| `to_js_number` |    13 | `host_proto_from_new_target`                                     |     3 |
| `define_data`  |    11 | `object_prototype`, `array_prototype`                            |     3 |
| `get_value`    |     9 | `global_object`, `array_from`, `call`, `to_js_string`, `inspect` |     6 |
| `host_scope`   |     9 | `heap.*` (`str_len`, `to_string_lossy`)                          |     2 |

Four shared helpers carry 44 more sites: `def_method` 18, `def_get` 16, `add_fn` 7, `define_accessor` 3. Changing the four changes the 44.

## Measurements

archMachine, origin/main `37df5bd1`, callgrind on a release build with symbols (`debug=1`, no strip), one run per row. Instruction counts do not depend on the load of the machine.

| Row                      |   Total | `Interp::collect` (incl.) | Old `Host::trace` (incl.) | `controls::prune` + `select::prune` (self) |
| ------------------------ | ------: | ------------------------: | ------------------------: | -----------------------------------------: |
| `form_load gc 1`         | 124.9 M |                     17.1% |                     2.04% |                                        610 |
| `form_load gates 1`      | 141.9 M |                     15.2% |                     1.83% |                                        610 |
| `form_load submit 1`     |  44.6 M |                     43.6% |                     6.36% |                                        610 |
| `select_load churn 1000` | 359.8 M |                     15.6% |                     1.51% |                                      5,520 |

Findings:

- The forms own no trace, and their two prune functions cost under 6,000 instructions in a row (under 0.002%). Moving them to owners gains nothing that these rows can show. The gain of S0.4 for forms is only that the old hook can go when the last subsystem leaves.
- The old hook is 1.5% to 6.4% of these rows. It is the sum of the probes of all other subsystems, not of forms.
- Cost of one owner: about 90 instructions for a key, an owner entry and a prune step (`select_load churn 1000` +0.49% for 20,021 wrappers, `docs/progress.md`, 2026-11-19). Hypothesis: a `ValidityState` or `labels` list is made at most once for each element that a script reads, so the cost stays under 0.1% on rows that read them. No row reads them (see Risk).
- `select_load churn 1000` spends 1.5% in `select_options`. That is the path that makes the `select.options` collection, which Option A would register. Hypothesis: one owner for each collection made adds under 0.2% on this row. Not measured.

## How other engines do it

From memory of each engine's source. To verify before the build.

| Engine        | The `ValidityState` and its element                                                                              |
| ------------- | ---------------------------------------------------------------------------------------------------------------- |
| Blink with V8 | The element keeps the `ValidityState` as a traced member, made at the first read. The element's trace visits it. |
| WebKit        | The element owns it (reference count). Its wrapper is alive while the element's tree is (opaque root).           |
| Gecko         | A field of the element, made lazily, with a wrapper cache. The cycle collector frees the pair.                   |
| Servo         | A lazy `MutNullableDom<ValidityState>` field of the element. Traced by the reflector.                            |
| Ladybird      | A GC cell. The element visits it in `visit_edges`.                                                               |

The common rule: the element owns the companion through a lazy field, and the element's trace visits it. No engine keeps a map from node to companion that the collector prunes.

## Options

### A. Companions are owners; singletons are persistent (recommended)

- Each `ValidityState`, `labels` list and `select` collection gets a host key at creation (`set_host_owner`). The owner variant holds `(kind, NodeId)`. `Host::prune(dead)` removes the entries of the dead keys from the three maps. The two `retain` loops go.
- The maps stay keyed by `ObjId` for the lookup of `this`. The cost of a read does not change.
- The 13 + 13 shared objects become `Persistent` handles (about 70 instructions each, once, when the first element of the kind gets a wrapper).
- Cost: one owner for each companion made. Memory: one `Option<Owner>` entry (about 16 bytes) for each.

### B. Weak handles

- The interface gets `Weak`: `Scope::weak(v) -> Weak`, `Scope::upgrade(&Weak) -> Option<Local>`. The maps hold `Weak`. A dead entry is dropped at the next lookup, plus a sweep at a size limit.
- No owner and no `prune`. The engine needs a weak table (the old engine has one for `WeakRef`; to verify that it can hold host entries cheaply).
- Cost: one weak table entry for each companion. A stale entry stays until a lookup or a sweep.

### C. The element's trace holds the companion (Blink, Servo, Ladybird)

- The wrapper owner of the element (PR #700) holds `Traced` fields for its companions. `trace(key of element)` visits them. The companion is alive while the wrapper is. No weak map, and no `prune` for the forward map.
- The reverse lookup (`this` to element) still needs a map by `ObjId` or a key in the object. With a key, `Scope::host_key(obj)` is a new method. The old engine has no cheap object-to-key lookup for these objects (the 104-byte header has no room; `docs/design-lazy-properties.md`, "Build of the first PR").
- Cost: a companion lives as long as the wrapper, not as long as a script holds it. Memory: up to one companion of each kind for each wrapped element.

### D. Leave forms on the old hook, last

- `Host::prune(live)` stays for the three maps until the end of S0. The two hook calls stay.
- Cost: nothing now. The old hook cannot be removed, and the new engine must keep a `live` query. This conflicts with the decision that owners replace per-object probes (`docs/design-host-tracing.md`, decision 1).

### Comparison

|     | Work per collection                                           | Work per companion                                 | Interface change                          | Size |
| --- | ------------------------------------------------------------- | -------------------------------------------------- | ----------------------------------------- | ---- |
| A   | one `prune` for dead keys                                     | about 90 instructions (hypothesis for these kinds) | none                                      | S    |
| B   | none                                                          | weak entry; a stale entry until swept              | `Weak` type, 2 methods, engine weak table | M    |
| C   | one visit in the element's trace                              | a `Traced` store                                   | `Scope::host_key` for the reverse lookup  | M    |
| D   | `retain` of 3 small maps (measured: under 6,000 instructions) | none                                               | none                                      | none |

A has no interface change and removes the `live` query. D is the cheapest now, but it keeps the hook. B and C are cleaner when the new engine exists. They do not pay back in S0, because forms have no measurable collection cost.

## New interface methods

Forms need no method that is specific to forms. They need the base set of the handlers, which every S0.4 subsystem needs. The first PR that needs a method adds it; the CSSOM and navigation designs list only what is more. The counts are the sites in the files of the three subsystems of S0.4 (forms, CSSOM, navigation).

| Method (on `Scope`, from `Engine`)     | Sketch                                                        | Semantics                                                                                     |     Sites |
| -------------------------------------- | ------------------------------------------------------------- | --------------------------------------------------------------------------------------------- | --------: |
| `type_error`, `range_error`            | `(&mut self, msg: &str) -> Error<'s>`                         | Makes the exception object. The caller returns it.                                            |        91 |
| `charge`                               | `(&mut self, n: u64) -> Result<(), Error<'s>>`                | Adds to the step budget. `Abort(Limit(Steps))` over the limit.                                |        96 |
| `to_number`, `to_string`               | `(&mut self, v: Local) -> Result<f64 / Local<'s>, Error<'s>>` | ToNumber and ToString. They can run script.                                                   |        23 |
| `to_boolean`                           | `(&self, v: Local) -> bool`                                   | ToBoolean. Runs no script, so `&self`.                                                        |         7 |
| `new_array`, `is_array`                | `(&mut self, &[Local]) -> Result<Local<'s>, Error<'s>>`       | An array from items; a type test.                                                             |         5 |
| `intrinsic`                            | `(&mut self, Intrinsic) -> Local<'s>`                         | `ObjectPrototype`, `ArrayPrototype`, `IteratorPrototype`, `ErrorPrototype`, `SymbolIterator`. |        13 |
| `get_prototype_of`, `set_prototype_of` | `(&mut self, obj, proto: Option<Local>)`                      | Prototype chain.                                                                              |         6 |
| `define_accessor`                      | `(obj, key, get: Option<Local>, set: Option<Local>, attrs)`   | The four helpers above need it for every getter and setter.                                   | 3 helpers |
| `proto_from_new_target`                | `(&mut self, default: Local) -> Result<Local<'s>, Error<'s>>` | The prototype for a constructor call (`is_new`).                                              |         6 |
| `string_utf16`                         | `(&mut self, &[u16]) -> Result<Local<'s>, Error<'s>>`         | A string from code units, with lone surrogates. `Scope::string` takes `&str`.                 |         6 |
| `wall_now_ms`                          | `(&self) -> f64`                                              | The clock of `Date.now()` (virtual in tests).                                                 |         2 |

Not in the list on purpose: `host_scope`, `host_checkpoint` and the promise calls belong to the event-loop design (9 `host_scope` sites here are driver entries such as `Browser::focus`, `browser_focus.rs:364`). `regexp` stays a direct use of a stateless module; it moves to the helper crate of S0.2 question 5, so no method is needed.

## Leaves the old hook

| Call                                               | Line at `37df5bd1`       | After               |
| -------------------------------------------------- | ------------------------ | ------------------- |
| `browser_select::roots`, `browser_controls::roots` | `browser.rs:913`, `:922` | persistent handles  |
| `browser_select::prune`, `browser_controls::prune` | `browser.rs:978`, `:981` | `Host::prune(dead)` |
| `trace`                                            | none                     | none                |

## Risk to the cost rule

Rows that exercise forms: `form_load` (submit, gc, gates), `select_load` (value, react, churn), `selection_load` and `focus_load`. `form_load gates` was +0.90% under glibc only in the events PR; it is close to the gate for noise. `select_load churn 1000` is at +0.49% since the wrapper PR, so any added cost per `select` object can break it. A registers one owner for each `select.options` and `selectedOptions` object, so it needs a callgrind A/B on that row first.

No example script reads `validity`, `checkValidity`, `labels`, `setCustomValidity` or `form.reset()` (grep of `crates/bare/examples`). `form_load gates` runs the native check through a click only. A regression in the JavaScript side of these paths is invisible to the matrix. The first PR must add a `validity` case to `form_load` and measure on the base.

## Plan

1. (Shared) the base set above in `crates/js-api/src/lib.rs`, with the adapter in `crates/js/src/eval_api.rs` and adapter tests. No browser change. Cost: none (hypothesis: no existing call path changes).
2. Add the `validity` and `labels` case to `form_load`. Record the base numbers.
3. Option A: owners for the three companions; persistent handles for the 26 shared objects; remove the 4 hook calls. Tests: a `ValidityState` of a dead element frees its key and its map entries; `el.validity === el.validity` while a script holds it; `select.options` after a collection; a key that is reused answers for the new object only.
4. Gates: Linux and the Windows VM; the cost matrix within +0.5% under both allocators; `select_load churn 1000` and the new case measured again with callgrind.

## Open questions for the owner

1. Does a subsystem PR move its handlers to `Scope`, `Local` and `Args`, or does one PR move all handlers after the last subsystem (proposed: one PR at the end, because every subsystem shares `H`)?
2. Option A (proposed), B, C or D for the companion objects?
3. Is the base set of handler methods a PR of its own (proposed), or part of the first subsystem PR?
4. May forms go last in S0.4 (proposed)? They gain nothing measurable, and they carry the unmeasured validity paths.
5. A new `form_load` case for `validity` and `labels`: add it before the move (proposed)?

## Decisions (owner, 2026-11-19)

The owner accepted every proposed answer.

1. One PR moves all handlers to `Scope`, `Local` and `Args` after the last subsystem.
2. Option A for the companion objects.
3. The base set of handler methods is a PR of its own.
4. Forms go last in S0.4.
5. Add a `form_load` case for `validity` and `labels` before the move.
