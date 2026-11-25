# Re-entry depth bounded by stack bytes, not by a count

Date: 2026-11-25. Base: main at 2f92860b.

## Problem

- `MAX_NEST = 100` in `crates/vex/src/reenter.rs` capped native-to-script re-entry (Promise executor, JSON reviver, `replace` callback, Proxy trap, for-of iterator, `yield*`, getters) at 100 levels. A second count, `proxy_depth >= 100` in `bi_proxy.rs`, did the same for Proxy traps.
- V8 limits stack space only. Real sites (Teams telemetry worker: 1DS SDK, dynamicProto, promise libraries) recurse finitely past 100 and got a false `RangeError`.
- A count that guards the stack is a design smell (CLAUDE.md). The cost per level is not constant: release about 5 to 7 KB, debug 150 to 440 KB.

## Options

Measured with the release build, `JSON.parse` reviver recursion (`zz_nest_mem`, a temporary example, not committed), 3 runs each, max RSS.

| Option | 1 000 levels | 5 000 levels | Runaway | Cost rows (promise/gen, 16 rows) |
|---|---|---|---|---|
| Main (`MAX_NEST = 100`) | RangeError | RangeError | ends at 101 levels, 6.2 MB | base |
| A. Byte budget: 64 MB of stack segments live together (chosen) | 14.1 MB (13.96 to 14.19) | 48.9 MB (48.79 to 49.33) | 11 499 levels, 105.1 MB (105.10 to 105.15) | mean +0.390%, max +0.480% |
| B. Count 10 000 (about 4.8 KB x count) | not run | 51.3 MB (51.16 to 51.60) | 10 001 levels, 94.0 to 98.6 MB | mean +0.334%, max not read |

- Both fit the 256 MB stop rule in release and both remove the false error.
- B is 0.06 points lower in mean instructions. That is inside the noise of code layout. Hypothesis, not verified.
- B ties memory to the frame size of one path. A debug build uses 150 to 440 KB per level: 10 000 levels would be 1.5 to 4.4 GB. A byte budget bounds memory for any frame size, so A is chosen.

## Change

- `Realm::grown` counts bytes of live stack segments. `Realm::grow` allocates a segment (`stacker::grow`, 4 MB) only when less than 512 KB is left, and adds it to `grown`.
- `Realm::stack_full` replaces the count: true when the next segment would pass `STACK_BUDGET` (64 MB release, 384 MB debug). The error text is the one of the register-stack limit: `maximum call stack size exceeded`.
- `call_function`, `construct_script`, `gen_nested` and the indirect-eval entry use it. `construct_script` and `gen_nested` now also grow the stack: before, only `call_function` did, and the count of 100 was the only guard for them.
- `proxy_op` (`bi_proxy.rs`) uses it in place of `proxy_depth >= 100` and runs its body on `grow`, so a chain of Proxies over Proxies does not use the thread stack. Its message was `maximum Proxy depth exceeded`; it is now the message above, as in Chrome.
- The first 4 levels (`nest + proxy_depth`) run on the thread stack, as before.
- The JS register-stack limit (`MAX_STACK`) is unchanged. A Promise executor runaway ends on it: the error is thrown when the executor frame is entered, so the Promise rejects with it.

## Cost

- `tools/js/cost_matrix.sh -a -g 'promise|js2_gen'`, base 2f92860b, arjun-e16, glibc, minimum of 3 runs of `instructions:u`. These rows are the ones in `cost_rows.txt` that re-enter script from a native. There is no `interp_load` row in `cost_rows.txt` (the example is bytecode only and has no nested calls), so no row of it was run.
- 16 rows, none over +0.5%. Mean +0.390%, largest `promise_all_load settled 10` +0.480%, smallest `js2_gen_load next 100000` +0.218%. Jemalloc rows were skipped by the script because no row was over 0.5% under glibc.
- Hypothesis: the growth is the added field and the check in the nested call path, plus code layout. Not verified.

## Tests

- New: `crates/vex/src/reenter_depth_tests.rs`, 10 tests, run in a thread with a 2 MB stack.
  - Finite recursion of 2 000 levels through a Promise executor, a JSON reviver, Proxy get traps and a custom for-of iterator. `yield*` runs 2 000 levels in release and 500 in debug (about 440 KB per level in debug, room for 868 in the budget).
  - Runaway recursion through each ends with a catchable `RangeError` with the message `maximum call stack size exceeded`. For the Promise executor the test reads the rejection reason after `drain_microtasks`.
- On main (`--release`): the 5 finite tests and the Proxy and Promise runaway tests fail. The three other runaway tests pass there, as the count already ended them.
- Pass in debug and in `--release`. `cargo test -p bare-vex --lib -- proxy generator promise reenter`: 68 pass. `t`: pass. `cargo check -p bare-vex -p bare`: pass.
- Miri: not run. `reenter.rs` has no `unsafe`.

## Not tested

- A live Teams run with this change.
- test262 counts: not run.
- Peak memory of paths other than the JSON reviver (Proxy trap, `yield*`), and of a small thread stack other than 8 MB.
- Debug build peak memory of a runaway: the budget is 384 MB there. The 256 MB stop rule was applied to release only.
- Jemalloc cost rows: skipped by the script.
