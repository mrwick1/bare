# js2 async generators and `for await`

Stacked on [top-level await](2026-11-21-js2-top-level-await.md) (#844). Design: `docs/design-js2-async.md` (step A2). Before this change, `async function*` and `for await` threw `Unsupported`. They caused 45 of the 51 module tests that pass on the old engine and fail on js2.

## What is built

- Runtime in `asyncgen.rs`. An async generator is a generator object with the flag `F_AGEN`. It has a request queue: one array in an inline slot of the object, with 3 entries for each request (mode, value, promise). `next`, `throw` and `return` add a request and give its promise. The spec steps are named in the code: `AsyncGeneratorCompleteStep`, `AsyncGeneratorDrainQueue`, `AsyncGeneratorAwaitReturn`, `AsyncGeneratorYield` and `AsyncGeneratorUnwrapYieldResumption`.
- The activation is its own promise reaction, as for an async function (A1). A resume after an `await` goes to `agen_job`. A resume after a `yield` goes to the next request.
- Objects: `%AsyncIteratorPrototype%`, `%AsyncGeneratorPrototype%`, `%AsyncGeneratorFunction.prototype%`, and `%AsyncFromSyncIteratorPrototype%` with `next`, `return` and `throw` (with the close of the sync iterator and the `Continuation` step). The realm makes them at the first use (`ag_proto`).
- Compiler (`compile_asyncgen.rs`, small edits in `compile_stmt.rs`, `compile_async.rs`, `compile_gen.rs`): `yield v` waits for `v` and then yields it. `return x` waits for `x`. `return(x)` sent into a suspended generator waits for `x`. `yield*` is a loop with `AYsCall`, an `await` and `AYsUnpack`. `for await` is the loop of `for-of` with `AIterOpen`, `AIterNext`, an `await` and `AIterUnpack`. A jump out of `for await` closes the iterator with `AIterClose` or `AIterCloseT` and waits for what `return()` gave.
- 8 new ops (`AIterOpen`, `AIterNext`, `AIterUnpack`, `AYsUnpack`, `AIterClose`, `AIterCloseT`, `AIterCheck`, `AYsCall`). All are in `cold_ops!` and in the cold copy of the loop. `Yield` and `RetGen` check `gen_kind` and call `agen_yield` and `agen_return` for `GK_ASYNCGEN`. The verifier knows the new ops and the new kind.
- No `unsafe`. No trait change. No GC change: the new objects are rooted through `consts` and the generator record.

## Tests

- `asyncgen_tests.rs`, 11 tests, each case run plain and under constant collection: results and promises, `yield` waiting for its operand, `await` and `return` in the body, the queue, `return` and `throw` requests, calls with a wrong `this`, the prototype objects, `for await` (async generators, arrays, sync iterables, custom `Symbol.asyncIterator`, destructuring, `continue` and labels), close on `break`, `throw` and `return`, `yield*`, and the close of the sync iterator by the async-from-sync iterator.
- Three older tests expected `async generator is unsupported`. They are removed.
- `cargo test --locked -p bare-js2 --lib` and `cargo test --locked -p bare-js-api --features suite` pass.

## test262

Machine: archMachine. Same rules for both trees (`test262_js2.sh`, `test262-js2-skip.txt`). Base: the tree before this change.

| classic (`tools/js/test262_js2.sh`) |   base |    now |
| ----------------------------------- | -----: | -----: |
| pass (js2)                          | 25,975 | 26,047 |

0 passes lost. 72 gained. `tools/fixtures/js/test262-js2-baseline.txt` is written with `-u`.

| modules (`test262_modules new`, 1,604 files, skip file without `allow:` lines) |  base |   now |
| ------------------------------------------------------------------------------ | ----: | ----: |
| pass                                                                           | 1,068 | 1,113 |
| fail                                                                           |    70 |    25 |

0 passes lost. 45 gained. No async generator or `for await` test is left in the 25 that fail. The causes of the 25: module namespace objects (own-property, descriptor and uninitialized-binding rules, 18), `with` (6, not built in js2) and `eval` (1, not defined in js2).

## Cost

Release builds, own `CARGO_TARGET_DIR` for each tree and profile, `tools/js/instr.sh` (minimum of 3 runs, user-mode instructions), 3 runs of that for the spread, archMachine. Base: the tree of `main` before this change. glibc and jemalloc (`LD_PRELOAD`).

| Row                                              | glibc change | jemalloc change | cgu=1 glibc | cgu=1 jemalloc |
| ------------------------------------------------ | -----------: | --------------: | ----------: | -------------: |
| `interp_load loopfn 3000000`                     |      +0.000% |         +0.000% |     +0.000% |        +0.000% |
| `interp_load calls 1000000`                      |      +0.504% |         +0.501% |     +0.000% |        +0.000% |
| `gen_load next 100000`                           |      +0.528% |         +0.518% |     +0.244% |        +0.240% |
| `promise_load then 100000`                       |      +0.199% |         +0.195% |     +0.265% |        +0.260% |
| startup (`script_run`, `var x = 1;`)             |      +0.073% |         +0.022% |     -0.012% |        +0.015% |
| `parse_load full stmts 10` (glibc, default only) |      +0.003% |                 |             |                |

The spread of the 3 minimums is under 0.001% on the `interp_load`, `gen_load` and `promise_load` rows, and under 0.15% on the startup row. Two rows are over +0.5% in the default build: `interp_load calls` (+3,000,000 instructions, 3 for each call) and `gen_load next` (+900,000, 9 for each turn). Both are within +0.5% at `codegen-units=1` on both trees, so the rule allows them. Hypothesis: a layout effect of the new ops in the loop (the `calls` row does not move at `codegen-units=1`). The `gen_load` row keeps +0.24% at `codegen-units=1`; the `gen_kind` check in `Yield` and `RetGen` may cause it (to verify). This is the same layout effect as in A1 and top-level await.

## Not tested

- `BARE_GC_STRESS=1`: js2 has no such switch. The tests run under a constant collection trigger instead.
- `for await` and `yield*` over a string: js2 has no `String.prototype[Symbol.iterator]`. The built-ins lane owns it.
- The old engine and the browser pages: this change is in `bare-js2` only.
- Windows. Only archMachine ran.
- Wall time and memory.
