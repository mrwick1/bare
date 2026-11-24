# Promises and microtasks through the host interface

Status: decided by the owner, 2026-11-19 (plan step S0.4, promises and microtasks). See "Decisions" at the end.

## Problem

The browser uses promises and the microtask queue through 20 items of `Interp` and `Host` (`docs/design-host-interface.md`, section 6). It passes them as raw `Value` and `ObjId`. Three things must change before the browser runs on `bare-js-api`:

1. The interface has no promise, microtask or iteration method. The browser cannot call them.
2. The host keeps promises in its own tables. Some are roots (`Host::roots`). After S0.2 a root is a `Persistent`, and a reference that an object owns is a `Traced` (`docs/design-host-tracing.md`).
3. The three host hooks (`report_exception`, `between_jobs`, `promise_rejections`) get `&mut Interp` and `Value`. They need `&mut Scope` and `Local`.

Today a collection runs only between tasks. A host local is safe by accident. With a collection inside a task (S1.6) each raw `Value` that the host holds across an allocation is a bug. This page lists them.

## Current design

Engine side (`crates/js/src`):

- The promise table (`PromiseState`, `eval_promise.rs:144`), the job queue (`queue`, a `VecDeque<Job>`), the rejection lists and the resolver data are engine state. The collector traces them. The host never sees them, so they need no interface for tracing.
- `drain_microtasks` (`eval_promise.rs:227`) and `host_checkpoint` (`eval_host.rs:200`) run `drain_queue` (`eval_promise.rs:245`): before each job it calls `Host::between_jobs` (`eval_promise.rs:780`), then it runs the job. At the end it calls `Host::promise_rejections` once (`notify_rejections`, `eval_promise.rs:269`).
- `host_checkpoint` does nothing unless it runs inside a top-level `host_scope` (`depth == 1`). This is the HTML rule "do nothing when the JavaScript stack is not empty".
- A failed job makes later drains fail. `clear_failed_job` (`eval_promise.rs:303`) clears that state after the host aborts the task.
- `HostIter` (`eval_host.rs:24`) holds an `It` (iterator object, `next` method). It "needs no rooting" because nothing is collected during a host call.

Browser side. Counts are call sites outside `*_tests.rs` files in `crates/bare/src`:

| Item                                                           |     Sites | Where                                                                                                                    |
| -------------------------------------------------------------- | --------: | ------------------------------------------------------------------------------------------------------------------------ |
| `host_new_promise`                                             |        24 | fetch 7, streams 6, web 5, img 2, media 2, cssom 1, custom 1                                                             |
| `host_resolve_promise`                                         |        10 | streams 4, img 2, custom 2, fetch 1, cssom 1                                                                             |
| `host_reject_promise`                                          |        20 | streams 10, fetch 3, img 3, media 2, cssom 1, custom 1                                                                   |
| `host_promise_then`, `host_promise_react`                      |       2+2 | streams 3, basics 1                                                                                                      |
| `promise_status`                                               |         3 | rejection 2, loader modules 1 (and 1 test)                                                                               |
| `queue_microtask`                                              |         3 | scheduler (the global), mutation observers, custom elements                                                              |
| `host_checkpoint`                                              |        23 | scheduler 11, fetch 3, and 1 each in 9 files (events, frame, idle, intersection, navigation, platform, resize, xhr, ...) |
| `drain_microtasks`                                             |         2 | `Browser::drain` (`browser.rs:2053`), custom elements (`browser_custom.rs:767`)                                          |
| `pending_jobs`                                                 |         2 | `page.rs:671`, `page.rs:734` (and 11 test sites)                                                                         |
| `clear_failed_job`                                             |         1 | `Browser::survive` (`browser.rs:2019`)                                                                                   |
| `Host::between_jobs`, `promise_rejections`, `report_exception` |    1 each | `browser.rs:1005` to `1015`                                                                                              |
| `host_iter_open`, `host_iter_next`, `host_iter_fail`           | 11, 10, 8 | scheduler, fetch, abort, matrix, web, navigation params, custom. `host_iter_close`: 0. `HostIter` in 2 signatures.       |
| `MAX_PENDING_REJECTIONS`                                       |         1 | the text of a console message (`browser_rejection.rs`)                                                                   |
| `PromiseLimits`, `set_promise_limits`                          |         0 | tests only (5 sites). The limits stay engine configuration.                                                              |

The bodies of the hooks:

- `between_jobs` runs the queued inline scripts (`run_inline_scripts`, `browser_loader.rs:442`). It runs a script with `host_scope` and `run_script`, so the interface also needs script execution (the loader PR, not this one).
- `promise_rejections` builds `PromiseRejectionEvent` and dispatches it (`browser_rejection.rs`). It reads `promise_status` and data properties.
- `report_exception` reports a throw of a `queueMicrotask` callback to `window.onerror`.

What the host holds for a promise. Most promises are settled in the call that made them: 21 of the 24 `host_new_promise` sites call `resolve` or `reject` before the function returns (`browser_fetch::settle`, `blob_text`, `subtle_*`, `replace`). Six places keep a pending promise:

| Holder                                                              | Type                          | Kept alive today                           |
| ------------------------------------------------------------------- | ----------------------------- | ------------------------------------------ |
| fetch queue, `Pending::promise` (`browser_fetch.rs:425`)            | `Value`                       | `Fetch::roots`                             |
| `customElements.whenDefined`, `when` (`browser_custom.rs:203`)      | `HashMap<Box<str>, Value>`    | `ce_roots`                                 |
| `play()` promises, `pending`, `aborting` (`browser_media.rs`)       | `Vec<Value>`                  | `media::roots`                             |
| image `decode()` promises, `decodes` (`browser_img.rs:45`)          | `HashMap<NodeId, Vec<Value>>` | `img::roots`                               |
| stream reads, `Rdr::reads` (`browser_streams.rs:113`)               | `VecDeque<Value>`             | `Streams::trace` (owner: the reader)       |
| fetch abort algorithm, `Algo::Fetch(Value)` (`browser_abort.rs:34`) | `Value`                       | `browser_abort::trace` (owner: the signal) |

Fetch, streams and abort belong to `docs/design-host-fetch.md`. This page covers the first four, and the one raw `ObjId` in the loader (`Modules::evals`, `docs/design-host-modules.md`).

## Measurements

archMachine, `origin/main` 37df5bd1, release build with symbols, callgrind, one run for each row, `BARE_JS_EXEC=auto`. `tools/js/instr.sh` (minimum of 3 runs) gives the totals.

| Row                      |   Total | Hook `Dom::between_jobs` |  Calls |
| ------------------------ | ------: | -----------------------: | -----: |
| `streams_load flow 1`    | 212.6 M |           0.72 M (0.33%) | 20 000 |
| `scheduler_load mixed 1` |  56.3 M |           0.22 M (0.38%) |  6 008 |
| `fetch_load post 1`      |  22.4 M |          0.018 M (0.08%) |    502 |

The hook costs 36 instructions per call in all three rows. The call from the engine (`Rc` clone, depth check, step limit) is not in this number.

| Operation (inclusive)   | Instructions per call | Row                                           |
| ----------------------- | --------------------: | --------------------------------------------- |
| `host_new_promise`      |                   280 | `streams_load flow 1`                         |
| `host_resolve_promise`  |            185 to 530 | `fetch_load objects 1`, `streams_load flow 1` |
| `host_promise_react`    |                 1 005 | `streams_load flow 1`                         |
| `host_promise_then`     |                   490 | `streams_load churn 1`                        |
| `host_iter_open`        |                   375 | `fetch_load objects 1`                        |
| one job (`drain_queue`) |           about 8 100 | `streams_load flow 1`: 162 M for 20 000 jobs  |

Each of these calls is under 3% of any row. A wrapper of 10 to 20 instructions for a `Local` push is under 0.1% of a row. Not measured: the real wrapper cost. It exists only after the adapter has the methods.

Hypothesis: the rows `promise_load *`, `promise_all_load *`, `async_load *` and `iterator_load promise_new` run in `bare-js` with no host. They test the engine queue, not the hooks. The browser rows that use the hooks are `fetch_load`, `streams_load`, `scheduler_load`, `loader_load mixed`, `xhr_load` and `web_apis_load abort`.

## How other engines do it

From memory of each engine's source. To verify before the build.

| Engine                     | Queue                                                                                                          | Host hook                                                                                          |
| -------------------------- | -------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------- |
| V8 with Blink              | `MicrotaskQueue` in the engine. `EnqueueMicrotask`, `PerformCheckpoint` (run when the call depth is 0).        | `PromiseRejectCallback` for rejections. Blink runs checkpoints itself between tasks and callbacks. |
| SpiderMonkey with Gecko    | A host `JobQueue` object: the engine calls `enqueuePromiseJob`; the host runs the jobs (`JS::RunJobs`).        | `JS::SetPromiseRejectionTrackerCallback`.                                                          |
| JavaScriptCore with WebKit | Engine queue (`VM::queueMicrotask`); the host calls `drainMicrotasks`.                                         | `promiseRejectionTracker` callback in the global object's method table.                            |
| Ladybird                   | Engine-side `VM::enqueue_promise_job`, host callback `host_enqueue_promise_job`; the web layer owns the queue. | `host_promise_rejection_tracker`.                                                                  |

V8 and JavaScriptCore keep the queue in the engine. SpiderMonkey and Ladybird let the host own it. All four give the host a rejection callback and no per-job hook. Bare needs `between_jobs`, because it runs inserted inline scripts between jobs (HTML runs them inside the insertion).

## Options

### A. The engine keeps the queue; the interface adds methods and handle-typed hooks (recommended)

- Engine methods on `Engine` and `Scope`: `new_promise`, `resolve_promise`, `reject_promise`, `promise_then`, `promise_react`, `promise_status`, `queue_microtask`, `checkpoint`, `drain_microtasks`, `pending_jobs`, `clear_failed_job`, `iter_open`, `iter_next`, `iter_fail`. Each returns a `Local` or a plain number. The sketch is below.
- Host hooks on `Host<E>`: `report_exception`, `between_jobs`, `promise_rejections`. They get `&mut Scope` and `Local` slices.
- A promise that the host settles later is a `Persistent`. A promise that an owner holds (a reader's pending reads) is a `Traced`. A promise that the host makes and settles in one call is a `Local`.
- Cost: one engine call for each old call, plus a `Local` push (hypothesis: 10 to 20 instructions). A job costs one more host call: the hook, 36 instructions today plus the scope.
- Memory: a persistent slot is 8 bytes plus its id. Six places keep one.

### B. The host owns the job queue (SpiderMonkey, Ladybird)

- The engine calls `Host::enqueue_job(job)` for each reaction. The host keeps the job and runs it later. `between_jobs` goes away, because the host loop decides what runs between jobs.
- Cost per job: the host must keep the job's values alive (a `Persistent` or a table entry, about 70 instructions for create and drop, from #695), plus a host call. Hypothesis: about 150 instructions for each job. A job is about 8 100 instructions in `streams_load flow 1`, so that is +1.9%. `promise_load chain 1` has a job of at most 6 300 (8.4 M over about 1 300 jobs): +2.4%. Both are over the 0.5% rule.
- Gain: the host can reorder jobs. The browser has no need for that.

### C. A gated hook: option A, and the engine calls `between_jobs` only when the host asks

- The host sets a flag (`Engine::want_between_jobs(bool)`) when it queues an inline script, and clears it when the queue is empty. The engine skips the host call while the flag is off.
- Gain: the 36 instructions of the hook per job (0.33% of `streams_load flow 1`) and the interface cost of the scope. Cost: one flag, one more rule for hosts, and a stale flag is a lost script.
- A can move to C later without an interface break: the flag is an addition.

### Comparison

|                        | A. Engine queue + hooks          | B. Host queue                          | C. A + gated hook        |
| ---------------------- | -------------------------------- | -------------------------------------- | ------------------------ |
| Cost per job           | +10 to 20 (hypothesis)           | about +150 (hypothesis, from #695)     | about -30                |
| `promise_load chain 1` | unchanged (no host)              | about +2.4% (hypothesis)               | unchanged                |
| `streams_load flow 1`  | about +0.1% (hypothesis)         | about +1.9% (hypothesis)               | about -0.2% (hypothesis) |
| New interface methods  | 14 plus 3 hooks                  | 14 plus 3 hooks plus job type          | A plus 1                 |
| Engine change (old)    | small: wrappers over `host_*`    | large: a job type crosses the boundary | A plus a flag            |
| New engine (S1, S3)    | the queue is in the promise code | same, plus a host job type             | same as A                |

## Interface (option A)

On `Engine`; `Scope` gets the same names as thin wrappers, as in S0.2. `PromiseState<'s>` is `Pending`, `Fulfilled(Local<'s>)` or `Rejected(Local<'s>)`.

```rust
// Promises. The engine owns the state; the host holds the promise as Local, Persistent or Traced.
fn new_promise<'s>(&mut self) -> Result<Local<'s>, Error<'s>>;                 // host_new_promise
fn resolve_promise<'s>(&mut self, p: Local<'_>, v: Local<'_>) -> Result<(), Error<'s>>;
fn reject_promise<'s>(&mut self, p: Local<'_>, reason: Local<'_>) -> Result<(), Error<'s>>;
fn promise_then<'s>(&mut self, p: Local<'_>, ok: Local<'_>, err: Local<'_>) -> Result<(), Error<'s>>; // intrinsic: reads no `then`
fn promise_react<'s>(&mut self, x: Local<'_>, ok: Local<'_>, err: Local<'_>) -> Result<(), Error<'s>>; // PromiseResolve, then `then`
fn promise_status<'s>(&mut self, v: Local<'_>) -> Option<PromiseState<'s>>;   // None: not a promise
// Microtasks.
fn queue_microtask<'s>(&mut self, cb: Local<'_>) -> Result<(), Error<'s>>;    // the engine roots cb
fn checkpoint<'s>(&mut self) -> Result<usize, Error<'s>>;                     // host_checkpoint: no-op with a script on the stack
fn drain_microtasks<'s>(&mut self) -> Result<usize, Error<'s>>;               // only with an empty stack
fn pending_jobs(&self) -> usize;
fn clear_failed_job(&mut self);
// Iteration of a script iterable. `Iter<'s>` holds two Locals (the iterator, its `next`) in the scope.
fn iter_open<'s>(&mut self, v: Local<'_>) -> Result<Option<Iter<'s>>, Error<'s>>;
fn iter_next<'s>(&mut self, it: &mut Iter<'s>) -> Result<Option<Local<'s>>, Error<'s>>; // charges one step
fn iter_fail<'s>(&mut self, it: &mut Iter<'s>, err: Error<'s>) -> Error<'s>;  // IteratorClose with the error kept
// On Host<E>. The engine pushes the Locals before the call and drops them after it.
fn report_exception<'s>(&self, s: &mut Scope<'s, E>, thrown: Local<'s>) {}
fn between_jobs<'s>(&self, s: &mut Scope<'s, E>) -> Result<(), Error<'s>> { Ok(()) }
fn promise_rejections<'s>(&self, s: &mut Scope<'s, E>, unhandled: &[Local<'s>], handled: &[Local<'s>], dropped: u32) -> Result<(), Error<'s>> { Ok(()) }
pub const MAX_PENDING_REJECTIONS: usize = 1024; // in bare-js-api: both engines keep at most this many
```

Rules:

- `checkpoint` and `drain_microtasks` may collect. A `Local` that the host made before the call stays valid, because it is in the scope. A value that the host keeps in a `Traced` of an owner that is no longer reached is gone.
- `iter_next` pushes one slot per item. A loop over a large iterable runs each step in an inner scope and moves the item out with `escape_scope`, or keeps the item in a `Persistent`. The 10 sites of the browser charge a step per item and have caps (`max_listeners`, the header cap).
- `host_iter_close` has no site. It stays out.
- `unhandled_rejections` and `pending_rejections` have no site. They stay out.
- `set_promise_limits` and `PromiseLimits` stay on the engine type (engine configuration, as `EvalLimits`).

## Old-engine calls that leave the old hook

None of `Host::trace` or `Host::prune` is involved: the promise state is engine state. Three lines leave `Host::roots` (`browser.rs:898`), because their promises become `Persistent`:

- `crate::browser_media::roots` loop over `pending` and `aborting` (`browser_media.rs:155`).
- `crate::browser_img::roots`, the `decodes` line (`browser_img.rs:50`).
- `ce_roots`, the `s.when.values()` line (`browser_custom.rs:824`).

Fetch roots (`Fetch::roots`, the `Pending::promise` part) leave with the fetch PR.

## Risk to the cost rule

- No row exercises the hooks and the promise calls together at a scale that hides a change. The rows are listed in "Measurements". The matrix gate (+0.5%, both allocators) decides each PR.
- The largest risk is the per-job hook in `streams_load flow *` and `scheduler_load mixed *` (0.33% and 0.38% today). A wrapper that adds 20 instructions raises them by about 0.2%. If a row passes +0.5%, option C removes it.
- Rows with settled-at-once promises (`fetch_load objects *`: 1 000 rounds, 1 000 `host_new_promise`) pay the `Local` push for each. Hypothesis: under 0.05%.
- A promise moved from `Host::roots` to a `Persistent` pays about 70 instructions for create and drop. It is paid once for each pending promise (a `play()` call, a `whenDefined` name, an image decode). No cost row uses them (hypothesis: not measured).

## Plan

1. PR M1, interface: the methods and hooks above in `bare-js-api`; the adapter in `crates/js/src/eval_api.rs` (wrappers over `host_*`, a `Local` push for each result); a test that a promise settles and a job runs through the interface, and that `between_jobs` sees a `Scope`. No browser change. Gate: the matrix, which must show no change because nothing calls the new code.
2. PR M2, the three root lines move to `Persistent` (media, img, custom). Tests: a `play()` promise and a `whenDefined` promise that the script dropped still settle. Gate: the matrix.
3. PR M3, the three hooks: `Dom` implements them on `bare_js_api::Host`, the old `Host` methods go, the engine calls the interface hook. This needs `run_script` on the interface for `between_jobs`. Gate: the matrix, and `streams_load flow *`, `scheduler_load mixed *` and `fetch_load post *` measured again.
4. The 24 + 10 + 20 call sites move from `it.host_*` to `Scope` methods when their file becomes generic (open question 1). `browser_fetch.rs`, `browser_streams.rs` and the others go with their subsystem PRs.

Order: M1, M2, M3. The fetch PR (F1 in `design-host-fetch.md`) uses M1. The modules PR uses M1 (`Local` promises and `promise_status`).

## Open questions for the owner

1. Do the handler bodies move to `Scope` and `Local` inside the subsystem PRs, or in one later PR after all subsystems hold `Persistent` and `Traced`? The four fetch files alone have about 150 handler functions with `Interp` and `Value` in their signatures. This page and the fetch page assume the later PR: the subsystem PRs change references and add methods, the browser calls the old `Interp` until then (as the timers, events and wrapper PRs did).
2. Option A, B or C? (Proposed: A; C only if a row passes +0.5%.)
3. `iter_next` pushes one slot for each item (proposed). Or: `iter_next` writes into one reused slot, and a host that keeps an item copies it?
4. `MAX_PENDING_REJECTIONS` in `bare-js-api` (proposed), or `promise_rejections` gets the bound as an argument?

## Decisions (owner, 2026-11-19)

The owner accepted every proposed answer.

1. Option A: the engine keeps the promise state and the job queue; the interface adds methods; the three host hooks take `Scope` and `Local`.
2. The handler bodies move to `Scope` and `Local` in one later PR, after all subsystems hold `Persistent` and `Traced`. The subsystem PRs change references and add methods only.
3. `iter_next` pushes one slot for each item.
4. `MAX_PENDING_REJECTIONS` is a constant in `bare-js-api`.
