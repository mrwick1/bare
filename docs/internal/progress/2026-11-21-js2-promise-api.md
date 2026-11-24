# js2 promise methods on the host interface (step P2 of `docs/design-js2-async.md`)

What is built. `Realm` implements the promise part of `bare_js_api::Engine`: `new_promise`, `resolve_promise`, `reject_promise`, `promise_then`, `promise_react`, `promise_status`, `queue_microtask`, `checkpoint`, `drain_microtasks`, `pending_jobs`, `clear_failed_job`, and it calls the hooks `between_jobs`, `report_exception` and `promise_rejections` (`crates/js2/src/api.rs`). The job runner moved from `Vm` to `Realm::drain_jobs` (`promise.rs`): each job starts on a pooled machine at the top, with no hold, so the collector may run inside a job. `Vm::drain_microtasks` (tools, test262) is a thin wrapper over it.

Rules (also in `design-js2-host-api.md`, section 11):
- A checkpoint from a host call or a native does nothing. `drain_microtasks` there is `Unsupported`. A drain inside a `task` works.
- A stop of the drain is sticky until `clear_failed_job`: the budget, `max_jobs_per_drain` and an error of `between_jobs` (the old engine does the same). An error of `promise_rejections` is not sticky.
- The hooks run in the order of the old engine: `between_jobs` before each job and once when the queue is empty; after the queue is empty, `promise_rejections` (if there is anything to tell), then the jobs that the listeners queued. A script that `between_jobs` runs gets its own budget, and the budget of the checkpoint comes back after. The other two hooks run as a task level (they share the budget).
- The host settles a promise once: a settle of a settled promise does nothing. A value that is no promise is `Unsupported` for `resolve_promise`, `reject_promise` and `promise_then`; `promise_status` gives `None`.
- `promise_then` makes a derived promise that nobody holds, so a throw of the handler is an unhandled rejection that the host hears (as in the old engine). It reads no `then` and no `constructor`. `promise_react` is `PromiseResolve(%Promise%, x)` then `promise_then`.

A fix in P1 code that the host path showed: `take_rejections` added the rejections that did not fit the weak `reported` list (256) to `dropped`, so a host that got 1,024 rejections was told that 844 were dropped. `dropped` is now only the count over the pending bound (1,024), and a full `reported` list drops its oldest entries, as the old engine does.

Tests (archMachine, debug build, own target directory):

| Command | Result |
| --- | --- |
| `cargo test --locked -p bare-js2 -p bare-js-api -p bare-js-diff --lib` | 515 + 32 + 2 pass |
| `cargo test --locked -p bare-js-api --features suite` | 84 + 5 + 1 pass (21 suite tests, each on the old engine and on js2, plain and under constant collection) |

The shared suite got 9 promise tests (`suite.rs`), written once and run on both engines: host promise settle and reactions in order; a second settle does nothing; script and host promises crossing; one microtask; a checkpoint inside a script function; a promise that survives collections as a local settled in the call and as a persistent handle settled later (the value only in the queue); `between_jobs` before each job, a failing hook and `clear_failed_job`; the host hears a throwing callback, an unhandled rejection once and a late handler once; the bound of 1,024 rejections and the `dropped` count. `api_tests.rs` has 4 more for js2: a hook script has its own budget and the budget comes back; a job out of budget fails the drain until it is cleared; the drain collects between and inside jobs while locals, persistent handles and queued values survive (collections counted); a throw of a host reaction reaches the host. The test of the unsupported methods lost the entries of `new_promise` and `queue_microtask`.

### Cost

Base: `origin/main` `8875c60a` (no engine change since the merge base). Release builds, own `CARGO_TARGET_DIR` for each tree, `tools/js/instr.sh` (minimum of 3 runs), 2 rounds in the order base, branch. Machine archMachine.

| Row | Base | Branch | Change |
| --- | ---: | ---: | ---: |
| `interp_load loopfn 3000000` | 800,383,029 | 800,383,341 | +0.000% |
| `interp_load calls 1000000` | 572,440,038 | 572,440,232 | +0.000% |
| `interp_load fib 27` | 258,288,042 | 258,288,113 | +0.000% |
| `interp_load closures 1000000` | 1,941,800,205 | 1,941,797,981 | -0.000% |
| `interp_load objects 1000000` | 720,312,287 | 720,312,008 | -0.000% |
| `host_call_load new host 100000` | 75,290,550 | 75,290,905 | +0.000% |
| `host_call_load new native 100000` | 70,299,675 | 70,299,455 | -0.000% |
| `host_call_load new none 100000` | 26,981,758 | 26,985,205 | +0.013% |

No row is over +0.5% (the setup counts at N = 0 are within +0.05% too), so the `codegen-units=1` rule is not needed. The adapter row (`tools/js/adapter_load.sh` builds `crates/js/examples/adapter_load`): the binary of the branch is byte-identical to the base (`cmp`), because this change touches no code that `bare-js` links, so its counts are the base counts. `crates/bare` links `bare-js2` for one constant (`NOT_READY`) only. I did not run the full cost matrix: its rows are binaries of `bare-js`, `bare` and `bare-stress`, which this change leaves as they are. This is a hypothesis for `bare` and `bare-stress` (not compared byte by byte).

The promise rows of js2 (`js2_promise_load`, `Vm::drain_microtasks`) got slower, because a job now starts on a pooled machine instead of on the stack of the `Vm` (a pop and a push of a 48-byte record, and the result mapping). The step is the difference of two scales divided by the difference of the scales:

| Row | Base | Branch | Change |
| --- | ---: | ---: | ---: |
| `then` | 1,832 | 1,875 | +2.4% |
| `thenable` | 2,800 | 2,874 | +2.6% |
| `microtask` | 895 | 926 | +3.5% |
| `new` | 2,601 | 2,594 | -0.3% |

A first version of the runner started a job through `call_held`, which read the function twice (`call_script`, then `Vm::start`): it was +100 instructions per job (+5% to +10%). Callgrind on `microtask` showed the second read of the function (+32 per job) and the extra call layers. `job_call` now reads it once (`start_pooled`). The rest is the pool. Keeping the stack of the `Vm` for `Vm::drain_microtasks` would remove it for the tools, but the browser path (`Engine::drain_microtasks`) pays it anyway, so I kept one runner.

### Not tested

Windows and macOS; Miri (no new `unsafe`); the full cost matrix (see above); a real browser loop on js2 (`crates/bare` is not wired, step A3); `promise_react` with a subclass whose `constructor` is a getter that throws (no test); `Symbol.species` (P3); `between_jobs` that calls `set_host`.

### Cost decision (main session, 2026-11-21)

The js2 `js2_promise_load` steps are +2.4% to +3.5%. js2 does not run in the browser yet, so these rows do not reach a user. The same rule held for #757. The fix waits for a profile of the journeys on js2.
