# Delete of an optional chain

Date: 2026-11-24.

## Problem

Vex did not compile `delete a?.b`. The compile failed with "not supported yet: delete of an optional chain". The Microsoft Teams login code uses this form. Its login attempt failed, and the page stayed on the loading screen.

## Change

`compile_expr.rs`, `delete`:

- A `Chain` node sets the short-circuit label and compiles the inner `delete`. A nullish link jumps to the label, and the result is `true` (ECMA-262, 13.5.1.2).
- A `Member` or `Index` link with `optional` checks its base for nullish before it evaluates the key. So `delete n?.[f()]` does not call `f`.

## Cost gate

- `interp_load`: not run. Its rows are hand-written bytecode; this change touches only the compiler.
- `cost_matrix.sh -g 'js2_'`: 4 rows, glibc mean −0.006%, 0 over the rule.

## Tests

- New: `compile_delete_chain_tests.rs` (2 tests): results and side effects for a present base, a nullish base, a longer chain and a frozen property; a strict-mode `TypeError` on a frozen property.
- Changed: 2 tests in `script_tests.rs` used `delete ({})?.x` as their unbuilt construct. They now check that it compiles. No construct that source code can reach gives "not supported yet" now, so the unbuilt path has no source-level test case. Hypothesis: the internal cases (a function that the pre-parser did not read, an assignment to a prelude slot) still use it.
- `cargo test -p bare-vex`: all pass.

## Not tested

- test262 counts.
- The full cost matrix and the journeys.
- Windows.
