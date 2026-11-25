# Vex gets `WeakRef` and `FinalizationRegistry`

Date: 2026-11-25. Base: main 59a40269.

## Problem

- Outlook web threw `ReferenceError: WeakRef is not defined` 4 times (timer and custom-element callbacks). Teams threw it once.
- Vex had neither global.

## Change

- New file `crates/vex/src/bi_weakref.rs`. Both globals are lazy (`factory`, as `TextEncoder`). A page that does not name them pays nothing.
- `WeakRef`: an object of the new kind `K_WEAKREF` with one hidden slot. The slot holds a `WeakTab` (the ephemeron table of `WeakMap`) with one entry: key and value are the target. The entry does not keep the target. The collector clears it when the target dies. `deref` reads the key of the live entry (`WeakTab::first_key`, new).
- `FinalizationRegistry`: kind `K_FINREG`. The cleanup callback is never called (the spec allows it). `register` keeps neither the target nor the held value. For `unregister`, the registry keeps a `WeakTab` keyed by the unregister token, with the registration count as value. It is bounded by the live tokens.
- Argument checks follow the spec: `new` required, target and token must be objects, target and held value must differ, the callback must be callable.
- `typeof` is `function`; `length` is 1 for both.

## Deviations and gaps

- Targets and tokens must be objects. A symbol throws `TypeError`, as for `WeakMap` keys. The spec also allows non-registered symbols. 4 test262 tests fail for this reason.
- KeepDuringJob is not done. A target that only a `WeakRef` names may be collected inside the job that made the ref. A fix needs a rooted per-job list that the microtask checkpoint clears.
- New tests live in `bi_weakref_tests.rs` and are mounted from `bi_weakref.rs` (the pattern of `bi_collections.rs`), not from `lib.rs`.
- The tests were written together with the code, not before it.

## Tests

`t` runs 7 new tests (live deref, deref after death, errors, subclass, registry errors, return values, no retention) and the weak-table tests. All pass.

test262 (checkout at `/data/claude-tmp/test262`, sloppy mode only, a small shell loop with `assert.js`, `sta.js` and the listed includes, because the old runner no longer exists): `built-ins/WeakRef` and `built-ins/FinalizationRegistry`, 76 files: 70 pass, 6 fail, 0 skipped. The 6: 4 symbol targets or tokens, 2 need `$262` (`proto-from-ctor-realm`). Not comparable with the baseline file.

## Cost

Release builds of base and this change, `tools/js/cost_matrix.sh -a -g 'js2_'` (the only Vex rows in `cost_rows.txt`; there is no `interp_load` row), glibc, instructions:

| row | base | new | change |
|---|---:|---:|---:|
| js2_gen_load next 100000 | 168,477,580 | 168,477,829 | +0.000% |
| js2_lex_load small 1 1 | 114,997,119 | 114,996,778 | -0.000% |
| js2_parse_load full stmts 1 | 9,708,874 | 9,708,803 | -0.001% |
| js2_promise_load then 100000 | 151,798,299 | 151,798,444 | +0.000% |

Start cost (realm install, one more lazy table): `interp_load fib 27` 240,675,757 → 240,676,208 (+0.0002%); `script_run eval_calls.js`-class run 123,530,983 → 123,530,892. No row over the rule. Jemalloc was skipped (no row over on glibc).

Not tested: Miri (no unsafe module changed), browser journeys on Outlook or Teams, Windows, the full test262 baseline, a collection in the middle of a job.
