# js2 top-level await

Stacked on [A1](2026-11-21-js2-async-a1.md) (#840). Before this change, `module_parse` threw `Unsupported("top-level await")` and 213 module tests failed for that reason ([modules](2026-11-21-js2-modules.md)).

## What is built

- A module that has a top-level `await` compiles as an async function (`compile_module.rs`: `gen_kind = GK_ASYNC`, `AsyncInit` after the `phase` parameter, `ret_undef` at the end of phase 0). Both phases of such a module give a promise. `ModInfo.has_tla` tells the host. A module with no top-level `await` compiles as before.
- `module.rs` evaluates a graph as the spec says (16.2.1.5.3): `InnerModuleEvaluation` with the async evaluation order, the pending count of each module, the parent lists, the cycle roots, `ExecuteAsyncModule`, `AsyncModuleExecutionFulfilled` (`md_async_fulfilled`, with `GatherAvailableAncestors`) and `AsyncModuleExecutionRejected` (`md_async_rejected`). Two natives (`async_ok`, `async_err`) are the reactions of the promise of an async module. A sync module runs in the call of its parent, as before.
- `md_evaluate` gives one promise for each cycle root, and gives the same promise again when it is called again. A second graph that needs a module that still waits joins that wait.
- A dynamic `import()` of a graph that waits settles its promise when the evaluation ends (`md_finish`, natives `import_ok` and `import_err`).
- When a module is rejected, its own promise settles first and then the promises of its parents (spec step order). The first version settled the parents first. `language/module-code/top-level-await/rejection-order.js` found this.
- The shape of the activation object is cached and pinned in `Realm.pins` (`generator.rs`). The shape collector freed it before; a debug assert found this.

## Tests

`module_tests.rs`, 6 new tests, each case run plain and under constant collection:

- a wait in the module and the importer waiting for it; values, promises, loops and `try` around `await`;
- a sync sibling that runs before an async one ends, and the order of `d` after `b`;
- a rejected `await` that fails the graph, a sync throw, a late throw;
- a dynamic import of a module that waits;
- a second graph that joins a module that still waits (settled and rejected);
- the rejection order of two dynamic imports of one waiting module.

`compile_async_tests.rs` has one new test: a collection with no activation alive does not free the shape of the next one.

`cargo test --locked -p bare-js2 --lib` (archMachine, debug): 691 pass, 0 fail. `cargo test -p bare-js-api --features suite --test engines`: 152 pass, 0 fail.

## test262 (`test262_modules`, archMachine)

Same files and rules as the [modules](2026-11-21-js2-modules.md) run: `language/module-code` and `language/expressions/dynamic-import`, 1,604 files, `test262-js2-skip.txt` without its `allow:` lines, `test262_modules ENGINE ROOT SKIP LIST`. The test262 checkout is the one of the other runs. One run for each engine.

|      | old engine | js2 before (modules PR) | js2 now |
| ---- | ---------: | ----------------------: | ------: |
| pass |      1,090 |                     621 |   1,068 |
| fail |         48 |                     517 |      70 |

51 tests pass on the old engine and fail on js2. 29 tests pass on js2 and fail on the old engine. The runner prints the cause of each of the 51. All are features that js2 does not build yet:

- async generator: 32 (`not supported yet: an async generator`)
- `for await`: 13
- `with`: 6

## Cost

Base: tree `a6b61871` (before G2). Release builds, own `CARGO_TARGET_DIR` for each tree and profile, `tools/js/instr.sh` (minimum of 3 runs), archMachine. This change does not touch the interpreter loop, so only the rows that share code with it were run again.

| Row                          | Default change | cgu=1 change |
| ---------------------------- | -------------: | -----------: |
| `interp_load loopfn 3000000` |        +0.009% |      +0.009% |
| `interp_load calls 1000000`  |        +0.013% |      +0.014% |
| `gen_load next 100000`       |        -0.013% |      +0.290% |
| `promise_load then 100000`   |        +0.654% |      +0.118% |

The rows are over the whole stack (G2, G3, A1 and this change) because the base is the tree before G2. The `promise_load then` row is over +0.5% in the default build and within it at `codegen-units=1`: the rule allows this. It is the same layout effect as in A1.

## Not built and not tested

- Async generators and `for await`: not built. They cause 45 of the 51 lost module tests.
- `with` in a module: not built in js2 (6 tests).
- Not run: Windows, Miri, the browser, jemalloc, the full cost matrix, the rows of `parse_load` (the parser does not change).
- `tools/fixtures/js/test262-js2-baseline.txt` is not regenerated. The classic-script test262 run (`test262_js2.sh`) was not run again: the compiler for scripts does not change.
- The spread of the 3 runs of each cost row was not recorded.
