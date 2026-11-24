# 2026-11-21: js2 flat prelude (step 2 of `docs/design-js2-lazy-builtins.md`, design D2)

Branch `t3/js2-flat-prelude`. Base: the head of PR #848 (`22e4b7b2`), which is merged to main. This is PR 2 of 2 of the built-ins lane. PR 1: `2026-11-21-js2-string-iterators.md`.

## Result

The spike passed, so D2 is built (D1 was not needed). A script with no built-in costs 719,582 instructions at start, not 1,979,773 (-63.7%, glibc). The design expected about -1.0 M to -1.1 M; the gain is -1.26 M.

## What changed

- `prelude.js` is a flat text: top-level functions, no wrapper, never run as a whole. The names are the same as before, with a prefix for the public ones (`array_forEach`, `string_replace`, `helperAssign`).
- `GetPre bx` (new op, `bytecode.rs`, hot arm in `interp.rs`): reads `Realm::pre[bx]`. A slot that is `undefined` is made by `Realm::pre_make` and kept. Only the compiler of the prelude emits it (`Input::pre`, from `Unit::pre`): a name that no scope declares and that is in `PRE` reads a slot. An assignment to such a name is a compile error.
- `prelude_pre.rs` (new): the table `PRE`. A slot is a top-level function (a stub; the first call compiles it), a native that is no property of any object (the original `Object.keys`, `Object.defineProperty`, ...), or `%ArrayIteratorPrototype%`, which Rust builds. The first seven slots are the helpers that natives tail-call (`H_DEFINE` to `H_PAIRS`).
- `lazy.rs`: three new entry kinds: `Script(top)` (a prelude function as a method: a stub at the first lookup, no script runs), `SymSame` (`Array.prototype[Symbol.iterator]` is the entry `values`) and `Acc` (an accessor of two natives). The tables of `Array.prototype`, `Array`, `String.prototype` and `Object.prototype` use them.
- `Object.prototype.__proto__` is now two natives (`proto_get`, `proto_set` in `bi_object.rs`) with the same `name`, `length` and attributes. Before, it was two script functions.
- `script.rs`: `load_with` no longer compiles or runs the prelude. `LazyCompile::prelude_fn` makes a stub for a top-level function from the facts. The prelude unit (the facts, a copy of 45 entries) is made at the first stub. `compile_after` lost its `known` argument, and `parse_script_known` (`parse.rs`) is deleted: nothing else used them.
- `__def`, `__helper` and `__set` are gone (the three hidden globals and their natives).
- `prelude_facts.rs` (generated) has a new table `TOP`: the name and the fact id of each top-level function. The freshness test and the new test `every_prelude_slot_function_exists` keep it true.
- Test change in `lazy.rs`: the `EAGER` switch is replaced by `Realm::lazy_everything` (a test calls it after the load), because an eager install cannot make a prelude function before the compiler is set.

Core-lane files touched with a small diff, to review: `bytecode.rs` (one op), `interp.rs` (one hot arm, and `enter_helper!` reads the slot), `compile.rs`/`compile_expr.rs`/`compile_class.rs` (the `pre` flag, 12 lines), `parse.rs` (one function deleted). No `unsafe`, no trait change except one method with a default body (`LazyCompile::prelude_fn`), no GC change.

Not changed on purpose: the prelude functions still read `Object`, `Array`, `Symbol`, `TypeError` and the other globals by `GetGlobal`, and still call `indexOf`, `slice` and `bind` as properties, as before. Own-key order of `Array.prototype`, `String.prototype` and `Object.prototype` is now the order of first use (owner decision 2).

## Tests

`prelude_pre_tests.rs`, 10 tests (each case also runs under constant collection): name, `length` and attributes of every public prelude function; no hidden global and no property made before a lookup; the seven helpers give their results (`Object.assign`, `defineProperty` and `defineProperties` with accessors, spread and rest of an accessor, `Object.values` and `entries` with a getter, `indexOf`, `lastIndexOf` and `includes` on an array-like); a script that replaces `Object.keys` or `Object.defineProperty` does not change the helpers; the `__proto__` accessor (descriptor, primitives, `null`, a cycle, `undefined` receiver); the array iterator (prototype, tag, `next` name, wrong receiver, a replaced string iterator); identity of the functions at each lookup; calls before and after the first compile. Two old tests changed (`prelude_functions_are_...`, `a_lazy_compile_that_fails_...`) because the stubs are no longer made at start.

A before and after dump of the own keys, `name`, `length` and attributes of every prelude function (a script run on the PR 1 build and on this build) differs only in the order of own keys and in the three removed globals.

| Check | Result |
| --- | --- |
| `cargo test --locked -p bare-js2 --lib` | 742 passed, 0 failed |
| `cargo test --locked -p bare-js-api --features suite` | all ok (32, 152, 1, 5 passed) |
| `tools/js/test262_js2.sh -u` against the per-test list of PR 1 | 26,124 passes in both, 0 lost, 0 gained; the baseline file did not change |
| `cargo check -p bare -p bare-js -p bare-js-api` | ok |

## Measurements

Machine: archMachine, other sessions running. `tools/js/instr.sh` (minimum of 3, user mode), release build, default profile. Base: the build of PR 1 (`22e4b7b2`). Startup is `script_run` with `var x = 1;`.

| Row | glibc base | glibc new | change | jemalloc base | jemalloc new | change |
| --- | --- | --- | --- | --- | --- | --- |
| startup | 1,979,773 | 719,582 | -63.65% | 5,417,903 | 4,058,823 | -25.08% |
| `interp_load loopfn 3000000` | 887,299,237 | 875,298,202 | -1.35% | 890,700,230 | 878,667,912 | -1.35% |
| `interp_load calls 1000000` | 598,357,392 | 589,355,471 | -1.50% | 601,726,510 | 592,724,044 | -1.50% |
| `interp_load fib 27` | 263,607,127 | 260,427,410 | -1.21% | 267,007,308 | 263,827,922 | -1.19% |
| `interp_load closures 1000000` | 1,968,205,112 | 1,965,207,162 | -0.15% | 1,971,474,225 | 1,968,494,857 | -0.15% |
| `interp_load objects 1000000` | 715,132,699 | 712,131,603 | -0.42% | 718,529,442 | 715,528,694 | -0.42% |
| eval_loop | 91,238,052 | 86,981,863 | -4.66% | 94,678,629 | 90,328,624 | -4.59% |
| eval_loopfn | 50,066,279 | 47,812,831 | -4.50% | 53,504,903 | 51,149,068 | -4.40% |
| eval_calls | 73,985,235 | 71,129,779 | -3.86% | 77,422,025 | 74,473,117 | -3.81% |
| eval_fib | 5,275,668 | 3,985,997 | -24.45% | 8,712,625 | 7,355,819 | -15.57% |
| eval_closures | 14,247,727 | 12,845,621 | -9.84% | 17,703,361 | 16,225,808 | -8.35% |
| eval_objects | 68,894,465 | 67,005,599 | -2.74% | 72,414,822 | 70,490,138 | -2.66% |
| eval_concat | 13,155,130 | 11,697,489 | -11.08% | 16,594,199 | 15,043,594 | -9.34% |
| eval_throws | 30,985,315 | 29,455,559 | -4.94% | 34,436,076 | 32,813,765 | -4.71% |
| eval_webpack | 7,713,609 | 6,503,628 | -15.69% | 11,327,126 | 10,047,953 | -11.29% |
| bundle_like | 127,278,758 | 127,271,183 | -0.01% | 130,909,751 | 130,858,802 | -0.04% |

No row is above +0.5%, so no `codegen-units=1` run was needed. The micro rows (`interp_load`) do not call a prelude function: their gain is the hot loop layout after the new arm (a first build with `GetPre` as a cold op gave -0.00% to -0.14% on them). The rows that are not in the startup part change by layout only, not by design; do not count them as a gain.

Steady state and first call of the prelude functions (`script_run` of `a.<fn>(cb)` on a 10-element array; per call = (2,001 calls - 1 call) / 2,000):

| Function | per call base | per call new | change | 1 call, whole run, base | 1 call, whole run, new |
| --- | --- | --- | --- | --- | --- |
| `forEach` | 10,083 | 9,804 | -2.8% | 2,497,689 | 1,272,173 |
| `map` | 14,209 | 13,907 | -2.1% | 2,681,109 | 1,458,526 |
| `sort` (with `slice`) | 62,491 | 62,012 | -0.8% | 3,091,404 | 1,862,926 |
| `find` | 7,562 | 7,322 | -3.2% | 2,486,285 | 1,261,952 |

The first call of `forEach` costs 34 K more than before when startup is taken out (552,446 against 518,028): the five stubs (`forEach`, `obj`, `len`, `need`, `bound`) are made at the lookup now, about 3 K each, and the facts of the prelude are copied once (callgrind: `pre_fn` 14.6 K). The compile (388 K) is the same. With the 1.26 M saved at start, a page that calls `forEach` is 1.23 M cheaper. The design asked for +5 K at most for the first call; that figure did not count the stubs, which were made at start before. A page that uses 10 prelude functions pays about 30 K for their stubs.

Memory (`script_run` with `var x = 1;`, fresh copies of both binaries, 5 runs): peak RSS (`ru_maxrss`) 6,140 KiB to 5,900 KiB (-240 KiB, minimum); anonymous memory 356 KiB to 280 KiB (`smaps_rollup`); `malloc` peak 85,154 to 30,701 bytes (massif); page faults 348 to 278. The RSS of the file mapping moves by up to 500 KiB with the state of the page cache of the binary (an old copy and a fresh copy of the same binary differ by that much), so the minimum of fresh copies is the number to trust, and the anonymous part is the exact one.

## Not tested

- Windows, Miri and a collector stress run beyond the constant-collection tests. No `unsafe` was added, but a new op and a new hot arm touch the loop; the Windows VM run of the design (section 7) was not done.
- The `eval_*` and `bundle_like` rows were not measured at `codegen-units=1` (no row above +0.5%).
- A prelude function that a page calls through `Function.prototype.toString` (it prints `[native code]`, as before).
- Time to ready (wall time) and a load-test ramp.
- `typeof` and `delete` of a name of `PRE` inside the prelude are not compiled as slot reads (no prelude function uses them).
- The old peak RSS figure of the design (a hypothesis) was not re-measured on the base of PR 1 at a larger page.
