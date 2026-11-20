# Design: host handler bodies on `Scope` and `Local`

Status: design for owner review (plan step S0.4, the last large part). No code. Base: `origin/main` `1cfb3807`, 2026-11-20.

Owner decision of 2026-11-19: "The handler bodies move to `Scope` and `Local` in one later PR." This page checks that decision against counts that were measured on the current tree, and proposes a split if "one PR" does not hold. It also lists what `bare-js-api` lacks for these bodies.

Gate S0 (`plan-engine-rewrite.md`, section 5): every journey passes on the old engine through `bare-js-api`; the instruction change is under 0.5%; the stress mode passes. The handler bodies are what keeps that gate open. The browser also cannot run on `bare-js2` (stage S5) while a body names `Interp`, `Value` or `ObjId`.

## 1. Inventory

### 1.1 Method

All counts come from `rg` over `crates/bare/src` at the base commit. `SRC` is the non-test sources, `TST` the `*_tests.rs` files. A "site" is a matching line, counted once per line.

```sh
cd <repo>
SRC=$(git ls-files 'crates/bare/src/*.rs' | grep -v '_tests\.rs$')     # 111 files, 66,560 lines
TST=$(git ls-files 'crates/bare/src/*.rs' | grep '_tests\.rs$')        # 108 files, 33,402 lines
rg -c '\.engine\(\)'                 $SRC     # s.engine()
rg -c '&mut Interp'                  $SRC     # handlers and helpers
rg -c '^\s*(pub(\([a-z]+\))? )?fn .*&mut Interp' $SRC            # fn items that take the interpreter
rg -c '^\s*(pub(\([a-z]+\))? )?fn .*&mut Interp.*\b_?this: Value' $SRC   # of them, host handlers
rg -c '\bObjId\b'                    $SRC
rg -c '\bValue\b'                    $SRC
rg -c 'host_bind'                    $SRC
rg -c '\bapi_[a-z_]+\('              $SRC     # api_err, api_unerr
rg -c '\.heap\.|\bit\.heap\b'        $SRC     # direct heap access
rg -oN --no-filename '\b(it|self\.it|b\.it)\.([a-z_0-9]+)\(' -r '$2' $SRC | sort | uniq -c | sort -rn   # Interp methods used
rg -oN --no-filename '\.heap\.([a-z_0-9]+)\(' -r '$1' $SRC | sort | uniq -c | sort -rn                   # Heap methods used
rg -oN --no-filename 'bare_js::[a-z_]+::(\{[^}]*\}|[A-Za-z_]+)' $SRC | sort | uniq -c | sort -rn       # bare_js items
```

The per-file table and the group totals were made by a small script with the same patterns (one `rg -c` per column and file). Stale counts are possible after a rebase; run the commands again before a PR.

### 1.2 Totals

| What                                           |                                               Non-test sources |             Tests |
| ---------------------------------------------- | -------------------------------------------------------------: | ----------------: |
| Files (lines)                                  |                                                   111 (66,560) |      108 (33,402) |
| Files that name `bare_js::` (sites)            |                                                       94 (267) |          92 (250) |
| Files that name `bare_js_api` (sites)          |                                                        28 (96) |             3 (3) |
| `s.engine()`                                   | 6 in 2 files (`browser.rs` 4, `browser_navigation_state.rs` 2) |                 0 |
| `&mut Interp`                                  |                                              2,132 in 88 files |                 0 |
| `fn` items that take `&mut Interp`             |                                              2,099 in 88 files |                   |
| of them host handlers (`this: Value`)          |                                              1,044 in 73 files |                   |
| `Interp` (any mention)                         |                                              2,249 in 91 files |      8 in 5 files |
| `ObjId`                                        |                                                803 in 65 files |       1 in 1 file |
| `Value`                                        |                                              3,793 in 94 files |   160 in 59 files |
| `host_bind`                                    |                6 calls on 3 lines, all in `browser_streams.rs` |                 0 |
| `api_err`, `api_unerr`                         |                                             7 calls in 2 files |                 0 |
| `it.heap` (direct heap access)                 |                                                153 in 41 files |    35 in 13 files |
| `define_data`                                  |                                                274 in 55 files |                 2 |
| `get_value` / `put_value`                      |                                119 in 52 files / 21 in 8 files |             0 / 4 |
| `type_error`                                   |                                                436 in 73 files |                 3 |
| `R<` (the `Result<_, eval::Error>` alias)      |                                              1,298 in 90 files |                   |
| Lines with `Interp`, `Value`, `ObjId` or `it.` |                                   6,851 (10.3% of the sources) | 423 (with `.it.`) |
| `Persistent` / `Traced` already in use         |                                29 in 12 files / 49 in 11 files |                 0 |

Reading the table:

- `s.engine()` is not the problem. 6 sites remain, all in the interface hooks of the earlier PRs. The problem is the old `Host` trait: `Dom` implements it, the numbered handlers run through its `call(it, id, this: Value, args: &[Value])`, and the interface `Host::call` of `Dom` returns `Unsupported("browser host functions through the interface")` (`browser.rs:837`).
- About 1,044 of the 2,099 `Interp` functions are handlers (`type H = fn(&Dom, &mut Interp<'_>, Value, &[Value]) -> R<Value>`, `browser.rs:257`). The other 1,055 are helpers that take `it` and forward it. The helpers move with the handlers: a handler cannot become `Scope`-based while its helpers take `&mut Interp`.
- 120 `add_fn` and `add_ctor_nt` calls in 47 files register handlers. They end in one `host_function` call in `browser.rs`.

### 1.3 By subsystem

Columns: files, lines, `fn` items that take `&mut Interp` (host handlers among them are about half), `ObjId` and `Value` sites. The last column is a hypothesis: touched lines, taken as the share of `fn` items of the total 6,851 lines. Each touched line shows twice in a diff (removed and added).

| Group                                                                                                                                                                                                                                | Files |  Lines | `fn` with `Interp` | `ObjId` | `Value` | Touched lines (hypothesis) |
| ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ | ----: | -----: | -----------------: | ------: | ------: | -------------------------: |
| A1 DOM core: `browser.rs`, dom, element(s), attr, nodelist, dataset, html*, reflect, nodecmp, chardata, fragment, namespace, slot, shadow, table, details, implementation, lazy, sources, svg, gaps, shims, basics, cookie, handlers |    27 | 12,283 |                465 |     190 |     820 |                      1,520 |
| A2 DOM algorithms: range, walker, selection, mutation, intersection, resize, platform*                                                                                                                                               |     9 |  5,821 |                255 |     100 |     480 |                        830 |
| B events, input, event loop, console: events, uievent, mouse_legacy, error_event, focus, ui, editing, textarea, scroll, motion, frame, idle, scheduler, rejection, perf*, console                                                    |    17 |  9,481 |                352 |      78 |     660 |                      1,150 |
| C forms: form, controls, select, submit, face                                                                                                                                                                                        |     5 |  3,577 |                177 |      42 |     226 |                        580 |
| D1 CSSOM, style, geometry: cssom, computed, style*, css_ns, geom, matrix, point, geometry, rects, used, fonts, fontface                                                                                                              |    13 |  8,219 |                241 |     105 |     376 |                        790 |
| D2 media, image, canvas                                                                                                                                                                                                              |     3 |  1,869 |                115 |      31 |     195 |                        375 |
| E fetch, XHR, streams, abort, web, encoding, worker                                                                                                                                                                                  |     7 |  7,256 |                288 |     160 |     626 |                        940 |
| F navigation, frames, storage, clone, child pages                                                                                                                                                                                    |     9 |  4,912 |                131 |      76 |     265 |                        430 |
| G loader, modules, custom elements, drive, session                                                                                                                                                                                   |    10 |  7,069 |                 75 |      21 |     142 |                        245 |
| No engine use (`main`, `page`, `control`, `downloads*`, `resources`, `images`, `agent`, `force`, `lib`, `test_server`)                                                                                                               |    11 |  6,073 |                  0 |       0 |       3 |                          0 |
| Total                                                                                                                                                                                                                                |   111 | 66,560 |              2,099 |     803 |   3,793 |                      6,850 |

The largest files: `browser_dom.rs` 126 functions, `browser_fetch.rs` 96, `browser_media.rs` 85, `browser_range.rs` 82, `browser_select.rs` 66, `browser_cssom.rs` 63, `browser_web.rs` 61.

### 1.4 What the bodies call on the engine

The Interp methods called as `it.<name>(` in the sources (2,011 calls, 95 names), with the interface method that answers each one:

| Call (sites)                                                                                                                                                                   | Interface today                                                      |
| ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ | -------------------------------------------------------------------- |
| `type_error` 436, `range_error` 4, `charge` 284                                                                                                                                | `Scope::type_error`, `range_error`, `charge`                         |
| `define_data` 274, `get_value` 118, `put_value`, `get_str` 1                                                                                                                   | `define`, `get`, `set`, `get_str` (take a `Local` key; see gap G3)   |
| `object_prototype` 66, `array_prototype` 8, `iterator_prototype` 3, `error_prototype` 2, `symbol_iterator` 5, `global_object` 48                                               | `intrinsic`, `global`                                                |
| `to_js_number` 65, `to_js_bool` 43, `to_js_string` 16                                                                                                                          | `to_number`, `to_boolean`, `to_string`                               |
| `array_from` 37, `is_callable_value` 33, `call` 31                                                                                                                             | `new_array`, `is_callable`, `call`                                   |
| `host_proto_from_new_target` 28                                                                                                                                                | `proto_from_new_target`                                              |
| `host_new_promise` 23, `host_resolve_promise` 10, `host_reject_promise` 20, `host_promise_then` 2, `host_promise_react` 1, `host_checkpoint` 23, `host_iter_open/next/fail` 29 | The M1 methods (`new_promise` and the rest)                          |
| `persist_value` 26, `persistent_value` 18, `trace_value` 24, `traced_value` 13, `store_value` 24, `set_host_owner_obj` 14                                                      | `persist`, `local`, `trace_ref`, `traced`, `store`, `set_host_owner` |
| `host_bind` 6                                                                                                                                                                  | `bind`                                                               |
| `host_scope` 51 (13 files)                                                                                                                                                     | none: gap G5                                                         |
| `heap_error` 109                                                                                                                                                               | none needed: it only converts the `HeapError` of `it.heap`           |
| `new_array_buffer` 4, `new_typed_array` 3, `new_uint8_array` 2, `bytes_of` 6                                                                                                   | none: gap G7                                                         |
| `display` 7, `inspect` 1                                                                                                                                                       | none: gap G12                                                        |
| `eval_global` 4, `run_script` 2, `eval_function` 1, `compile_function` 1, `call_scoped`, `is_scoped_function`                                                                  | none: gap G6                                                         |
| `wall_now_ms` 9                                                                                                                                                                | `wall_now_ms`                                                        |

The `Heap` methods called as `.heap.<name>(` (153 sites in 41 files): `set_prototype_of` 16, `to_string_lossy` 14, `peek_own_property` 13, `str_len` 12, `define_own_property` 11, `str_units` 10, `intern_str` 10, `get_prototype_of` 8, `new_object` 5, `bytes` 5, then names with 1 to 4 sites each. The files with most: `browser.rs` 13, `browser_console.rs` 11, `browser_web.rs` 9, `browser_handlers.rs` 9, `browser_basics.rs` 7.

### 1.5 `bare_js` items that `crates/bare` still names, against the S0.1 inventory

S0.1 (2026-11-18) counted files, tests included. Today, sources and tests together:

| Item                        | S0.1 files | Now: sources, tests |
| --------------------------- | ---------: | ------------------- |
| any `bare_js`               |        219 | 95, 92              |
| `Value`                     |        151 | 94, 59              |
| `ObjId`                     |         64 | 65, 1               |
| `Interp` (`eval::Interp`)   |         87 | 91, 5               |
| `PropertyKey`               |         16 | 18, 0               |
| `StrId`                     |          4 | 3, 0                |
| `PropertyDescriptor`        |         11 | 14, 41              |
| `HeapLimits` / `EvalLimits` |    82 / 95 | 3, 87 / 4, 90       |
| `it.heap.` sites            |        221 | 153, 35             |

By module (`bare_js::<module>` in the sources): `value` 110 paths in 89 files, `eval` 108 in 94, `heap` 40 in 30, `parse_ahead` 3 in 1, `parse_cache` 1, `regexp` 1, and the math functions `sin`, `cos`, `atan2`, `acos` (1 each).

The count of files fell where the earlier PRs worked (tracing, owners, promises, modules). It did not fall for `Value`, `ObjId` and `Interp`: the handler bodies still hold all three. The test count of `HeapLimits` and `EvalLimits` is the harness (`Browser::new` in each test), not handler code.

### 1.6 State that still holds raw values

The old hook has not left `Dom`. The handlers write into these tables, so each group of handlers moves its tables with it:

- 27 `roots` functions (26 lines in `Dom::roots`), 15 `trace` functions on `ObjId` and 14 `prune` functions on `live: &dyn Fn(ObjId)`, each in its own file.
- 150 struct fields of type `ObjId`, `Value`, or a `Vec`, `Option` or `Cell` of them, in 31 files.
- 50 maps keyed by `ObjId` in 26 files (the reverse lookup from `this` to the host state). `Dom::node_of` is one hash lookup (`by_obj`) at the start of almost every DOM handler (`browser.rs:1405`).
- 40 `Traced<Value>` in 8 files; 29 `Persistent` uses in 12 files.

### 1.7 Callers outside the handlers

- `Browser` has a public field `it: Interp<'static>` (`browser.rs:1944`). `crates/drive` reaches it in 5 places (`b.it.display`, `get_str`, `promise_status`: `run.rs` 4, `locate.rs` 1) and names `bare_js` in 10 lines (`run.rs` 9, `locate.rs` 1). `crates/mcp` uses the engine only through `bare`.
- Tests: 88 files, 322 sites of `.it.` (for example `b.it.get_str(v, "name")` in 56 files). 423 test lines name `Interp`, `Value`, `ObjId` or `.it.`.
- Examples: 35 of 39 files in `crates/bare/examples` name `bare_js` (147 lines). They are the cost-matrix binaries.
- Session setup (`browser_session.rs`) builds the engine with `HeapLimits` and `EvalLimits`, sets the clock, and reads `BARE_JS_EXEC`. This is not handler code, but a generic browser needs it behind a trait (see question 2).

## 2. What `bare-js-api` lacks for these bodies

Existing methods cover the first five rows of 1.4 and the promise, persistent and trace rows. The gaps below are in the order of how many sites they block. "Smallest addition" names the least that unblocks the sites. The first PR that needs a method adds it, as in S0.4 so far.

| Id  | Gap                                                                                                                                                                                                    | Sites                                                                           | Smallest addition                                                                                                                                                                                                                                                                                                                                                                                                                    | Size    |
| --- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ | ------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ | ------- |
| G1  | `Engine::host_function` makes a non-constructor with no `new.target`. `add_fn` passes a `ctor` flag, and `add_ctor_nt` needs `new.target`.                                                             | 120 registrations; `host_construct` 2, `host_new_target` 1                      | One enum `HostFn { Method, Ctor, CtorNewTarget }` on `host_function`, and `Scope::construct(f, args, new_target)`                                                                                                                                                                                                                                                                                                                    | S       |
| G2  | Reverse lookup from `this` to host state. Today `by_obj.get(&ObjId)`.                                                                                                                                  | 50 maps, one lookup per handler call                                            | `Scope::host_key(Local) -> Option<u32>` (the old `Heap::host_key` exists, `heap_binary.rs:468`). Key-indexed tables then replace the maps.                                                                                                                                                                                                                                                                                           | S       |
| G3  | A property key is a `Local` string. Bodies today call `key_from_str` or use cached `PropertyKey`s. `Scope::string` pushes a string and `api_key` converts it each call.                                | 274 `define_data`, 118 `get_value`, 314 `d.key(it, name)`                       | Provided methods `define_str`, `set_str`, `define_accessor_str` on `Scope` (no `Engine` change). Then decide, from the spike of section 4, if a hot key needs an interned handle.                                                                                                                                                                                                                                                    | S       |
| G4  | The result and error types. `R<T>` is `Result<T, eval::Error>`; `Error::Throw(Value)` 46 sites in 20 files; `Unsupported(...)` 91 sites in 30 files; `LimitKind` 24 in 9 files.                        | 1,298                                                                           | None in the API: `Error<'s>` and `Abort::{Limit, Unsupported}` exist. The browser gets `type Res<'s, T> = Result<T, Error<'s>>`. Browser limit errors (`d.lim("dom-string")`) become `Abort::Limit(&'static str)`.                                                                                                                                                                                                                   | S       |
| G5  | `host_scope`: a driver entry that starts the step budget, counts one call level, and refuses collect, `run_script` and `drain_microtasks`. `enter` does none of these (`eval_host.rs:172`).            | 51 sites in 13 files (event dispatch, focus, rects, scheduler tasks)            | `Engine::task<R>(f)` and `Scope::task`. The old engine calls `host_scope`.                                                                                                                                                                                                                                                                                                                                                           | S       |
| G6  | Script entry: `run_script`, `compile_function` (event handler attributes), `call_scoped` (a handler with a scope chain), `eval_global`, `eval_function`.                                               | 14 sites in 9 files                                                             | `run_script(src) -> Local`, `compile_function(name, params, body) -> Local`, `call_scoped(f, this, args, scope_chain)`                                                                                                                                                                                                                                                                                                               | M       |
| G7  | Binary data: `new_array_buffer`, `new_uint8_array`, `new_typed_array`, `bytes_of`, and the `Heap` view calls (`view_parts`, `view_type`, `view_load`, `bytes_mut`, `buffer_detached`, `bigint_limbs`). | 27 sites in 10 files                                                            | 4 constructors; `bytes(Local) -> Option<&[u8]>` and `bytes_mut` tied to `&Scope`; `view(Local) -> Option<View>`; `is_detached`                                                                                                                                                                                                                                                                                                       | M       |
| G8  | Create host-filled objects and mark lazy ones. The interface has the `Host::indexed_*`, `named_*` and `lazy_member` hooks, but no way to make the object or to mark it.                                | 16 sites in 9 files (`browser_sources.rs:78` makes every list, map and dataset) | `new_host_indexed(proto, key)`, `new_host_named(proto, key, open)`, `set_lazy(obj, chain: bool)`, `clear_lazy(obj)`                                                                                                                                                                                                                                                                                                                  | M       |
| G9  | Own-property reads: `peek_own_property` (16 sites in 9 files) and `define_own_property` (11 sites in 11 files).                                                                                        | 27                                                                              | `has_own` covers a presence test (at least 8 of the 16 sites are `.is_none()` or `.is_some()` on one line); two sites read the kind (`browser_console.rs:508` a data string, `browser_implementation.rs:83` an accessor): add `own_kind(obj, key) -> Option<Own<'s>>`. `define` and `define_accessor` cover `define_own_property` (to verify: every descriptor of the 11 sites is a value with three flags, or a getter and setter). | S       |
| G10 | Call arguments. The adapter `call` copies the arguments into a `Vec<Value>` for each call (`eval_api.rs`, `fn call`).                                                                                  | 31 `it.call` sites, plus every listener dispatch                                | No API change: the adapter uses a small stack array. Listed because it is on the hot path (section 3).                                                                                                                                                                                                                                                                                                                               | S       |
| G11 | Symbols: `symbol_to_string_tag` and `symbol_description`.                                                                                                                                              | 3                                                                               | `Intrinsic::SymbolToStringTag`; a `symbol_description` method only if the 1 site stays                                                                                                                                                                                                                                                                                                                                               | S       |
| G12 | Console and drive text: `display` (7 sites), `inspect` (1 site, with a node-name callback on `ObjId`).                                                                                                 | 8                                                                               | `Engine::display(Local) -> String`; `inspect(Local, cap, &dyn Fn(Local) -> Option<String>) -> String`                                                                                                                                                                                                                                                                                                                                | S       |
| G13 | Smaller calls: `set_integrity` 2, `array_length` 1, `key_to_value` 2, `shared_text` 1, `new_error(proto, msg)` 3.                                                                                      | 9                                                                               | `freeze`, `array_len`, `error(proto, msg)`; the rest by `string`                                                                                                                                                                                                                                                                                                                                                                     | S       |
| G14 | Engine construction, limits, clock and the collection between tasks (`gc_due`, `collect`, `stats`, `set_wall_clock`, `set_default_time_zone`).                                                         | session code, 4 files                                                           | Not for this step. A trait `EngineFactory` comes with the generic flip (question 2).                                                                                                                                                                                                                                                                                                                                                 | M later |

Not a gap: `host_bind` (`Scope::bind` exists), the promise and iterator calls, persistent and traced handles, structured clone, modules. `parse_ahead::Ahead`, `parse_cache::Kind` and `regexp` stay outside the interface (S0.1 question 5 and 6; 1 file each).

Total of the additions: about 14 methods and two types (`HostFn`, `Own`), with their adapter bodies and tests. Estimate (hypothesis): 600 to 900 lines in `crates/js-api`, `crates/js/src/eval_api.rs` and their tests. No browser change.

## 3. How a handler call works, and the cost risk

Today (old engine): the engine finds the old host first, calls `Dom::call(it, id, this, args)`, which reads `handlers[id]`, sets `style.cur`, checks `needs_new`, runs the custom-element hook (`ce.on`) around `h(...)`, and runs the mutation-observer hook after it (`browser.rs:1005`). `this` and the arguments are raw `Value`s on the Rust stack.

Through the interface: the engine pushes `this` and the arguments as consecutive handle slots and calls `Host::call(s, id, this, args, is_new)`. The body reads each argument as a `Local`. The result is a `Local` that the engine reads before it drops the slots.

Measured costs in the progress file, and what is not measured:

| Fact                                                                                    | Source                              | Value                                                                                                           |
| --------------------------------------------------------------------------------------- | ----------------------------------- | --------------------------------------------------------------------------------------------------------------- |
| A `Persistent` costs about 70 instructions to create and drop (75 in a loop of 1 M)     | S0.4 timers entry, 2026-11-18       | `scheduler_load timers 10` +0.77% (about 128 instructions per timer, with the call record)                      |
| One check of the interface host before the old host, on the host-call path              | S0.3 entry, 2026-11-18              | 5 host-call rows over +0.5%, `select_load react 1000` +0.91%. Fixed with a cold function after the old check.   |
| The interface hook for each microtask job                                               | M3 entry, 2026-11-19                | about +28 instructions per job; `scheduler_load mixed` +0.30% and +0.36%, `streams_load flow` +0.25% and +0.27% |
| A `Local` push                                                                          | `design-host-microtasks.md`         | 10 to 20 instructions: hypothesis. Not measured in a browser row.                                               |
| The adapter `call` allocates a `Vec` per call                                           | `eval_api.rs`                       | not measured                                                                                                    |
| `Scope::string` plus `to_property_key` for each property access, against `key_from_str` | `eval_api.rs` (`string`, `api_key`) | not measured. `Scope::string` calls `Heap::shared_text` (to verify: whether it interns).                        |

Two things follow. First, the cost of a handler body on `Local` is the cost of every push, key conversion and copy on a path that runs once for each host call. The matrix has rows where 1% is a single check, so a fixed overhead of 20 to 40 instructions for each host call can pass or fail the 0.5% rule. Second, the old engine cannot collect inside a task, so no test shows a wrong `Local` use. The compiler shows a `Local` that outlives its scope. A raw `Value` kept in a Rust variable compiles and works on the old engine, and breaks only on `bare-js2`. So the gate for "done" has to be a count, not a test (section 4, gates).

## 4. Options

### Option A: one PR (the 2026-11-19 decision)

Change every handler, helper, table and hook in one PR.

- Size: about 6,850 touched lines (about 13,700 in the diff, hypothesis), 88 files, 2,099 functions, 150 fields, 27 `roots`, 15 `trace`, 14 `prune` functions. Plus about 700 lines in tests and examples, plus the 600 to 900 lines of interface additions (section 2).
- Risk: high. Review is not possible in one pass. 14 PRs touched `crates/bare/src` between 2026-11-18 and 2026-11-20 (48 commits, 20 of them in `browser.rs`). The branch conflicts on each of them; the base moves while it is built. A fault shows as a journey failing in one of 9 subsystems with no way to bisect by PR.
- Cost-matrix risk: high, and not attributable. Every host call and every property access changes at once. If a row is over +0.5%, the cause can be a push, a key conversion, the `Vec` in `call`, or a table change, in any of 111 files. A callgrind diff of the whole browser has no baseline per subsystem.
- Verification: `cargo test -p bare --lib` (about 1,650 tests), the 805-row matrix once, the journeys. One red result blocks everything.
- Is it realistic? No. The 2026-11-19 decision was made before the counts existed; the design pages for fetch and microtasks guessed "about 150 functions in these four files". The count for the whole crate is 2,099 functions.

### Option B: per-group PRs on a dual registry (recommended)

Both `Host` traits stay on `Dom` during the move. Each handler is registered in exactly one of them. A converted handler goes through `host_function` of the interface (with `HostFn`, G1) and the interface `Host::call`. An unconverted handler stays on the old `Host::call` and costs what it costs today. The engine tells them apart by a bit in the host function record (to verify: a bit in the id is enough, and it costs no new branch beyond the one that checks the old host today).

Steps:

0. PR H0, no body moves: the interface additions of section 2 (G1 to G13), the dual registry, the `Browser` facade (`get_str`, `display`, `eval`; question 5), and one dispatch function that both entries use (`style.cur`, `needs_new`, the custom-element hook, the mutation-observer hook). It also carries the cost spike below.
1. PR A1: the helper layer and the DOM core. Helpers get a `Scope` form first. The old signature becomes a wrapper (`enter` the scope, push the values, call the new helper, read the result), used only by handlers that have not moved. This is the one place with temporary duplication.
2. PRs B, C, D1, D2, E, F, G, A2: one group each, in this order of hotness. After A1, these groups are file-disjoint except for helper signatures, so up to 5 can run in parallel (the standing rule of up to 5 agents).
3. PR Z, cleanup: delete the `eval::Host` impl of `Dom`, the old `H` alias, the wrappers of step 1, the 27 `roots`, 15 `trace` and 14 `prune` functions, make `Browser::it` private, move the tests and examples to the facade, add the grep gate.

Sizes (touched lines, hypothesis, from 1.3): A1 1,520; B 1,150; E 940; A2 830; D1 790; C 580; F 430; D2 375; G 245. H0 600 to 900. Z about 500 plus tests and examples about 700. The largest is A1 at 465 functions in 27 files. If the owner finds that too large, split A1 by file: `browser.rs` and `browser_dom.rs` (the helpers and 126 handlers) first, the rest after.

Risk: medium. Each PR is reviewable (a group of 75 to 465 functions). Conflicts are limited to the files of the group. A journey failure points at one group.

Cost-matrix risk: low to medium and attributable. Each PR moves only the handlers of one group, so the rows of that group change (rows by subsystem: `attr_load`, `dataset_load`, `nodelist_load`, `handler_load`, `range_load`, `select_load`, `form_load`, `focus_load`, `cssom_load`, `fetch_load`, `streams_load`, `xhr_load`, `navigation_load`, `scheduler_load`, `loader_load`, and the rest). The step-1 wrappers add cost to unconverted handlers while they exist. Order A1 first so that the hot helpers lose their wrappers early. Risk of a transitional cost: a handler that calls a helper of the other form pays the wrapper (about a push and a copy for each call, hypothesis 20 to 40 instructions). Every PR is gated against its parent, so a transitional cost cannot add up unseen.

The spike (in H0, before any group): write three hot handlers both ways on a branch (`get_attribute`, `has_attribute`, `matches_selector` of `browser_dom.rs`, 1453 to 1591), and measure `attr_load`, `select_load react`, `handler_load` and `nodelist_load` with `tools/js/instr.sh`, 3 runs, base against branch. It answers: what a host call costs for each of `this` push, argument push, key conversion, and `node_of` by `host_key` against `by_obj`. Decision rule: if the converted handlers are within +0.2% on those rows, go on with the split as written. If not, change the call path first (args read in place, an interned key handle for hot names, G3) and measure again. Do not ask for an exception for a bridge cost.

Verification for each PR:

- Tests: the filters of the group (the 108 test files map to groups by name: `browser_dom*`, `browser_attr*`, `browser_range*`, `browser_events*`, `browser_fetch*`, and so on), then `cargo test --locked -p bare-js-api -p bare-js -p bare --lib` once at the end. New tests: a handler of the group runs with `BARE_GC_STRESS=1` (the stress mode of S0.x) and keeps its state across a collection.
- Journeys: `tools/suite/run.py` (40 generated tests, V0 and V2, plain and under stress); the staging journey A/B; the Windows VM filters.
- Cost: `tools/js/cost_matrix.sh` against a base built fresh from the commit the PR rebases on; no row over +0.5% under both allocators; the codegen-units rule for rows that fail.
- Gate for "converted" (count, since tests cannot show it; run in the PR and in Z). In the files of the group: `rg -c '\bbare_js::(value|eval::Interp)|\bObjId\b|\bInterp\b|\.engine\(\)|\.heap\.'` is 0. In Z the same count is 0 for all of `crates/bare/src` except `browser_session.rs`.

### Option C: a trampoline first, then the move

One PR makes `Dom` implement only the interface `Host::call`. The trampoline reads each `Local` with `Engine::raw` (so it uses `s.engine()` once), calls the existing `H` body with `Interp` and `Value`, and pushes the result with `raw_local`. All 1,044 bodies stay as they are. Then the groups move one by one as in B.

- Size: small for the first PR (about 300 lines, hypothesis). The later PRs are the same as B.
- Risk: low for the first PR. It reaches the literal wording of gate S0: every journey passes through `bare-js-api`.
- Cost-matrix risk: high for the first PR. Every host call pays a push of `this` and each argument, a copy to a slice, and a push of the result, on all 1,044 handlers at once. The S0.3 entry shows that one extra check on this path is +0.5% to +0.91% on call-dense rows. This is the cost that the dual registry of B avoids.
- It does not remove `Interp`, `Value` or `ObjId` from the bodies, so it does not unblock `bare-js2` by itself. It leaves the 6,850 lines to a later PR, which is option B again, and it adds a cost to every row in the meantime. Only useful if the owner wants gate S0 closed early at a measured cost; the spike of option B gives that cost.

### Option D (rejected): a browser-side `Val` that looks like `Value`

A `Copy` type in the browser, with the shape of `Value`, would turn the move into a rename. It would hold a raw engine reference. That is the thing design 6.8 removes: with a collection inside a task (S1.6) a raw reference that the host keeps across an allocation is a bug. It would move the same work to S5.

### Comparison

|                              | Size of the largest PR                        | Reviewable | Conflicts with other PRs   | Cost rows attributable     | Unblocks `bare-js2`                                       |
| ---------------------------- | --------------------------------------------- | ---------- | -------------------------- | -------------------------- | --------------------------------------------------------- |
| A, one PR                    | about 13,700 diff lines                       | no         | every PR of the next weeks | no                         | yes, at the end                                           |
| B, groups on a dual registry | A1, about 3,000 diff lines (or split by file) | yes        | files of the group only    | yes, by group              | yes, group by group; the slice (S2.8) needs A1 and B only |
| C, trampoline first          | first PR 300 lines                            | yes        | few                        | first PR touches every row | no, until B                                               |

## 5. Recommendation

Option B, with H0 first and the cost spike inside H0. "One PR" is not realistic: 2,099 functions in 88 files, 6,851 touched lines (10.3% of the crate), 150 raw fields and 3 old hooks in 27 files, while main merged 14 PRs into these files in 2 days. Proposed split for the owner to approve: H0, A1, then B, C, D1, D2, E, F, G, A2 (up to 5 at once after A1), then Z. That is 11 PRs. Each one passes the journeys and the matrix on its own.

Why not C: its first PR charges every host call before any body has gained from the move. Why not A: no PR can be reviewed or bisected.

One change in the target design would reduce cost, not add it. With `Scope::host_key` (G2), `this` resolves to its host state by an index into the owner table instead of a hash lookup in `by_obj`. 50 maps keyed by `ObjId` can become key-indexed tables in the group that owns them. The earlier navigation PR expected a gain from owners and measured about 0.05% (hypothesis for the cause in that entry), so the spike must measure this; it is not promised.

## 6. Open questions

1. Cost gate while the move runs. Proposed: every PR is within +0.5% of its parent (the standing rules apply), and each PR reports the sum against the base before H0. No exception for a bridge cost; if the spike shows a fixed cost per call, change the call path first. Decision needed: is a cumulative bound also wanted (proposed: report it, do not gate on it)?
2. Generic over `Engine` now, or after. Proposed: bodies are written against one alias (`Eng`, today `Interp<'static>`; `Scope<'s, Eng>`), and the flip to `Dom<E: Engine>` is its own mechanical PR with the first `bare-js2` `Engine` implementation (S2.8). Reason: 1,044 handlers get a generic parameter and `Browser` changes type, and a generic with one instance proves nothing that the grep gate does not. Cost of waiting: the compiler does not prove that bodies avoid `Value` (the grep gate does). Decision needed: accept, or generic from the start (the choice of 2026-11-18 reads as generic; this asks when).
3. The reverse lookup. Proposed: `Scope::host_key(Local)` and key-indexed tables, group by group; keep `by_obj` until its group moves. Alternative: require `Hash + Eq` on `Engine::Raw` and keep the maps.
4. Property keys. Proposed: provided `*_str` methods first (G3); add an interned key handle only if the spike shows the key conversion is a measurable share. Decision needed after the spike.
5. `Browser::it` is public. Proposed: H0 adds facade methods (`eval`, `get_str`, `display`, `call`, `with_scope`). Tests (322 `.it.` sites in 88 files), examples (35 files) and `drive` (5 sites) move in Z. Examples are the matrix binaries: each facade method is `#[inline]`, and the matrix compares examples of both trees, so the move of an example changes its measured program. Proposed: move the examples in a separate PR before Z, with a matrix run that compares the example built from the old and the new text on the same tree.
6. The hooks around every call (`style.cur`, `needs_new`, custom-element reactions, mutation-observer delivery). Proposed: one `dispatch` function in H0 used by both entries, so a handler behaves the same in either registry. Test: the existing `browser_custom_tests` and `browser_mutation_tests` run once with every handler on each registry (the registry is a flag in the test build).
7. Parse-ahead (`Ahead`, `parse_cache::Kind`) and `regexp`. Proposed: stay outside the interface (1 file each, session code), as in S0.1 question 6.
8. Order inside B. Proposed: A1, then B (events and the loop; the typing page path), so that S2.8 can start with the two groups it needs. Decision needed if the owner prefers the smaller groups first (C, F) to learn the cost on a small surface.
9. The 6 `host_bind` calls (`browser_streams.rs`, 3 lines): they move with group E using `Scope::bind`. No new method.
10. Who runs the groups. Proposed: the main session reviews and merges as before; groups C, D1, D2, E, F, G run in parallel after A1 (up to 5 agents). A1 and H0 run alone.

## 7. Not measured

- No instruction counts: this is a design. The costs in section 3 are the earlier measurements; the `Local` push, the `Vec` in `call`, key conversion and the wrapper cost are hypotheses.
- Touched lines per group are a share of `fn` items, not a count of the final diff.
- The split of `peek_own_property` sites into presence tests and kind reads is partial (8 of 16 seen on one line).
- Whether `Scope::string` interns, and whether a bit in the host function id adds no branch on the old call path.
- Counts cover `crates/bare/src`. `crates/drive` (2 files) and `crates/bare/examples` are listed, not counted by group.
