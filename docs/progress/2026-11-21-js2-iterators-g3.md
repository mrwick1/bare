# js2 iterator protocol and `yield*` (step G3 of `docs/design-js2-async.md`)

Stacked on [G2](2026-11-21-js2-generators-g2.md) (#831).

What is built.

- `iter.rs` (new): the protocol. `IterOpen` keeps the inline path for an array and a string (an index) and for a generator (kind `undefined`). Any other value goes through `Symbol.iterator`, `next`, `done` and `value` (`Realm::iter_record`, `iter_step`). `IterClose` calls `return()` for a generator and for a generic iterator. Its operand `c` is 1 on an exception: then the error of `return()` is dropped and the first error wins. On a `break`, `return` or end of the loop, an error of `return()` is the error, and a result that is no object is a `TypeError`.
- `for-of`, array destructuring and spread (`[...x]`, `f(...x)`, `new C(...x)`) use it. `Realm::iter_values` reads any iterable.
- `yield*` (`compile_gen.rs`, op `YieldStarStep`, cold): the loop of 14.4.14. A window of 5 registers holds the iterator record, the sent value and the mode. Each turn sends them to the inner iterator, and a resume goes back to the start of the loop, so `next()`, `throw()` and `return()` all pass to the inner iterator. A `throw()` to an iterator without `throw` closes it and throws a `TypeError`. A `return()` ends the delegation and returns the value (the `finally` blocks and the `return()` of each `for-of` around run). A generator, an array and a string are delegated to without the protocol (`gen_nested` and the index).
- `%IteratorPrototype%` is the prototype of `%GeneratorPrototype%`. It is the object that the host API makes (`iterator_prototype()` in `api_script.rs`, #821): it is made with the first generator function or the first ask of the host, whichever is first. #821 has its own `iter_record` for the host, so the function of this step is `iter_open_generic`.
- `Array.prototype[Symbol.iterator]` is `values`, and the array iterators have `[Symbol.iterator]() { return this }` (prelude), so `for (const [i, x] of a.entries())` and `[...a.keys()]` work. Strings have no `Symbol.iterator` method yet (a `for-of` over a string uses the inline path).

Two bugs of the code before this step, found by the new tests, are fixed here:

- A throw from a pattern that has an iterator closed the iterator and threw again from code at the end of the function, outside the `try` and outside the `for-of` that held the pattern. `try { var [a = f()] = it } catch (e) {}` was not caught on the base (`287805fe`: "FAIL d"). Inside a `try`, a `for-of` or the head of a `for-of`, the close and the throw now stay in the range of the handlers (`compile_pat.rs`, a jump over them). Outside, they stay at the end of the function.
- The registers of a `yield` operand were still in the root maps of the code after the `yield`. A resume does not bring them back, so a collection in a `finally` that `return()` ran could read a stale register. The fix is in #831 (commit `c931a206`) and the new test is in this PR too.

## Tests

`compile_gen_tests.rs` has 7 new tests, each run plain and under constant collection: generic `for-of` with `break`, `return`, `throw`, `continue`, labelled `break`, a `return()` that throws or returns a non-object, no `return`, a `next` that throws; values that are no iterable; destructuring and spread over generic iterators, generators, strings and arrays (when the iterator closes and when it does not); `yield*` (values, return value, sent values, registers that the outer keeps); `yield*` with `throw()` and `return()` (catch in the inner and the outer, `finally` order, no `return`, no `throw`, array, no iterable); the iterator prototype; the temporaries of a `yield`.

`cargo test --locked -p bare-js2 --lib` (archMachine, debug build): 570 pass, 0 fail. `cargo test --locked -p bare-js-api --features suite`: 32 + 84 + 1 + 5 pass. The goldens of `compile_tests.rs` show the new operand of `IterClose`.

### test262 (`tools/js/test262_js2.sh`, archMachine, 470 directories)

Base: G2 (`c931a206`). 0 lost passes, 3,612 new passes.

| Directory | G2 pass / fail | Now pass / fail |
| --- | ---: | ---: |
| `language/expressions/yield` | 21 / 42 | 57 / 6 |
| `language/statements/for-of` | 587 / 118 | 661 / 44 |
| `language/statements/for-of/dstr` | 493 / 76 | 542 / 27 |
| `language/expressions/assignment/dstr` | 316 / 52 | 347 / 21 |
| `language/statements/generators` | 236 / 28 | 249 / 15 |
| `language/expressions/generators` | 259 / 28 | 272 / 15 |
| `language/destructuring` | 15 / 2 | 15 / 2 |

The remaining failures in these directories:

- 4 tests of `yield*` (`star-rhs-iter-*-res-done-no-value`, `star-iterable`): the spec gives the inner result object to the caller as it is and does not read `value`. `YieldStarStep` reads `value` and `Yield` makes a new result object. A raw yield needs a mode in the `Yield` op and the unpack in `for-of`; I did not build it.
- Tests that change `Array.prototype[Symbol.iterator]` or `%ArrayIteratorPrototype%` (`*-array-prototype.js`, `Array.prototype.entries`): an array in a `for-of`, a pattern or a spread is read directly and does not look the method up. The same holds for `%GeneratorPrototype%.next`.
- `eval`, `Proxy`, `with`, `arguments` as a name, mapped `arguments`, and the dead zone of the head of a `for-of` (they fail on plain functions and loops too).

## Cost

Base: G2 head `c931a206`. Release builds, own `CARGO_TARGET_DIR` for each tree and for each profile, `tools/js/instr.sh` (minimum of 3 runs), archMachine. The default build is the release profile of the workspace (16 code generation units). The `codegen-units=1` build is `CARGO_PROFILE_RELEASE_CODEGEN_UNITS=1`.

| Row | Base (default) | Branch (default) | Change | Base (cgu=1) | Branch (cgu=1) | Change |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| `interp_load loopfn 3000000` | 848,411,845 | 887,411,857 | +4.597% | 845,399,447 | 839,399,196 | -0.710% |
| `interp_load calls 1000000` | 588,468,751 | 599,468,888 | +1.869% | 562,448,286 | 561,448,667 | -0.178% |
| `interp_load fib 27` | 260,541,875 | 261,813,693 | +0.488% | 249,085,381 | 248,450,099 | -0.255% |
| `interp_load closures 1000000` | 1,949,846,913 | 1,971,417,221 | +1.106% | 1,924,433,785 | 1,912,434,176 | -0.624% |
| `interp_load objects 1000000` | 716,342,625 | 728,277,346 | +1.666% | 717,256,036 | 719,255,904 | +0.279% |
| `js2_gen_load next 100000` | 170,472,998 | 171,608,224 | +0.666% | 165,060,762 | 163,863,618 | -0.725% |
| `js2_gen_load host 100000` | 101,589,941 | 101,990,349 | +0.394% | 98,963,980 | 98,166,891 | -0.805% |

At `codegen-units=1` no row is over +0.5% (the largest is +0.279%), so the rule holds (a row over +0.5% passes when it is within +0.5% at `codegen-units=1`). The default build moves by up to +4.6% on rows that run no op that this step changed. Two builds of the same source moved `loopfn` by +1.4% and +4.6%, so this is the layout of the code of `exec` (hypothesis: not checked with a disassembly). What this step puts in the hot loop: one arm (`IterOpen`) calls `iter_record` out of line; `IterNext` calls `iter_step_any` out of line; `IterClose` and `YieldStarStep` are in the cold copy. I did not run the jemalloc counts or the full cost matrix.

New paths (default build, 100,000 turns, total instructions of the script, base = G2 where the script ran):

| Script | Instructions | Per turn |
| --- | ---: | ---: |
| `for (v of g())`, generator, 5 locals (as in G2) | 137,969,003 (G2: 135,662,082) | 1,380 (G2: 1,357) |
| `for (v of it)`, a hand-written iterator object | 225,874,780 | 2,259 |
| `for (v of outer())` with `yield* inner()` | 248,908,949 | 2,489 |
| `[...g()]` | 113,395,752 | 1,134 |
| `for (v of array)` | 151,983,461 (G2: 151,578,907) | 1,520 (the loop fills the array too) |

The generator `for-of` is +1.7% against G2 (23 instructions per turn): the extra call of `iter_step_any`. It still runs the generator on a nested machine. A plan: push the generator frame in the same machine, and let `RetGen` of a done generator return to the jump of `IterNext`. This removes the machine and the result mapping (hypothesis: about half of the 1,380). Not built.

## Not tested

Windows. Miri. The browser and `bare` journeys. Wall time. The jemalloc counts and the full cost matrix. A `for-of` over a string whose `String.prototype[Symbol.iterator]` is changed (strings have no such method). `yield*` over a Proxy. The baseline file `tools/fixtures/js/test262-js2-baseline.txt` is not regenerated (other PRs change it too).
