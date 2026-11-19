# Modules through the host interface

Status: decided by the owner, 2026-11-19 (plan step S0.4, modules). See "Decisions" at the end.

## Problem

The browser loads, links and evaluates ES modules through 10 methods of the old `Interp` (`docs/design-host-interface.md`, section 7). One file uses them: `crates/bare/src/browser_loader/modules.rs` (671 lines). The interface `bare-js-api` has none of them. Four references in that file are raw engine values that the host holds across engine calls. With a collection inside a task (S1.6) three of them are bugs. This page lists the methods to add, the references to move, and the cost risk. There is one finding that affects the order of the PRs: no cost row exercises modules.

## Current design

Division of work (`eval_modules.rs`, header comment): the host fetches, resolves and caches; the engine parses, links and evaluates.

1. `module_parse(source, url) -> u32` (`eval_modules.rs:112`) parses a module and keeps its record. A syntax error is a thrown `SyntaxError`. It refuses to run inside a call (`depth != 0`).
2. `module_requests(m) -> Vec<String>` (`:128`) gives the specifiers. The host resolves them against the importing URL and the import map, fetches the files (two or more in parallel), and parses them (recursion, `load_rec` and `load_deps`, depth cap 128).
3. `module_set_requested(m, deps)` (`:133`) gives the module of each request.
4. `module_link(root)` (`:151`) checks every import and makes the environments of the whole graph. A bad import is a thrown `SyntaxError`.
5. `module_evaluate(m) -> ObjId` (`:183`) runs the graph and gives the promise of the evaluation. A second call gives the same promise (`cap`).
6. `import()` in a script or module: the engine queues an `ImportRequest { promise, specifier, referrer }`. The host takes the queue with `take_dynamic_imports` (`:248`), loads the graph and calls `finish_dynamic_import(promise, m)` (`:253`), `reject_dynamic_import(promise, Throw(v))` (`:271`) or `fail_dynamic_import(promise, message)` (`:279`, a `TypeError`).

The engine keeps the module records (`ModState::mods`), the namespace objects, the `import.meta` objects, the evaluation promises and the errors. `module_values` (`eval_modules.rs:288`) roots them for the collector. A module lives as long as its realm (the module map of the document, as in HTML). The host holds module ids (`u32`), not objects.

What the host holds (`Modules`, `modules.rs:43`):

| Field / type                  | Holds                                                                         | Rooted by                                                      |
| ----------------------------- | ----------------------------------------------------------------------------- | -------------------------------------------------------------- |
| `ids: HashMap<String, u32>`   | module ids, no engine reference                                               | not needed                                                     |
| `failed`, `map`, `nodes`      | strings, the import map, node ids, no engine reference                        | not needed                                                     |
| `evals: Vec<(ObjId, String)>` | the evaluation promise of each module graph that is not seen settled yet      | the engine (`ModRec::cap`), by accident                        |
| `Load::Throw(Value)` (`:70`)  | the `SyntaxError` of a parse, until `script_threw` or `reject_dynamic_import` | nothing                                                        |
| `ImportRequest::promise`      | the promise of an `import()`, from `take_dynamic_imports` until it is settled | nothing (the engine dropped it from `dyn_imports` at the take) |
| `promise_status` result       | the rejection reason, until `script_threw`                                    | nothing                                                        |

Timing and hooks in the loader that matter here:

- `settle_evals` (`:578`) runs at each microtask checkpoint from `after_js` (`browser_loader.rs:1457`). It reads `promise_status` for each promise in `evals`. A rejected one is reported (`script_threw`). A pending one stays.
- `serve_imports` (`:604`) takes the `import()` queue, starts the downloads of all requests together, then loads each graph. The loop blocks the page thread in the network layer and parses. The promises of the requests that wait are raw `ObjId` during that time.
- `load_rec` returns `Load::Throw(v)` up through `load_deps` and `load_graph`. On the way it removes entries from `ids`. It calls no engine method that allocates. So the raw `Value` is not at risk between the parse and its report, if both run in one scope.

Uses in `modules.rs` (the `bare_js::` items and `it.` calls, non-test):

| Item                                                                                               |                                       Count |
| -------------------------------------------------------------------------------------------------- | ------------------------------------------: |
| `it.module_parse`                                                                                  |                                           3 |
| `it.module_requests`, `module_set_requested`, `module_url`, `module_link`, `module_evaluate`       |                                      1 each |
| `it.take_dynamic_imports`, `finish_dynamic_import`, `reject_dynamic_import`, `fail_dynamic_import` |                                      1 each |
| `it.promise_status`, `it.type_error`                                                               |                                      1 each |
| `bare_js::eval::{Error, PromiseStatus}`, `bare_js::value::{ObjId, Value}`                          | 1 `use` each; `Value` 5 mentions, `ObjId` 2 |

No other file of `crates/bare`, `drive` or `mcp` calls a `module_*` method. `module_namespace` and `module_is_linked` have no browser site. They stay out of the interface.

## Measurements

Not measured by a cost row, because there is none. `tools/js/cost_rows.txt` has no row with `type=module` or `import()`. `loader_load dynamic` adds classic scripts from a `load` handler. `sheets_load imports` is CSS `@import`. The module code runs only in unit tests (`eval_modules_tests.rs`, `browser_loader_tests.rs`, 51 mentions of `module`).

What the existing rows show, archMachine, `origin/main` 37df5bd1, callgrind, one run:

| Row                     |  Total | Module code                                                                                  |
| ----------------------- | -----: | -------------------------------------------------------------------------------------------- |
| `loader_load dynamic 1` | 18.6 M | `take_dynamic_imports` 210 instructions; `after_js` in `modules.rs` 840 instructions (both under 0.01%) |

`module_values` has no symbol of its own in that profile (inlined; hypothesis: its cost with no module is a few tens of instructions per collection). The row has no module script, so these numbers show only the idle cost of the module hooks.

Hypothesis (not measured): a module costs 100 K instructions or more to parse, link and run, and the loader makes about 10 engine calls for each module. A wrapper of 10 to 20 instructions for each call is under 0.2% of a module. A `Persistent` for the evaluation promise (about 70 instructions for create and drop, #695) is under 0.1%.

Because no row covers modules, the cost rule cannot check this subsystem. A row must come first, on the base that the later PRs compare with.

## How other engines do it

From memory of each engine's source. To verify before the build.

| Engine                     | Who walks the graph                                                                                                                                                                                 | Evaluation result                                       |
| -------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------- |
| V8 with Blink              | The engine, with a host callback: `Module::InstantiateModule(context, ResolveModuleCallback)` asks the host for each import. Blink fetches the graph first (`ModuleTreeLinker`), then instantiates. | `Module::Evaluate` returns a promise.                   |
| SpiderMonkey with Gecko    | `JS::ModuleLink`, with a host `ModuleResolveHook` and `ModuleLoadHook`. Gecko loads the graph first (`ModuleLoader`), then links.                                                                   | `JS::ModuleEvaluate` gives a promise (top-level await). |
| JavaScriptCore with WebKit | Host loader (`ModuleLoader` JS object) fetches and calls `link`, `evaluate`.                                                                                                                        | Promise from `evaluate`.                                |
| Ladybird                   | LibWeb `fetch_module_script_graph`: the host loads the graph, then `Module::link`, `evaluate`.                                                                                                      | A promise.                                              |

In all four the host loads the whole graph before it links (V8 and SpiderMonkey also ask the host to map a specifier to a loaded module during link). The loader is host code, because it is I/O and policy (import maps, MIME checks, budgets). Bare already works this way.

## Options

### A. Keep the push model; module ids stay numbers; three raw references become `Persistent` (recommended)

- `bare-js-api` gets the 10 methods, with `Local` and `Persistent` in place of `Value` and `ObjId`. The interface sketch is below.
- `module_evaluate` gives a `Local`. The loader keeps a `Persistent` of it in `evals` until it sees the promise settled. `ImportRequest::promise` is a `Persistent` that `take_dynamic_imports` makes (the host's root replaces the engine's). `Load::Throw` holds a `Persistent`, made where `module_parse` throws. `settle_evals` reads `promise_status` of the `Persistent`.
- No `Traced` and no owner: nothing in the module state is a reference that an owner holds. Module records stay engine state.
- Cost: one `Persistent` for each module graph evaluated, each `import()` and each syntax error. All three are rare compared with the module work.
- Memory: 8 bytes and an id for each `Persistent`, released when the promise is seen settled.

### B. The engine pulls: a host callback resolves each import during link

- The host implements `Host::resolve_module(referrer, specifier) -> ModuleId`. The engine calls it from `module_link` and from a dynamic import.
- The loader's recursion moves into that callback. The callback does blocking network I/O and a parse inside `module_link`. `module_parse` refuses to run inside a call (`eval_modules.rs:112`), so the engine's depth rule must change. The parallel start of the downloads (`load_deps`) needs the whole request list before the first resolve, so the host calls `module_requests` anyway.
- Gain: it matches the V8 and SpiderMonkey signatures. Against: a rewrite of the loader (about 250 lines), a new reentrancy rule for both engines, and no measured gain.
- Rejected for S0.4. The push model can be wrapped later if the new engine wants the callback form.

### C. Module records as engine objects: the host holds a `Persistent` for each module

- `module_parse` gives a `Persistent` module handle, and every `module_*` method takes a `Local`. `ids` holds handles.
- Gain: a module that no map holds could be collected. There is none: the module map of a document holds every module until the realm ends, and the environments are rooted ("never freed", `eval_modules.rs` header). So there is no gain.
- Cost: 70 instructions for each module (create and drop) and a `Local` push for each call.
- Rejected.

### Comparison

|                               | A. Push, ids, 3 `Persistent`        | B. Pull callback                  | C. Module handles              |
| ----------------------------- | ----------------------------------- | --------------------------------- | ------------------------------ |
| Per module graph              | 1 `Persistent`                      | 0, but a callback for each import | 1 `Persistent` for each module |
| Collection                    | no host code                        | no host code                      | no host code                   |
| Interface methods             | 10                                  | 4 and a hook                      | 10, with handles               |
| Loader change                 | types only                          | rewrite (about 250 lines)         | types and map                  |
| Engine change (old)           | wrappers over `module_*`            | depth rule, a call from link      | handle table                   |
| New engine (S3 modules group) | ids are indices of the module table | needs the callback                | needs module cells             |
| Size                          | S                                   | L                                 | M                              |

## Interface (option A)

On `Engine`; `Scope` gets the same names as thin wrappers (S0.2 pattern). `ModuleId` is `u32`, as the browser keeps it in `ids` today.

```rust
pub struct ImportRequest { pub promise: Persistent, pub specifier: String, pub referrer: Option<u32> }

/// Parse and keep a module. A syntax error is `Throw`. Refused inside a call.
fn module_parse<'s>(&mut self, source: &str, url: &str) -> Result<u32, Error<'s>>;
/// The specifiers of module `m`, in order; the host answers with `module_set_requested`, same order.
fn module_requests(&self, m: u32) -> Vec<String>;
fn module_set_requested(&mut self, m: u32, deps: &[u32]);
fn module_url(&self, m: u32) -> &str;
/// Check imports and make the environments of the graph of `root`. A bad import is `Throw`; nothing changes then.
fn module_link<'s>(&mut self, root: u32) -> Result<(), Error<'s>>;
/// Evaluate a linked graph. The promise of the evaluation; the same promise on a second call.
fn module_evaluate<'s>(&mut self, m: u32) -> Result<Local<'s>, Error<'s>>;
/// The `import()` calls since the last call, oldest first. The engine gives up its root; each promise is the host's `Persistent`.
fn take_dynamic_imports(&mut self) -> Vec<ImportRequest>;
fn finish_dynamic_import<'s>(&mut self, p: &Persistent, m: u32) -> Result<(), Error<'s>>;   // link, evaluate, settle with the namespace
fn reject_dynamic_import<'s>(&mut self, p: &Persistent, reason: Local<'_>) -> Result<(), Error<'s>>;
fn fail_dynamic_import<'s>(&mut self, p: &Persistent, message: &str) -> Result<(), Error<'s>>;  // a TypeError
```

Rules:

- `module_url` borrows engine memory. It is tied to `&self`, so the borrow cannot cross a call that may collect (S0.2 rule). The loader copies the URL at once (`import_base`).
- `module_parse` and `module_link` can allocate and so may collect. A module id is a number and needs no root.
- The engine keeps module records alive for the life of the realm. This is part of the contract, as it is in HTML. A host does not release a module.
- `promise_status` (microtasks page, PR M1) reads the promise of an evaluation.
- `Host` gets no new method. The loader runs in `between_jobs` and `after_js`, outside engine calls.

## Old-engine calls that leave the old hook

None. Modules use no `roots`, `trace` or `prune` line of `browser.rs`. Module records are engine state (`module_values`), and the one host-held `ObjId` (`evals`) was never in `Host::roots`. The first row of the table above is the reason: it is a hazard that exists only once the engine collects inside a task.

## Risk to the cost rule

- No row covers the subsystem (see "Measurements"). A change here can pass the matrix by not being run. The first PR adds the row.
- The shared paths that modules touch are small: `module_values` in `collect` (not separated in the profile, see "Measurements") and the dispatch of `host_checkpoint` in `after_js`. They do not change in this move.
- `finish_module` and `serve_imports` run on the page thread and block on the network. Wall time of a module page is not an instruction count. The row measures instructions on a loopback server, as `loader_load` does.
- A `Persistent` for each `import()` request adds about 70 instructions on a path that parses and runs a module (at least 100 K, hypothesis).

## Plan

1. MD0, the row. Add `loader_load modules SCALE` (and its two lines per scale in `tools/js/cost_rows.txt`): `SCALE*20` module scripts with a graph of depth 3 and shared dependencies, a top-level `await`, an import map entry, and `SCALE*10` `import()` calls. It checks sums as the other `loader_load` cases do. Merge it, then measure the base on it. This is code (an example and a rows file), not a design document. Gate: the row passes its own checksums; run-to-run spread under 0.25% (the rule for a row).
2. MD1, the interface: the 10 methods and `ImportRequest` in `bare-js-api`, the adapter in `eval_api.rs`, and tests (parse, link, evaluate, a cycle, a top-level `await`, an `import()` that is taken, finished, rejected and failed, a `Persistent` promise that survives a collection between take and finish). No browser change. Depends on M1 for `promise_status`. Gate: the matrix (no change expected).
3. MD2, the loader: `evals` holds `Persistent`, `Load::Throw` holds a `Persistent`, `ImportRequest::promise` is a `Persistent`. The loader calls the old `Interp` methods until the browser is generic (open question 1 of `design-host-microtasks.md`). Gate: the matrix and the new row, measured again. Tests: a collection between the take and the finish of an `import()` (needs the stress mode S0.x to be a real test); `settle_evals` after a collection reports a rejected evaluation.
4. Windows and Linux: the loader tests of modules pass on both (`browser_loader_tests.rs`).

Order: MD0, then MD1 (after M1), then MD2.

## Open questions for the owner

1. Option A (proposed), B or C?
2. Add the cost row first (proposed: MD0 as its own small PR), or in the same PR as MD1?
3. `u32` for module ids (proposed: the browser stores `u32`), or a `ModuleId` type?
4. The evaluation promise: a `Persistent` in `evals` (proposed), or a read-only method `module_promise(m)` so the host keeps only the id? The second needs one more method and no `Persistent`. It makes `module_evaluate` a pure call.

## Decisions (owner, 2026-11-19)

The owner accepted every proposed answer.

1. Option A: keep the push model and the `u32` module ids; `evals`, `Load::Throw` and `ImportRequest::promise` become `Persistent`.
2. Add the module cost row first, as its own small PR (MD0).
3. Module ids stay `u32`.
4. The evaluation promise is a `Persistent` in `evals`.
