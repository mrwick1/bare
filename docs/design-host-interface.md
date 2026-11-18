# Design: the host interface between the browser and the JavaScript engine

Status: inventory (plan step S0.1 of `plan-engine-rewrite.md`). The owner reviews this list before S0.2 builds the interface crate `bare-js-api`.

Goal of stage S0: the browser runs on today's engine (`bare-js`) or on the new engine (`bare-js2`) through one interface. This page lists what the browser uses of `bare-js` today, grouped by kind. It does not design the interface yet; the last section lists the questions that the design must answer.

## Method

- Source: `origin/main` at 2026-11-18. Read with a script over `crates/bare`, `crates/drive` and `crates/mcp`.
- Items: every `bare_js::` path, in `use` lines (groups expanded) and inline.
- Methods: each public method of `Interp`, `Heap` and the `Host` trait that appears as `.name(` in the three crates.
- Limit: methods are matched by name. A name that other types also have (`new`, `get`, `set`, `call`, `collect`, `display`, `run`, `stats`, `to_string`) gives a count that is too high. Those counts are not given below.

## Where the engine is used

| Crate                |                  Files that name `bare_js` | Of them tests |
| -------------------- | -----------------------------------------: | ------------: |
| `bare` (the browser) |                                        219 |            91 |
| `drive`              |                                          2 |             0 |
| `mcp`                | 0 (it uses the engine only through `bare`) |             0 |

`drive` uses 5 items: `Value`, `Interp::display`, `parse::parse_script` (a syntax check of a step), `PromiseStatus` and `Error::Throw`. Everything else is in `bare`.

## The items, by kind

Counts are the number of files that name the item.

### 1. Values

| Item                                                                                  |   Files | Use                                                           |
| ------------------------------------------------------------------------------------- | ------: | ------------------------------------------------------------- |
| `value::Value` (and its variants `Object`, `String`, `Bool`)                          |     151 | Every argument and result                                     |
| `value::ObjId`                                                                        |      64 | An object reference that the host keeps in its own structures |
| `value::PropertyKey` (and `Symbol`)                                                   |      16 | Property names                                                |
| `value::StrId`                                                                        |       4 | An interned string                                            |
| `value::to_uint32`, `to_int32`, `number_to_string`, `string_to_number`, `is_js_space` | 1 to 11 | Conversions with no engine state                              |
| `atan2`, `cos`, `sin`, `tan`, `acos`                                                  |  1 to 2 | Math for CSS and canvas, with no engine state                 |

`Value` is `Copy` and holds a raw `ObjId`. The host keeps `ObjId` and `Value` in its own structures across calls (event listeners, wrappers, timers) and reports them through `Host::roots` and `Host::trace`.

### 2. The interpreter and the realm

| Item                                                                                                                                               |    Files | Use                                                        |
| -------------------------------------------------------------------------------------------------------------------------------------------------- | -------: | ---------------------------------------------------------- |
| `eval::Interp`                                                                                                                                     |       87 | One realm: its heap, its globals and its job queue         |
| `eval::*` (glob import)                                                                                                                            |       30 | Mostly `Interp`, `Error`, `EvalLimits`                     |
| `Interp::run_script`, `eval_global`, `eval_function`, `compile_function`                                                                           | 4 to 132 | Run classic scripts and handlers                           |
| `Interp::global_object`, `object_prototype`, `array_prototype`, `error_prototype`, `iterator_prototype`, `symbol_iterator`, `symbol_to_string_tag` |  2 to 40 | Built-in objects that the host links to                    |
| `Interp::realm`, `set_host`                                                                                                                        |   1 to 2 | Make a realm and attach the host                           |
| `parse::parse_script`, `parse_cache::Kind::ClassicLazy`, `parse_ahead::{Ahead, MIN_AHEAD, taken_here}`                                             |   1 to 2 | Parse ahead of time on a worker; a syntax check in `drive` |

### 3. Objects and properties

The host reaches into the heap directly: `Interp` has a public field `heap`, and `it.heap.` appears 221 times. The most used calls:

| `Heap` method         | Sites | `Heap` method       |       Sites |
| --------------------- | ----: | ------------------- | ----------: |
| `peek_own_property`   |    18 | `get_prototype_of`  |           8 |
| `set_prototype_of`    |    16 | `new_indexed`       |           7 |
| `intern_str`          |    15 | `key_from_str`      |           7 |
| `to_string_lossy`     |    14 | `intern_units`      |           7 |
| `str_len`             |    13 | `bytes`             |           7 |
| `str_units`           |    12 | `new_object`        |           6 |
| `define_own_property` |    11 | others (27 methods) | 1 to 4 each |

On `Interp`: `define_data` (58 files), `get_str` (64), `get_value` (53), `put_value`, `array_from` (18), `array_from_proto`, `to_js_number` (39), `to_js_bool` (20), `to_js_string` (10), `is_callable_value` (21), `is_constructor_value`, `inspect` (6).

Types: `heap::PropertyDescriptor` (11 files), `heap::Property` (`Data`, `Accessor`), `heap::Slot` (7), `heap::Elem` (4), `heap::Get`.

63 of 170 public `Heap` methods and 103 of 123 public `Interp` methods are used.

### 4. Host functions and calls

| Item                                                                                                                                                                      | Use                                                                        |
| ------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------- |
| `eval::Host` trait, method `call`                                                                                                                                         | The host implements every DOM method as one `call(id, this, args, is_new)` |
| `Interp::host_function`, `host_function_interned`, `host_ctor_with_new_target`, `host_construct`, `host_new_target`, `host_proto_from_new_target` (18 files), `host_bind` | Make host functions and constructors                                       |
| `Interp::call`, `call_scoped`, `host_scope` (13 files), `is_scoped_function`                                                                                              | Call script functions from the host                                        |
| `Interp::type_error` (73 files), `range_error`, `new_error`, `heap_error` (41)                                                                                            | Make exceptions                                                            |
| `eval::Error` (119 files), `Error::Throw`, `Error::Unsupported`, `eval::LimitKind` (27)                                                                                   | Results and failures                                                       |

### 5. Wrappers: lazy and virtual objects

The DOM makes most wrapper properties on demand. The engine asks the host when a script reads a key that is not there yet.

| Item                                                                                                             | Use                                                                   |
| ---------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------- |
| `heap::NamedSource`, `NamedValue`, `NamedOp`, `NamedRequest`, `set_named_source`, `new_named`, `new_named_open`  | Named properties that the host fills (`dataset`, `style`, storage)    |
| `heap::IndexedSource`, `set_indexed_source`, `new_indexed`, `virtual_len`, `Host::fill_indexed`                  | Indexed properties that the host fills (`NodeList`, `HTMLCollection`) |
| `heap::LAZY_TAG`, `LAZY_ALL`, `set_lazy_on`, `clear_lazy_on`, `lazy_on_key`, `set_lazy_chain`, `lazy_chain_name` | Prototype members made at first use                                   |
| `Host::named_write`                                                                                              | A script writes a named property of a host object                     |
| `heap::HeapError::NeedsHost`                                                                                     | The engine asks the host to fill a key                                |
| `Interp::private_slots`, `add_external`, `external_live`, `set_external_live`                                    | Host data tied to an object                                           |

### 6. Promises, microtasks and the event loop

| Item                                                                                                                                                                            | Use                                              |
| ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------ |
| `Interp::host_new_promise` (7 files), `host_resolve_promise`, `host_reject_promise`, `host_promise_then`, `host_promise_react`, `promise_status`, `eval::PromiseStatus`         | Promises that the host settles (`fetch`, timers) |
| `Interp::queue_microtask`, `drain_microtasks`, `pending_jobs`, `host_checkpoint` (10 files), `clear_failed_job`                                                                 | The microtask checkpoint of the HTML event loop  |
| `Host::between_jobs`, `Host::promise_rejections`, `Host::report_exception`, `unhandled_rejections`, `eval::MAX_PENDING_REJECTIONS`, `eval::PromiseLimits`, `set_promise_limits` | Errors and rejection events                      |
| `Interp::host_iter_open`, `host_iter_next`, `host_iter_fail`, `eval::HostIter`                                                                                                  | Host code that walks a script iterable           |

### 7. Modules

`Interp::module_parse`, `module_url`, `module_requests`, `module_set_requested`, `module_link`, `module_evaluate`, `take_dynamic_imports`, `finish_dynamic_import`, `fail_dynamic_import`, `reject_dynamic_import`. The host fetches each module; the engine parses, links and runs it.

### 8. Binary data and structured clone

`Interp::new_array_buffer`, `new_typed_array`, `new_uint8_array`, `bytes_of`, and the `Heap` methods `bytes`, `bytes_mut`, `buffer_len`, `buffer_detached`, `buffer_transfer`, `new_buffer`, `new_view`, `view_type`, `view_parts`, `view_load`, `view_detached`, `bigint_parts`, `bigint_limbs`, `bigint_from_parts`. Structured clone: `clone_class`, `eval::CloneClass`, `clone_new_coll`, `clone_coll_put`, `clone_coll_entries`, `clone_new_wrapper`, `clone_new_view`, `clone_new_regexp`, `clone_new_error`, `clone_new_date`.

### 9. The collector contract

| Item                                                                                              | Use                                                                                                        |
| ------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------- |
| `Host::roots`, `Host::trace`, `Host::prune` (the names appear in 26, 22 and 19 files; `roots` also matches `Interp::roots`)             | The host reports the values it keeps, the values an object holds for it, and drops entries of dead objects |
| `Interp::roots`, `collect`, `gc_due`, `weak_tracked`, `date_tracked`                              | Run the collector between tasks                                                                            |
| `heap::Collected`, `heap::HeapLimits` (122 files), `heap::HeapError` (11), `heap::Limit::Objects` | Limits and results                                                                                         |

Today the collector runs only between tasks (`gc_due` at depth 0), so a `Value` that host code holds in a local variable during a call is never collected. The hybrid collector (design question 11) also collects inside a task. Then every `Value` that host code holds across an allocation must be a root.

### 10. Limits, clocks and statistics

`eval::EvalLimits` (95 files), `eval::ScLimits`, `eval::ScLimit::Dates`, `set_sc_limits`, `regexp::{Regex, Limits, Scratch}`, `Interp::charge` (65 files), `eval::WallClock`, `set_wall_clock`, `wall_clock`, `wall_now_ms`, `eval::set_host_wall_clock`, `set_default_time_zone`, `stats`, `agg_stats`, `async_stats`, `ic_counts`, `bc_scope_check`, `bc_gen_check`, `heap::Ic`.

## What this means for S0.2

These are questions for the design of `bare-js-api`, not decisions.

1. Direct heap access. 221 sites call `it.heap` directly, through 63 `Heap` methods. The interface needs a smaller set of object operations. A census of these sites by operation decides the set (get, define, prototype, strings, buffers).
2. Raw references held by the host. `ObjId` and `Value` sit in host structures and are reported through `roots` and `trace`. With collection inside a task, host locals must also be roots. Design 6.8 proposes handle scopes and persistent handles. The 64 files that use `ObjId` show where persistent handles are needed.
3. Lazy and virtual objects (kind 5). Both engines must support "ask the host for this key". This is a large part of the DOM's speed today, so the interface keeps it as an engine feature, not a host workaround.
4. One `Host::call` with an integer id for every DOM method. This keeps the interface small. The new engine can keep it.
5. Items with no engine state (math, number conversion, `is_js_space`) can move to a small shared crate, so the browser does not depend on either engine for them.
6. Statistics and checks (`ic_counts`, `bc_scope_check`, `bc_gen_check`, `heap::Ic`) are engine-specific. They stay outside the interface, behind a feature or a debug trait.

Not done: a count by operation of the 221 direct heap sites, the call graph from DOM subsystems to these items, and the cost of each item on the daily journeys.
