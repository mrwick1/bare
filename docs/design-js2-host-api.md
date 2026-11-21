# Design: `bare-js2` behind `bare-js-api` (plan step S2.8, first part)

Status: design for owner review. No engine code changed. Date: 2026-11-20. Base: `origin/main` `ab2010b8`. It also reads PR #785 (`crates/js2/src/reenter.rs`, not merged), because that PR changes how a native calls script.

Words used in this file:

- "hypothesis" marks a number or a cause that no run proved.
- "native" is a `NativeFn` of js2: `fn(&mut Realm, this: Value, args: &[Value]) -> R<Value>` (`builtins.rs`).
- "window" is the `this` and the arguments of a running host call, as `bare-js-api` names them (`Args`, `Host::call`).
- "hold" is `Heap::hold_task` (#785): no collection runs until the outermost nested call returns.

## 1. Question

The first `bare-js2` implementation of `bare_js_api::Engine` must map handle scopes, persistent handles and the host-call window onto the roots of js2. Design 6.8 (`design-engine-rewrite.md`) gives the rules: safepoints, scoped roots, persistent roots, re-entrancy. `bare-js-api` gives the types. Neither says how a handle scope becomes a root of the js2 collector, which type implements `Engine`, or what happens when a host function calls script and the collector runs. The decision of 2026-11-20 (`design-host-handlers.md`, "Decisions") adds a cost rule: `Host::call` gets the window as handles that name the engine's own argument slots, with no copy and no push, and one scope for each call.

This page decides those points and lists what the adapter supports and lacks. It is a new design decision, so the code waits for review.

## 2. Facts about js2 (checked in the tree)

1. A native gets `&mut Realm` and the window as a slice (`builtins.rs`, `NativeFn`). It cannot reach a `Vm`. The stack, the feedback vectors and the program belong to `Vm::run` (`interp.rs`), or, after #785, to the machine that a call took from `Realm::pool`.
2. A native never collects. `native_call!` runs `gate!` before the call (`interp.rs`), and `Realm::collect` runs only from `gate!` and from `Vm::collect_idle`. So no host function collects, unless it calls script.
3. Natives get the arguments of a call as `&stack[ab..ab + argc]`: the registers of the caller. The root map of a call names the callee, the argument registers and, for `Call`, the `this` register. Checked on `function f(x){ var y = {}; var z = g(x, y, 3); return z }`: the `CallU` at the pc of the call has `b = 3`, `c = 3` and the map `[0, 1, 3, 4, 5, 6]`. So a collection that walks the caller frame keeps the window alive.
4. A window is not always in a frame. `Realm::call_native` also runs a native with `tail_args` (a `Vec`) and `call_function` (#785) runs one with a slice that a native holds in a Rust local. A map does not name those values.
5. `Realm::collect(fbs, walk_frames)` builds its roots from the realm fields (`consts`, `lex`, `pins`, the feedback vectors) and one closure that walks the frames. A new root is one more line there.
6. After #785 the realm can call script from any code that has `&mut Realm`: `Realm::call_function`. A nested call runs on a pooled machine and holds the collector. `Realm::collect` asserts `nest == 0`. A budget stop in a nested call is `Err(STOP)` (`Value::HOLE`); `Vm::start` turns it into `Exit::Fuel`.
7. A flat string is `OneByte` (Latin-1) or `TwoByte`, so it maps to `bare_js_api::Str::{Latin1, Utf16}` with no copy. A string built by `+` can be a `Rope`. `Str::flatten` needs `&mut Heap` and turns the rope into `Indirect`, which later reads follow (`string.rs`).
8. js2 has no barrier (no young generation yet), so `Engine::store_raw` keeps its default body.
9. js2 has no `Promise`, `Symbol`, `Map`, `Set`, `JSON`, `Date`, `RegExp`, generators, async functions, typed arrays or modules, and it has no `eval` (`interp.rs`, "Not here"). The function table of a `Program` is fixed when the `Vm` is made; #777 (compile at the first call) is the work that changes this.
10. The unsafe modules of js2 are a list in `lib.rs` (`cell`, `heap`, `string`, `atoms`, `shapes`, `gc`, `object`, `interp_rt`, `interp`). CI runs them under Miri.

Baseline of the call paths (machine archMachine, release, minimum of 3 runs of `instructions:u`, 200,000 iterations, the loop is inside a function, the cost is the difference to the loop `x = i`):

| Engine                                    | Call                                                                             | Instructions per call |
| ----------------------------------------- | -------------------------------------------------------------------------------- | --------------------: |
| js2 (`origin/main`)                       | native `isNaN`, callee in a register                                             |                   348 |
| js2                                       | script function `function f(a){return a}`, callee in a register                  |                   340 |
| `bare-js` (`Interp::realm`, `run_script`) | host function through `Engine::host_function`, one argument, returns `undefined` |                   588 |
| `bare-js`                                 | native `isNaN`                                                                   |                   683 |

The loop alone costs 128 instructions per iteration on js2 and 1,238 on `bare-js`. The runs repeat to 0.001%. The `bare-js` lane is the default of `Interp::run_script`; I did not check which one it is. The js2 call path is the one that #775 makes cheaper, so the 348 will move.

## 3. Design

### 3.1 The engine is `Realm`

`impl bare_js_api::Engine for Realm`, with `type Raw = Value`. The host keeps a `Realm` and calls `enter(&mut realm, ...)`. The api state is one field of the realm: `Realm::api: ApiState` (new file `crates/js2/src/api.rs`).

Why: a host function is a native, and a native has only `&mut Realm` (fact 1). `Host::call` needs `&mut Scope<E>`, so `E` must be reachable from `&mut Realm`.

### 3.2 Handles and roots

`ApiState` has the same parts as the old engine (`crates/js/src/eval_api.rs`), with the same slot numbers:

| Handle                                       | Where the value is                                                                                                                                     | Why it is a root                                                                                                                                                                                              |
| -------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `Local` of the handle stack                  | `ApiState::stack: Vec<Value>`; the slot is the index                                                                                                   | `Realm::collect` marks `stack[SEEDS..]`. `Scope::drop` calls `stack_truncate`, so the slots above the base stop being roots at once.                                                                          |
| `undefined`, `null`, `false`, `true`         | four seed slots at the start of the stack, with the slot numbers `u32::MAX - 3..=u32::MAX`                                                             | They hold no cell. A push of one of them costs nothing.                                                                                                                                                       |
| `Local` of the window (`this`, argument `i`) | the caller's registers, or the slice of a native (facts 3 and 4); the slot number is `WIN` (`1 << 31`) plus 0 for `this`, plus `1 + i` for an argument | A `Win` record (`this`, pointer to the slice, length, link to the previous record) lives in the frame of the trampoline. `ApiState::win` points at the innermost one. `Realm::collect` marks the whole chain. |
| `Persistent`                                 | `ApiState::persistent: Vec<Value>`, with the live bits in `bare_js_api::Slots`                                                                         | `Realm::collect` marks each live slot. `Drop` clears the bit and needs no engine.                                                                                                                             |
| `Traced`                                     | in the host                                                                                                                                            | Not a root. It needs the owner trace; see 3.9.                                                                                                                                                                |

The roots join `Realm::collect` as one more statement in its closure: `api.stack`, the live `persistent` slots and the `Win` chain. The frames are still walked by the caller of `collect`. The `Win` chain is needed besides the root maps of fact 3, because of fact 4.

A `Local` is a number, so a `Vec` growth does not move it. A slot that names nothing is a bug of the host; the engine panics and never reads outside a slice (the rule of the old engine).

### 3.3 The host call (the trampoline)

`Engine::host_function(name, id, length, kind)` makes a native function cell. All host functions share three `Native` entries, one for each `HostFn` kind, made when the first one is needed. The cell keeps the host `id` in slot 1 and the kind in slot 2 (slots 1 to 3 are free for the function, `builtins.rs`). It gets `name` and `length` as a built-in does. So the `natives` table does not grow with the number of host functions, and the browser makes thousands.

The trampoline is one `NativeFn`, `host_call(realm, this, args)`:

1. Read the cell from `realm.callee`; read `id` and the kind from slots 1 and 2.
2. For `Method`: `new.target` must be `undefined`. For `Ctor`: `realm.nt` must be the function itself, else a `TypeError` (the rule of `HostFn`). `is_new` is `realm.nt != undefined`.
3. Flatten each rope in the window (see 3.6).
4. Link a `Win` record; a guard unlinks it also when the host panics.
5. `enter(realm, |s| host.call(s, id, Local::from_slot(WIN), Args::from_slots(WIN + 1, n), is_new))`.
6. Read the returned `Local` as a `Value`, and return it. The scope drops its slots.

The host is `Rc<dyn Host<Realm>>`. The trampoline reads it through a pointer that `set_host` stores (no `Rc` clone for each call, as the old engine does). `set_host` refuses while a host call runs.

### 3.4 Calls into script, and the collector

Rule 1: engine methods that a host function calls (`get`, `set`, `new_object`, `string`, and so on) allocate but never collect (fact 2).

Rule 2: `Engine::call`, `construct` and the conversions that run script (`to_number`, `to_string`, a getter) are re-entries.

- Inside a host function or a native (`api.win` is not null, or `nest > 0`): `Realm::call_function` of #785, with its hold. No collection runs until it returns.
- At the top, with no native on the Rust stack (`api.win` is null and `nest == 0`): a fresh entry on a pooled machine with no hold and no `nest` increment (`Realm::enter_host`, new, in `api.rs`). The collector may run inside this task at `gate!`, as S1.6 designs. The values of the host are all handles (`Local` cannot hold a raw `Value`), and the handle stack is a root, so this is safe.

The second rule is what makes the browser run with S1.6 at all. A browser task starts at the top: a timer, an event from the driver, a script. If every host entry held the collector, no browser task would collect inside itself.

The first rule keeps the gap that #785 documents: a loop inside a nested call that allocates a lot grows the heap until the call returns. It is a limit of this step, not of the API. Section 4.3 lists the way to lift it.

`Engine::safepoint` does nothing in this step. A safepoint needs the frames of the caller to be walkable, and a host function has no `(code, fp, pc)` of its caller (`Realm::cur` is set only by `gate!`).

### 3.5 Errors

| `bare-js-api`                            | js2                                            |
| ---------------------------------------- | ---------------------------------------------- |
| `Error::Throw(local)`                    | `Err(value)` of `R<T>`                         |
| `Error::Abort(Abort::Limit)`             | `Err(STOP)`, as a budget stop in #785          |
| `Error::Abort(Abort::Unsupported(what))` | `ApiState::abort = Some(what)` and `Err(STOP)` |

`STOP` runs no handler, so a script cannot catch an abort, as the interface says. `Vm::start` turns it into `Exit::Fuel`; the top-level `Engine::call` reads `ApiState::abort` and answers `Abort::Unsupported` or `Abort::Limit`. This part needs #785 (`STOP`).

`type_error` and `range_error` make an error object with `Realm::error`. `charge(n)` subtracts `n` from `Realm::fuel`.

### 3.6 Strings

`Engine::as_str(&self, Local)` borrows the characters and cannot flatten a rope. So a string that a `Local` names is always flat or `Indirect`:

- `api_push` flattens a rope before it pushes a result.
- The trampoline flattens the ropes of the window before it calls the host. The check is one tag compare for a value that is not a string cell.

A flatten allocates but never collects, so it is safe at both places. Hypothesis: the check costs about 4 instructions for each argument; it is not measured.

### 3.7 Persistent handles

`persist` takes the lowest free slot of `Slots` and writes the value; `persistent(id)` pushes it as a `Local`. This is the old code with `Value` of js2. The table is a root only through the live bits.

### 3.8 What the adapter supports and lacks

Supported in the first PRs (each maps to a function of `Realm` that exists):

- Values and handles: `stack_len`, `stack_truncate`, `stack_set`, `persist`, `persistent`, `slots`, `undefined`, `null`, `boolean`, `number`, `string`, `string_utf16`, `type_of`, `as_bool`, `as_number`, `as_str`, `to_boolean`, `raw`, `raw_local`, `store_raw`.
- Objects: `global`, `new_object`, `new_array`, `is_array`, `get`, `set`, `define`, `define_accessor`, `has_own`, `own_keys`, `own_enumerable_data`, `get_prototype_of`, `set_prototype_of`, `array_len`, `is_callable`, `bind`, `intrinsic` (`ObjectPrototype`, `ArrayPrototype`, `ErrorPrototype`), `new_error`.
- Calls: `call`, `construct`, `host_function`, `set_host`, `proto_from_new_target`, `to_number`, `to_string`, `task`.
- Errors: `type_error`, `range_error`, `charge`, `wall_now_ms`.
- `get_named`, `define_named` and `get_named_bool` (PR #779, not merged): `Realm::intern` and `Key::Name`, with no key `Local` and no string cell.

Each of these needs a check in the build against what js2 does today (for example `set_prototype_of` needs a cycle check, and `bind` needs the bound function that `bound_id` runs). A method that js2 cannot do yet returns `Abort::Unsupported(name)`.

Unsupported, with the reason:

| Methods                                                                                                                                                                              | Reason                                                                                                                                                                  |
| ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `iter_open`, `iter_next`, `iter_fail`                                                                                                                                                | `Realm::iter_values` collects an array or a string at once and has no iterator protocol. A later step can step an array or a string by index, with the state in `Iter`. |
| `module_*`, `take_dynamic_imports`, `finish_dynamic_import`, `reject_dynamic_import`, `fail_dynamic_import`                                                                          | no modules                                                                                                                                                              |
| `run_script`, `compile_function`, `call_scoped`, `eval_global`, `eval_function`                                                                                                      | no `eval`; the program is fixed at load. They wait for #777.                                                                                                            |
| `new_array_buffer`, `new_typed_array`, `bytes`, `bytes_mut`, `view`, `is_detached`, `view_get`, `bigint_limbs`                                                                       | no buffers, no BigInt                                                                                                                                                   |
| `set_host_owner`, `host_key`, `new_host_indexed`, `new_host_named`, `set_lazy`, `clear_lazy`                                                                                         | need collector hooks (3.9) and host-filled objects                                                                                                                      |
| `symbol_description`, `Intrinsic::IteratorPrototype`, `SymbolIterator`, `SymbolToStringTag`                                                                                          | no `Symbol`                                                                                                                                                             |
| `set_integrity`, `own_kind`, `display`, `inspect`                                                                                                                                    | later, when a handler needs them (`display` is on `Vm` now)                                                                                                             |
| `clone_out`, `clone_in`, `clone_copy`                                                                                                                                                | structured clone needs buffers, `Map`, `Set` and `Date`                                                                                                                 |

### 3.9 Not in this step: owners, `trace` and `prune`

`Engine::set_host_owner` makes an owner: the collector calls `Host::trace(key)` when it reaches the object, and `Host::prune` when the object dies. The collector of js2 (`gc.rs`) has no hook for this. `Roots::weak` runs after the marking, so it can prune but cannot mark more. The owner trace needs a loop between the mark loop and the weak step: for each marked owner not yet traced, call `trace`, mark what it visits, and drain the work list, until no new owner is marked. Its cost grows with the owners and the rounds, not with the heap. I did not design this part. It is a later PR with its own page, and the wrapper design of the browser (`design-dom-wrappers.md`) decides what it needs.

## 4. Options

### 4.1 Which type is the engine

| Option                                        | Data and cost                                                                                                                                                      | Result                                                                                    |
| --------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------ | ----------------------------------------------------------------------------------------- |
| **A. `Realm`** (chosen)                       | The api state is a field. The trampoline calls `enter(realm, ...)`.                                                                                                | No pointer from the realm back to a wrapper. One field and one root line in shared files. |
| B. A wrapper `Js2 { vm: Vm }`                 | The trampoline has only `&mut Realm`. It needs a raw pointer from the realm to the wrapper, and the host would run with two live `&mut` that name the same memory. | Unsound unless `Vm::run` hands out the wrapper, which it cannot (fact 1). Rejected.       |
| C. Change `NativeFn` to take a context struct | Every native gets `&mut Cx`. About 94 natives change (`rg` of `fn ...(&mut Realm, Value, &[Value])` in `crates/js2/src`), and the merge train on `builtins.rs`, `interp.rs` and `interp_rt.rs` (#768, #770, #774, #775) conflicts.    | Rejected. It has no gain over A.                                                          |

### 4.2 How the window is rooted and read

| Option                                                   | Cost for each call                                                                                                                     | Result                                                                                                                                         |
| -------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------- |
| **A. `Win` record with a pointer to the slice** (chosen) | One record on the Rust stack (4 words), a link and an unlink, a guard. Two `unsafe` reads (the link target and the slice) in `api.rs`. | Same cost model as the old engine, whose host call splits into 44 (`call_api_host`) and 12 (`enter`) by callgrind (`progress.md`, H0 round 2). |
| B. Copy `this` and the arguments to the handle stack     | `argc + 1` pushes and one truncate. No `unsafe`.                                                                                       | Hypothesis: 25 to 40 instructions more for 1 to 3 arguments, which breaks the "no copy, no push" rule of the owner.                            |
| C. Name the caller's registers by their stack index      | No record. The slot is an index into a stack.                                                                                          | The native does not know which machine stack it is on (fact 6), and a window in a Rust slice (fact 4) has no index. Rejected.                  |

A needs `api` in the list of unsafe modules. The unsafe code is two sites with a `SAFETY` comment each, and CI runs them under Miri. Question 1 asks for this decision.

### 4.3 What the collector does while a host function calls script

| Option                                                                  | Result                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                 |
| ----------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| **A. Hold in a nested call, no hold at the top** (chosen for this step) | Correct with the code of #785. A nested loop that allocates grows the heap until it returns. The browser tasks that start at the top collect inside themselves.                                                                                                                                                                                                                                                                                                                                        |
| B. A registry of the suspended machines                                 | Lifts the hold for calls that come from a host function (the handlers hold only handles) and keeps it for calls from a builtin (its Rust locals hold raw values). The collector walks every machine in the registry from its saved `(code, fp, pc)`. A native call would store those three words; the cost is on the hot path of a native call and is not measured. Needs `unsafe` pointers to the stacks of the outer machines, or a machine table that `Vm::run` indexes (a change of its hot loop). |
| C. Never hold                                                           | Unsound: a builtin that calls a getter keeps a `Value` in a Rust local across the call.                                                                                                                                                                                                                                                                                                                                                                                                                |

B is the design to lift the hold. I do not build it before the browser slice shows how much a nested task allocates (the measurement of design 6.11, item 2: "the largest allocation within one task"). CLAUDE.md asks to measure first.

## 5. Check

Define the check first:

1. One generic suite for the trait that both engines run. It lives in `bare-js-api` behind a feature (`suite`): a harness trait (`new`, `eval`, `collect`) and the tests of `crates/js/src/eval_api_tests.rs` that use only the interface: host functions, a callback from the host, throw, persistent handles, inner scopes, the escape scope, rooting through a collection, an engine error that crosses the host, a constructor. `bare-js` and `bare-js2` each implement the harness in a test and run the same suite. The old tests that use engine details stay where they are.
2. Every test of the suite also runs under constant collection (`heap.set_trigger(Trigger { min: 1 << 10, slack: 1 << 10 })`), as `script_tests.rs` does. A test where the host keeps a `Local` and a persistent handle across a top-level call that allocates (the case that can free a live cell) is part of the suite.
3. A test of each unsupported method: it answers `Abort::Unsupported(name)` and a script cannot catch it.
4. `cargo test --locked -p bare-js2 --lib`, `-p bare-js-api`, `-p bare-js`, `-p bare-js-diff`. `a_scope_with_many_locals_compiles_in_linear_time` fails under load, so it runs alone.
5. Miri on the new `unsafe` of `api.rs` and the window test.
6. Host call cost: `crates/js2/examples/host_call_load.rs` (a trivial host function in a loop, as `crates/bare/examples/host_call_load.rs` does for the old engine), 3 runs of `tools/js/instr.sh`, js2 against `bare-js`. The target is a total per call below the old engine's 588 and, for the trampoline alone, a figure that the PR states against the plain native call of js2 (348 now). Hypothesis: the trampoline adds 60 to 100 instructions to a native call (the old engine's split, 44 + 12 + the argument and result reads). Not measured.

## 6. Plan after approval

Each PR is at most about 400 changed lines of code (the rule of `plan-engine-rewrite.md`). It builds on #785.

1. The shared suite in `bare-js-api` and the harness of `bare-js` (no js2 code).
2. `api.rs` part 1: `ApiState`, the roots in `Realm::collect`, values, handles, persistent handles, objects, properties, the tests of 1 to 3 that apply.
3. `api.rs` part 2: `host_function` and the trampoline, `call`, `construct`, errors, `task`, `enter_host`; the cost example and the entry in `progress.md`.
4. Later, each with its own page or entry: owner trace and prune (3.9), `Promise` and the job queue (after js2 has them), modules, `run_script` (after #777), buffers, the registry of 4.3 B.

`Dom<E: Engine>` is its own mechanical PR (the decision of 2026-11-20). It follows step 3.

## 7. Questions for the owner

1. May `api` join the unsafe modules of js2 (two sites, Miri in CI), so the window needs no copy? Without it the answer is option B of 4.2, at about 25 to 40 instructions more for each call (hypothesis).
2. Is `Realm` the engine type (4.1 A)?
3. Is the hold in a nested call acceptable until the browser slice measures the largest nested allocation (4.3 A)?
4. Is it right that an `Unsupported` abort goes through `STOP` and `ApiState::abort`? It depends on #785. If #785 changes, this part changes.
5. Should the build wait for #785 to merge, or stack on its branch? I recommend to wait: the design uses `call_function`, `STOP` and `nest`.
6. Is the shared suite in `bare-js-api` behind a feature the right home (5.1)? The other way is a copy of the tests in each engine crate, which drifts.
7. `Engine::as_str(&self)` forces the flat-string rule of 3.6. The other way is to change the trait to `&mut self`, which touches every call site of the browser. Do you keep the trait?

## 8. Not measured

- The cost of the trampoline, the window flatten check, and the roots in `Realm::collect`. Only the baselines of section 2 are measured.
- The call path after #775 changes it.
- The old engine's lane in the baseline.
- The nested allocation of the journeys (needs the browser slice).
- Windows, macOS and arm64.

## 9. Decisions (owner, 2026-11-20)

1. The `api` module joins the unsafe modules of js2. The host call reads the window in place. Miri tests cover the two sites.
2. `Realm` is the engine type (4.1 A).
3. The collector stays held in a nested call (4.3 A). Build the registry of suspended machines only if the browser slice measures a large nested allocation.
4. An `Unsupported` abort goes through `STOP` and `ApiState::abort`.
5. The build waits for #785 to merge.
6. The shared suite lives in `bare-js-api` behind a feature (5.1).
7. The trait keeps `as_str(&self)`. Ropes are flattened on push and at window entry (3.6).

## 10. Built (2026-11-21)

Steps 1 to 3 of section 6 are one change (`progress.md`, entry of 2026-11-21). What differs from the text above:

- `run_script` stays unsupported. js2 has no completion value (the compiler makes none: `script::eval_program` puts the last expression in a global for the same reason). `Realm::run_text(src)` runs a script and gives no value; the shared suite harness reads the value from `__result`. `LazyCompile::script` is the new method that adds a script to a running program.
- The suite runs from an integration test of `bare-js-api` (`tests/engines.rs`), with `bare-js` and `bare-js2` as dev-dependencies, so `cargo test -p bare-js-api --features suite` runs both engines. The old engine needs the scope test inside `bare_js_api::task`, and answers `unsupported:` where js2 throws a `TypeError` for `class extends` of a `HostFn::Ctor` function.
- The cost example is `crates/js-api/examples/host_call_load.rs` (both engines in one file), not `crates/js2/examples/`.
- The `unsafe` sites of `api.rs` are four reads: the chain of windows in `ApiState::mark`, the slot of a window in `a_win`, the two lifetime casts of `as_str`, and the host pointer in `host_call`. Each has a `SAFETY` comment, and `api_tests.rs` covers them (a collection with a window and no script under it, a panic in a host function, a slot that names nothing).
- A collection never runs inside a host call today: a host function runs inside a native, and a native never collects, and a nested call holds the collector. The window is a root all the same, because the first safepoint that a later step adds needs it.
- The `Intrinsic` values `IteratorPrototype`, `SymbolIterator` and `SymbolToStringTag` answer `undefined` (js2 has no symbols and no iterator protocol).

## 11. Built: promises and microtasks (2026-11-21, plan step P2 of `design-js2-async.md`)

The promise methods, `queue_microtask`, `checkpoint`, `drain_microtasks`, `pending_jobs`, `clear_failed_job` and the three hooks (`between_jobs`, `report_exception`, `promise_rejections`) are built on the job queue of `promise.rs`. What differs from the text above:

- The job runner moved from `Vm` to `Realm::drain_jobs`: each job is a `call_held` on a pooled machine, so `Engine::drain_microtasks` (which has only `&mut Realm`) and `Vm::drain_microtasks` (tools) share it.
- A stop of a drain is sticky until `clear_failed_job`: a budget stop, `max_jobs_per_drain` and an error of `between_jobs`. The old engine does the same. An error of `promise_rejections` is not sticky.
- `drain_microtasks` inside a `task` works (js2 has no call depth to refuse it); inside a host call or a native it is `Unsupported`. `checkpoint` there answers 0.
- A script that `between_jobs` runs gets its own budget, and the budget of the checkpoint comes back after. The two other hooks run as a task level, so a script in them shares the budget of the checkpoint.
- `promise_then` makes a derived promise that nobody holds, so a throw of the handler is an unhandled rejection that the host hears (as in the old engine).
- The shared suite has the promise tests (`suite.rs`), run on both engines with and without constant collection.
