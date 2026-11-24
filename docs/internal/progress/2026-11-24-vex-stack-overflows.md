# Two pages that crashed the browser with a stack overflow

Date: 2026-11-24. Base: main e796e970.

## Problem

Two tests aborted the whole process on Vex with "thread has overflowed its stack". A page could crash the browser this way. Both tests passed on the old engine, so #1001 marked them ignored.

- `browser_elements_tests::indirect_eval_keeps_the_call_budget_surface`: `function recur(){return window.eval('recur()')} recur()`. Each level is a host call into Vex that compiles and runs a new script. Each level puts a frame of `Vm::run` and the native above it on the Rust stack. The frames are big, and the thread ran out of stack before the nest limit (`MAX_NEST` = 100) fired.
- `browser_range_tests::a_range_cut_in_a_very_deep_tree_is_a_limit_not_a_stack_overflow`: `deleteContents` on a range across 6000 nested `div`s, on an 8 MiB thread. `cut` in `browser_range.rs` called itself once per partly selected node, so once per tree level.

## Fix

- `browser_range.rs`: `cut` runs on a work stack (`Work::Cut`, `Work::Tail`) and not on recursion. The depth of the tree does not use the Rust stack. The order of the changes stays the order of the standard.
- `crates/vex/src/reenter.rs`: above nest level 4, a nested call runs in `stacker::maybe_grow` (red zone 512 KiB, grows by 4 MiB). `stacker` was already a dependency of Vex.
- Indirect eval at `MAX_NEST` ends the run with `Abort::Limit("the call depth")`. `eng_new.rs` reports it as `LimitKind::CallDepth`. Script recursion keeps its catchable `RangeError`.

Rejected option: a lower `MAX_NEST` for eval. It would hide the frame size and still fail on a small thread.

## Tests

- Both tests pass. Their `#[ignore]` marks are removed.
- `t` on the change: see the PR.

## Cost rows (`cost_matrix.sh -g`, archMachine, one run per row)

The Vex micro rows (`js2_*_load`) did not change (+0.000% to +0.020%).
The `promise_all_load` rows grew by +0.6% to +1.0%. They cross the host call path on each promise. These rows are over the rule under both allocators:

| Row | glibc | jemalloc |
| --- | --- | --- |
| promise_all_load all 10 | +0.690% | +0.645% |
| promise_all_load any 10 | +0.814% | +0.754% |
| promise_all_load anyerr 10 | +0.718% | +0.669% |
| promise_all_load many 10 | +1.025% | +0.937% |
| promise_all_load race 10 | +0.865% | +0.796% |
| promise_all_load settled 10 | +0.603% | +0.572% |
| promise_all_load thenable 10 | +0.653% | +0.613% |

Hypothesis: the cost is the closure and the `nest` test in `Realm::call` on each host call. The `range_load` rows failed to run (4 rows) and were not measured.

## Not tested

- Windows.
- The `codegen-units=1` check of the rows over the rule.
- The `range_load` cost rows.
