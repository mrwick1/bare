# 2026-11-21: js2 `String.prototype[Symbol.iterator]` and `Array.from` over iterables

Branch `t3/js2-string-iterators-lazy-prelude`. Base: origin/main 804ec986. This is PR 1 of 2 of the built-ins lane. PR 2 is step 2 of `docs/design-js2-lazy-builtins.md` (flat prelude).

## What changed

- `crates/js2/src/bi_string_iter.rs` (new): `String.prototype[Symbol.iterator]` and `%StringIteratorPrototype%` (`next`, `@@toStringTag` "String Iterator"). The iterator walks code points: a lone surrogate is one value, a valid pair is one value. State is two hidden slots in an object of kind 7 in the `F_COLL` bits (1 to 6 are `Map` and `Set`). The prototype and the shape are made at the first call, so a realm that never asks pays nothing.
- `lazy.rs`: a new entry kind `Kind::Sym` for a method with a well-known symbol key. The symbol entries that are not made are in `Lazy::syms`, so a symbol lookup on an object with a table costs one short scan. (A first version scanned all tables for each symbol lookup: `bundle_like` was +1.7%.)
- `prelude.js`: `Array.from` uses `Symbol.iterator` (a `Set`, a `Map`, a generator, a string by code points) and closes the iterator when the callback throws. An array with the original `values` and an array-like keep the old path. The `%ArrayIteratorPrototype%` now inherits `%IteratorPrototype%` and has the tag "Array Iterator" (before: an object with its own `@@iterator`).
- `tools/fixtures/js/test262-js2-subset.txt`: four directories added: `ArrayIteratorPrototype`, `MapIteratorPrototype`, `SetIteratorPrototype`, `StringIteratorPrototype` (infra lane file, 4 lines). The baseline is updated with `test262_js2.sh -u`.

Already working before this PR (probed, not changed): `for-of`, spread and destructuring over a string primitive (the loop reads a string directly), `Array.prototype.keys/values/entries`, `Map` and `Set` iteration with `for-of`, `yield*` over a string, `for await` over a string.

## Tests

`crates/js2/src/bi_string_iter_tests.rs`, 8 tests, written before the code. Each case runs normally and under constant collection: descriptor, `name`, `length`, not a constructor; code points and lone surrogates; `next` and exhaustion; the prototype chain (`%IteratorPrototype%` is shared with the array iterator and the generator prototype); a changed method; `Array.from` over iterables, with a callback, closing on a throw; `for await` and `yield*`.

| Check | Result |
| --- | --- |
| `cargo test --locked -p bare-js2 --lib` | 731 passed, 0 failed |
| `cargo test --locked -p bare-js-api --features suite` | all ok (32, 152, 1, 5 passed) |
| `tools/js/test262_js2.sh`, per-test lists of this branch and of a main build (804ec986), same subset files except the 4 new directories | 0 passes lost; 56 passes gained: 37 in the new directories (`StringIteratorPrototype` 7 of 7, `MapIteratorPrototype` and `SetIteratorPrototype` 11 of 11 each), `Array/from` +14, `String/prototype` +5 |

## Measurements

Machine: archMachine, other sessions running. `tools/js/instr.sh` (minimum of 3 runs, user mode), release build, default profile, base worktree at 804ec986. Rows of the code that changed: the js2 micro rows, the eval programs and `bundle_like` (`crates/js2/examples/data`), run with `script_run`, and startup (`var x = 1;`).

| Row | glibc base | glibc new | change | jemalloc base | jemalloc new | change |
| --- | --- | --- | --- | --- | --- | --- |
| startup | 1,972,055 | 1,979,785 | +0.39% | 5,411,345 | 5,416,604 | +0.10% |
| `interp_load loopfn 3000000` | 887,298,417 | 887,299,777 | +0.00% | 890,697,866 | 890,699,904 | +0.00% |
| `interp_load calls 1000000` | 598,355,832 | 598,357,629 | +0.00% | 601,725,260 | 601,726,670 | +0.00% |
| `interp_load fib 27` | 263,606,613 | 263,607,870 | +0.00% | 267,005,982 | 267,007,636 | +0.00% |
| `interp_load closures 1000000` | 1,968,205,330 | 1,968,206,473 | +0.00% | 1,971,471,848 | 1,971,474,471 | +0.00% |
| `interp_load objects 1000000` | 715,130,723 | 715,133,067 | +0.00% | 718,527,672 | 718,528,573 | +0.00% |
| eval_fib | 5,268,888 | 5,275,124 | +0.12% | 8,706,612 | 8,713,542 | +0.08% |
| eval_closures | 14,241,316 | 14,248,024 | +0.05% | 17,697,164 | 17,702,778 | +0.03% |
| eval_webpack | 7,711,520 | 7,715,248 | +0.05% | 11,323,161 | 11,325,783 | +0.02% |
| bundle_like | 127,106,998 | 127,272,585 | +0.13% | 130,741,710 | 130,910,526 | +0.13% |

The other eval rows (`loop`, `loopfn`, `calls`, `objects`, `concat`, `throws`) are +0.00% to +0.05%. No row is above +0.5%, so no `codegen-units=1` run was needed.

The startup cost is the prelude: it grew by about 1.4 KB of text, and the compile of the wrapper scans the text for each variable that the wrapper holds. A first version with one more wrapper variable cost +17,000 and one with two cost +47,000 (a native captured at the start). The final version adds no wrapper variable and no nested function: +7,700 against a moving base (+3,400 before the merge of #845 and #846).

## Not tested

- Windows, Miri and a collector stress run beyond the constant-collection tests. No `unsafe` and no trait change.
- Peak RSS and time to ready.
- `arguments[Symbol.iterator]` is not added: an arguments object has no own `@@iterator`, so 8 tests of `ArrayIteratorPrototype/next` (`args-*`) fail. A per-call property would add cost to each `arguments` object; it needs a shape change in the core lane. `for-of` and spread over `arguments` work.
- The `Map` and `Set` iterator prototypes have their own `@@iterator` and do not inherit `%IteratorPrototype%` (made at the first generator, so an eager link would add startup cost). `ArrayIteratorPrototype/next/non-own-slots.js` fails: the array iterator keeps its state in own properties.
- A script that replaces `String.prototype[Symbol.iterator]` is not seen by `for-of` over a string primitive (the loop reads the string directly, as it does for an array).
