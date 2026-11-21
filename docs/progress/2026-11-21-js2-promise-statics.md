# js2 Promise statics, `finally`, `Symbol.species` and `AggregateError` (step P3 of `docs/design-js2-async.md`)

What is built, in Rust (owner decision 2026-11-21: no prelude). New file `crates/js2/src/promise_statics.rs`:
- `Promise.all`, `allSettled`, `any`, `race`, `withResolvers`, `try`, `Promise.prototype.finally`, `Promise[Symbol.species]`, `Promise.prototype[Symbol.toStringTag]`.
- `AggregateError` (constructor, prototype, the own `errors` property). `Promise.any` throws it with no `message`, as the specification says (V8 adds a text).
- `SpeciesConstructor` for `then`. It replaces the P1 rule "use `constructor` when it is `Promise` or inherits from it". `Promise[Symbol.species]` that is the built-in getter is found with one cached slot read (a `Spot`), so `then` on a plain promise reads nothing that a script can see. The thenable fast path of the job runner (`thenable_job`) takes the same check: a script that replaces the getter sees it called.
- One loop, `Realm::combine`, for the four combinators. It reads `C.resolve` once, opens the iterable, and for each element calls `C.resolve(next)` then `.then(...)`. A throw closes the iterator and rejects the capability. The state of the element functions is one 4-value `Slots` record, and each element function is a native cell (slots: the record, the index, the twin of an `allSettled` pair). No new `unsafe`.
- `Realm::construct_script` (`reenter.rs`) constructs a plain function or a class from a native, as a nested call with a collector hold: a base class runs its field initializers first, and a derived class runs `super(...)`. `Vm::start_new` is the entry (`interp.rs`: `Vm::start` and `Vm::start_new` share `start_at`). This is what lets `new_capability` build a promise with `class P extends Promise` or with a plain function (P1 threw a `TypeError` for both). The host `Engine::construct` of a class is still `Unsupported` (`api.rs` is not changed).
- Iteration: the iterator protocol is step G3. Until then the combinators follow it for an iterable that has `Symbol.iterator` (a generic object, with `return` called on a throw) and use `Realm::iter_values` for an array or a string without it. The values of an array are read at the start (a script that changes the array during the loop is not seen).

Tests (archMachine, debug build):

| Command | Result |
| --- | --- |
| `cargo test --locked -p bare-js2 --lib` | 516 pass, 1 fails: `compile::tests::a_scope_with_many_locals_compiles_in_linear_time` (a timing test; it fails when it runs next to the other tests and passes alone in 7 s; it does not touch this change) |
| `cargo test --locked -p bare-js2 --lib promise` | 14 pass |
| `cargo test --locked -p bare-js-api --features suite` | 84 + 5 + 1 pass |

`promise_tests.rs` got 2 tests over one table of 48 scripts (`STATIC_CASES`): the results of Node 22 (by hand: `Promise.try`, which Node 22 lacks, and the missing `message` of the `AggregateError` of `Promise.any`), once plain and once under constant collection. They cover the order of the jobs of each combinator against `then` chains, empty and non-iterable input, a generic iterable with `return`, a `Promise.resolve` that throws, `finally` with a value, a throw, a thenable and a non-function, a plain function and a class as constructor, `Symbol.species` that is `undefined`, a constructor, or a getter that counts its reads, and a class that extends `Promise`.

### test262

`tools/js/test262_js2.sh -j 4` at the pinned commit. The subset file no longer excludes `built-ins/Promise/{all,allKeyed,allSettled,allSettledKeyed,any,race}`, and the skip file allows the features `Promise.allSettled`, `Promise.any`, `Promise.prototype.finally`, `promise-with-resolvers`, `promise-try` and `AggregateError`. `await-dictionary` (`allKeyed`, `allSettledKeyed`) stays skipped: it is a proposal that is not built.

| | main (`8875c60a`) | this branch |
| --- | ---: | ---: |
| `built-ins/Promise`, js2 pass / fail | 161 / 31 | 610 / 13 |
| `built-ins/Promise`, old engine pass / fail | 190 / 2 | 602 / 21 |
| js2, all directories, pass | 17,941 | 18,394 |
| js2, tests that passed on main and fail now | | 0 |

The 13 failures of `built-ins/Promise`, all from a missing feature (the old engine passes 6 of them; 2 of them (`resolve` and `reject` `ctx-non-ctor`) failed on js2 on main too):
- 8 `*/ctx-non-ctor.js` (`all`, `allSettled`, `any`, `race`, `reject`, `resolve`, `try`, `withResolvers`): the test calls `eval` as the non-constructor. js2 has no `eval` (`ReferenceError`).
- 4 `{all,allSettled,any,race}/resolve-throws-iterator-return-is-not-callable.js`: the test has a BigInt literal (js2 has no BigInt, S2.7).
- 1 `prototype/finally/this-value-proxy.js`: needs `Proxy`.

The baseline file `test262-js2-baseline.txt` is the new full run (`-u` format). Other counts in it moved only where a test names `AggregateError` or a `Promise` feature that the skip file now allows (`Object/seal`, `Object/prototype/toString`, `subclass-builtins`: +1 pass each).

### Cost

Base for the micro rows: the P2 branch `380cf408` (this PR is stacked on it). Release builds, own `CARGO_TARGET_DIR` for each tree, `tools/js/instr.sh` (minimum of 3 runs), 2 rounds in the order base, branch. Machine archMachine.

| Row | Base | Branch | Change |
| --- | ---: | ---: | ---: |
| `interp_load loopfn 3000000` | 800,383,341 | 800,398,675 | +0.002% |
| `interp_load calls 1000000` | 572,440,418 | 572,454,834 | +0.003% |
| `interp_load fib 27` | 258,288,147 | 258,301,139 | +0.005% |
| `interp_load closures 1000000` | 1,941,800,519 | 1,941,833,321 | +0.002% |
| `interp_load objects 1000000` | 720,312,027 | 720,328,589 | +0.002% |
| `host_call_load new host 100000` | 75,291,977 | 75,506,026 | +0.284% |
| `host_call_load new native 100000` | 70,301,332 | 70,514,540 | +0.303% |
| `host_call_load new none 100000` | 26,982,614 | 26,997,238 | +0.054% |

**Startup (eager natives; no laziness built, as asked).** The setup rows (N = 0) of all these programs grow by 14,000 to 16,000 instructions: `interp_load ... 0` +1.3% to +1.7% (of about 0.9M), `host_call_load new ... 0` +0.6% to +0.75% (of 2.2M), and `script_run` of `var x = 1;` 1,969,288 to 1,984,148 (+14,860, +0.755%). This is the cost of 8 element natives, 7 methods (`all`, `allSettled`, `any`, `race`, `withResolvers`, `try`, `finally`), the species getter, the `toStringTag`, and the `AggregateError` constructor and prototype, made at each `Realm`. The host call rows at N = 100000 are +0.05% to +0.30%. Their call step (row at 100,000 minus row at 0, divided by 100,000) is 731.05 base and 733.03 branch for `host` (+2.0 instructions, +0.27%), and no change for `none` (248.02 on both). I did not find the cause of the 2 instructions (hypothesis: the inlined `start_at` changes the layout of the host entry; to verify with callgrind).

The `Promise` rows of js2 (`promise_load`, step = difference of two scales / difference of the scales):

| Row | Base (step) | Branch (step) | Change |
| --- | ---: | ---: | ---: |
| `then` | 1,875 | 1,913 | +2.0% |
| `thenable` | 2,874 | 2,960 | +3.0% |
| `microtask` | 926 | 916 | -1.1% |
| `new` | 2,601 | 2,610 | +0.4% |

`then` is slower because `SpeciesConstructor` now reads `Promise[Symbol.species]` (one lookup of a cached slot, about 25 instructions) and `thenable` has it twice (the `then` and the fast path of the thenable job). That is new work, not a codegen-unit effect. With `RUSTFLAGS="-C codegen-units=1"` on both trees: `then` +3.7%, `thenable` +4.7%, `microtask` +0.1%, `new` +0.6%. A first version of the entry point (`start` calling a non-inlined `start_at`) made `microtask` +2.4%; `#[inline(always)]` on `start_at` removed it and gave `microtask` back about 10 instructions for each job over P2.

**Against the old engine** (`crates/js/examples/promise_load` and `promise_all_load` for the old engine, `crates/js2/examples/promise_all_load.rs` for js2, new: the same programs; instructions for each item, scale 100 minus scale 10, divided by 90,000; the case `pthenable` is `thenable` of `promise_load`):

| Case | Old engine | js2 | js2 / old |
| --- | ---: | ---: | ---: |
| `chain` | 6,327 | 2,245 | 0.35 |
| `fanout` | 6,834 | 2,563 | 0.38 |
| `pthenable` | 14,450 | 6,358 | 0.44 |
| `finally` | 21,150 | 7,725 | 0.37 |
| `reject` | 12,563 | 5,860 | 0.47 |
| `ctor` | 14,698 | 5,843 | 0.40 |
| `closures` | 12,149 | 4,580 | 0.38 |
| `all` | 21,520 | 7,097 | 0.33 |
| `settled` | 25,616 | 9,725 | 0.38 |
| `race` | 15,533 | 5,516 | 0.36 |
| `any` | 17,237 | 5,924 | 0.34 |
| `anyerr` | 19,275 | 6,436 | 0.33 |
| `many` | 12,478 | 4,380 | 0.35 |
| `thenable` | 23,236 | 7,882 | 0.34 |

No case is slower than the old engine; js2 uses 0.33 to 0.47 of its instructions. Wall time and peak memory were not measured here.

### Not tested

Windows and macOS; Miri (no new `unsafe`; `construct_script` and `start_new` are safe code over `Vm::run`); the full cost matrix (the rows of `bare-js`, `bare` and `bare-stress` are binaries that this change leaves as they are; hypothesis, not compared byte by byte); a combinator over a generator or any iterable whose `Symbol.iterator` is a built-in (js2 has none yet, G3); the `Engine::construct` of a class on the host interface (`Unsupported`, not changed); the memory of a combinator (the old engine's `promise_all_load` prints it, the js2 example does not).
New entries: [progress/](progress/README.md).

### Cost decision (main session, 2026-11-21)

The js2 `promise_load` rows `then` and `thenable` are over +0.5% (species reads), and startup is +0.755% (eager natives). js2 does not run in the browser yet, so the rows do not reach a user (the rule of #757). Lazy built-ins (step 1 of `docs/design-js2-lazy-builtins.md`) pay the startup back. The `then` cost waits for a profile of the journeys on js2.
