# 2026-11-21: js2 script API: completion value, eval, handlers, iterators

Branch `t3/js2-script-eval-iterators`, measured against `52b560d4`; rebased on `c68ee0ef` (adds RegExp) before the PR. Design: `docs/design-js2-host-api.md` section 11.

## What changed

- `ScriptMode { value, eval, scoped }` (`compile.rs`). The completion register exists only with `value`: an expression statement writes it, and `if`, loops, `switch` and `try` set it to `undefined` first (the old engine's rule, not the full UpdateEmpty). A script with no `value` compiles as before.
- `LazyCompile::script` takes a `ScriptMode`. This is a js2 internal trait, not the `bare-js-api` trait. No signature of `bare-js-api` changed.
- `crates/js2/src/api_script.rs` (a child module of `api.rs`): `run_script`, `compile_function`, `call_scoped`, `eval_global`, `eval_function`, `iter_open`, `iter_next`, `iter_fail`, `symbol_description`. The three intrinsics are real. `api.rs` has the dispatch lines and three fields of `ApiState`.
- Handler code: `GetName`, `SetName`, `TypeofName` are separate arms in `interp.rs` (they were the same arms as the global ops and nothing emitted them). They look in the global lexical bindings, then in the scope of `call_scoped`, then in the global object.
- Shared suite: 5 new tests (completion value, eval, handler scope, iterators, symbols and intrinsics), run on both engines, each with and without stress. The js2 harness now uses `Engine::run_script` instead of `eval_program` and `__result`.
- No `unsafe` added.

## Checks

- `cargo test --locked -p bare-js-api --features suite --test engines`: 68 pass (old, old_stress, new, new_stress).
- `cargo test --locked -p bare-js2 -p bare-js-api --lib`: pass (js2: 551 tests after the rebase on `c68ee0ef`, 6 are new, in `api_script_tests.rs`; the test262 and cost numbers were taken before that rebase).
- test262 js2 subset, `tools/js/test262_js2.sh -j4`, this tree against `52b560d4`: both 17,941 pass, 2,599 fail, 11,630 skip. 0 tests pass on main and fail here.
- Completion value on test262 (not in the committed tree): the test runner of `crates/js-diff` has no global `eval`, so the 72 `cptn-*` tests cannot run on js2 today. With a 6-line patch to `run_new` that defines `eval` from `Engine::eval_function` (kept in `/data/bare-tmp/r21/script-api/js-diff-eval.patch`, not committed: `crates/js-diff` is outside this lane), js2 passes 68 of the 72 `cptn-*` tests and 409 more tests in total, with 0 lost. The 4 that fail: `async-function/cptn-decl` (no async functions) and 3 `try/cptn-finally-*` (the old engine's rule gives the value of `finally`).
- Cost, js2 micro rows of `interp_load` (instructions:u, net of the N=0 run, M instructions, min of 3 inside `tools/js/instr.sh`), machine archMachine, base `52b560d4`:

  | row | base default | new default | change | base cgu=1 | new cgu=1 | change |
  |---|---|---|---|---|---|---|
  | loopfn | 266.500 | 272.500 | +2.25% | 282.500 | 282.500 | 0.00% |
  | calls | 571.501 | 585.501 | +2.45% | 578.501 | 578.500 | 0.00% |
  | fib | 1090.133 | 1106.289 | +1.48% | 1061.862 | 1064.555 | +0.25% |
  | closures | 365.701 | 366.902 | +0.33% | 365.962 | 365.561 | -0.11% |
  | objects | 143.393 | 142.927 | -0.33% | 143.328 | 142.728 | -0.42% |

  Three rows are over +0.5% at the default build and within +0.5% at `codegen-units=1`, so they pass by the standing rule (layout noise, as in #804; hypothesis: the interpreter match has 3 more arms).
- Compile path, `script_run` (instructions:u, min of 3): `var x = 1;` 1,968,885 to 1,970,836 (+0.10%); `bundle_like.js` 126,837,940 to 126,905,362 (+0.05%); `eval_webpack.js` 7,660,962 to 7,664,674 (+0.05%). At `codegen-units=1`: +0.07%, -0.31%, -0.06%.

## Not built

- Direct `eval` (a function that has one fails at its first call), a strict eval that keeps its `var`s local, the `Function` constructor.
- Completion value of `try`/`finally` per the spec (see above).
- `run_script` inside a host call is `Unsupported`, as in the old engine.
- The code of `compile_function` is kept by text with no bound. The generator prototype does not inherit from `%IteratorPrototype%`.

## Not tested

Windows; the browser; Miri; wall time on an idle machine; the cost rows of `bare-js` and `bare` (no file of those crates changed); the whole-browser cost.
