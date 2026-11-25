# V8 names in Vex TypeError messages

Date: 2026-11-25. Base: main at b5be998e.

## Problem

- Vex threw `not a function`, `not a constructor`, `cannot read properties of undefined or null` and `cannot set properties of undefined or null` with no name.
- V8 says `o.f is not a function`, `Cannot read properties of undefined (reading 'length')`, `X is not a constructor` and `Cannot set properties of null (setting 'a')`. Sites sometimes match on this text. Each unnamed error in the Teams and Outlook bring-up took a probe build to locate.

## Options

| Option | Memory per code | Instructions on the hot path | Notes |
|---|---|---|---|
| A. Source span of the callee in a side table, made at compile time | one `(pc, start, end)` entry per call site, plus the source text kept alive | none | needs the source text for the life of the code (today it is dropped); a minified bundle is large |
| B. Scan the bytecode before `pc` at the throw (chosen) | 0 | 0 | prints from the op that wrote the register (`GetField`, `GetGlobal`, `Mov`, ...); cannot name a local variable |

- B costs nothing until a throw. A costs memory for every call site of every function, thrown or not. B is chosen.
- Limit of B: a register has no name. A local variable or a context slot prints as `(intermediate value)`, as V8 prints what it cannot name. `o.run()` on a parameter `o` gives `(intermediate value).run is not a function`.
- The scan takes the last writer of the register in word order. This holds for the code that the compiler emits for a call, `new` and `for-of`. Hypothesis: a callee that a branch wrote (`(c ? f : g)()`) can name the wrong arm. Not tested.

## Change

- `crates/vex/src/error_names.rs`: `Realm::expr_text` (decode words up to `pc`, find the writer, print it; depth 6) and `Realm::key_name`. Both are `#[cold]`.
- `interp.rs`: `Call`, `CallU`, `CallSpread`, `New`, `NewSpread` and the `noctor` check name the callee. `GetElem`, `ToKey` and `SetElem` name the key. `IterOpen` throws `x is not iterable` through the new `Realm::iter_method` (`iter.rs`); `iter_open_generic` uses it too, with its old text.
- `interp_rt.rs`: `Cannot read properties of ...` and `Cannot set properties of {undefined|null} (setting 'k')`.
- Not changed: calls from natives (`reenter.rs`, `api.rs`, `builtins.rs`), `enter_helper`, super calls, destructuring and spread iteration (`ArraySpread`, `AIterOpen`).
- Tests that asserted the old text now assert the V8 text: `script_tests.rs` (2), `interp_tests.rs` (2).

## Cost

- Base b5be998e and new, release, arjun-e16, glibc, minimum of 3 runs of `instructions:u`.
- `cost_matrix.sh -a -g 'promise_all_load|js2_'`: 18 rows, none over. Mean -0.158%, largest `js2_parse_load full stmts 1` +0.002%. Jemalloc rows skipped by the script.
- `interp_load` (N=100000, `fib` N=25): objects 71186138 to 70351444 (-1.17%), calls 55874963 to 55774268 (-0.18%), closures 160520304 to 160220267 (-0.19%), loopfn 27135060 to 26635504 (-1.84%), fib 93300348 to 92450409 (-0.91%).
- A first version formatted the message inline with `err!`. It raised `calls` by +1.25% and `loopfn` by +1.48%. Moving the formatting into `#[cold] #[inline(never)]` functions (`named_error`, `key_error`) removed it. Hypothesis: the inline `format!` changed the layout of the loop. Not verified.
- The shrinks are code layout, not a gain from the change.

## Tests

- New: `crates/vex/src/error_names_tests.rs`, 7 tests. All 7 fail on main and pass now.
- `cargo test --locked -p bare-vex --lib -- script_tests:: interp_tests:: error_names_tests:: iter`: 67 pass. `t`: pass. `cargo check -p bare-vex -p bare`: pass.

## Not tested

- A live Teams or Outlook run.
- test262 counts: not run. The baseline file is unchanged.
- A callee written in a branch; a callee in a `with` or `eval` scope.
- Miri: not run. The change adds no `unsafe`.
