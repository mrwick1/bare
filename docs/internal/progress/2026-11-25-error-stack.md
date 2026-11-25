# Error.prototype.stack, version 2

Status: ships as cost debt under rapid mode (owner, 2026-11-25). The cost rows are over the rule. Version 1 is in `/data/bare-tmp/teams-outlook-2026-11-25/error-stack-v1.diff`.

## Design

- The loop has no capture hook. A native call stores `realm.cur = (ci, fp, pc)` (`publish!` in `interp.rs`: `native_call!`, `Op::New`, `NewSpread`, `CallSuper`). `Vm::run` stores the address of its stack in `es.stk` and puts the old `stk` and `cur` back when it ends.
- `error_ctor` and `Error.captureStackTrace` walk the frames from `cur` (`error_stack.rs`, `capture_here`). Every read of the stack is bounds-checked, because a published frame can be one that has gone away.
- `unwind` calls `stack_for_throw`: an error that has no frames yet (made by an op or by a native) gets the frames of the throw point.
- Frames are `(code index, pc)` pairs in one `u16` string, in the hidden own slot `\0stk`. The keys are interned once. The limit read is cached by shape (an absent `stackTraceLimit` is cached too).
- `stack` is one accessor on `Error.prototype`. The getter makes the text at the first read and keeps it as an own data property. `captureStackTrace` puts the same accessor as an own property on its target.
- An error with no frame (host-made, or swallowed by a native) reads as `Name: message`.

## Tests

- `error_stack_tests.rs` (6 tests) and `script_tests.rs` (83 tests) pass.
- test262 `built-ins/{Error,NativeErrors,AggregateError}` (212 files, checkout `/data/claude-tmp/test262`, runner built from this tree): base 157 pass, 28 fail, 27 skip. New: 165 pass, 20 fail, 27 skip. The 8 new passes are `Error/prototype/stack/getter-*` tests. The remaining `stack/` failures need the proposal rules (enumerable own property from the setter, `TypeError` for a non-object receiver, no text for an object without error data). Not built.
- `tools/fixtures/js/test262-js2-baseline.txt` is not changed.
- Miri not run (one `unsafe` in `capture_here`: the read of `es.stk`).

## Over-rule rows (cost debt)

The owner session copies these rows into `cost-debt.md`. Every row below +0.5% is fine. Rows over +0.5% under both allocators: fib, objects, gen `next`, promise `then`, native-call loop, `new Map`, `forEach`, and the three error rows. A follow-up PR owns the fix.

## Cost (archMachine, `perf stat -e instructions:u`, minimum of 3, base = origin/main bd47d563)

Rows from `interp_load` and `script_run` on small JS files (`/data/claude-tmp/.../scratchpad/js`). The tree already builds the same at `-C codegen-units=1` (the counts are equal), so the cgu=1 exception does not apply.

| Row | glibc base | glibc new | glibc Δ | jemalloc base | jemalloc new | jemalloc Δ |
| --- | --- | --- | --- | --- | --- | --- |
| interp_load loopfn 3000000 | 776,506,257 | 779,511,310 | +0.387% | 779,911,792 | 782,916,996 | +0.385% |
| interp_load calls 1000000 | 550,555,661 | 547,561,294 | -0.544% | 553,929,031 | 550,935,335 | -0.540% |
| interp_load fib 27 | 240,928,439 | 243,158,009 | +0.925% | 244,332,483 | 246,562,805 | +0.913% |
| interp_load closures 1000000 | 1,742,906,695 | 1,740,914,275 | -0.114% | 1,746,266,199 | 1,744,273,833 | -0.114% |
| interp_load objects 1000000 | 700,840,464 | 707,859,912 | +1.002% | 704,214,009 | 711,265,308 | +1.001% |
| 100k `try { throw new Error }` | 145,675,257 | 210,839,354 | +44.732% | 149,051,916 | 214,472,346 | +43.891% |
| 100k `new Error` (no throw) | 132,489,026 | 186,223,516 | +40.558% | 135,869,028 | 189,816,318 | +39.705% |
| 100k `try { undefined() }` | 482,525,667 | 549,332,621 | +13.845% | 543,954,121 | 607,713,325 | +11.721% |
| 100k `new Map()` | 259,804,534 | 262,218,374 | +0.929% | 263,181,418 | 265,627,941 | +0.930% |
| 1M `Math.abs` native calls | 720,476,095 | 737,484,686 | +2.361% | 723,854,023 | 740,896,824 | +2.354% |
| 100k `forEach` with a callback | 326,956,678 | 329,866,511 | +0.890% | 330,337,421 | 333,245,487 | +0.880% |
| js2_gen_load next 100000 | 168,517,461 | 169,625,897 | +0.658% | 171,894,223 | 173,001,918 | +0.644% |
| js2_promise_load then 100000 | 151,796,151 | 153,602,389 | +1.190% | 155,163,194 | 156,969,889 | +1.164% |

## Where the cost comes from

- **Error rows (inherent).** One error now makes one `Str` and one extra property. Measured with a temporary switch: the frame walk is about 76 instructions per error, `Str::new` about 118, the property add about 199. The rest is the cache check and the setup. Version 1 cost about 1300 instructions per error. This version costs about 540. A stack that has frames cannot cost 0.
- **Hot rows (layout).** The deltas are exact multiples of the turns (loopfn +3,005,000 for 3,000,000 turns), so they are code changes in `Vm::exec`, not noise. They move when any line of `exec` changes: with the publish stubbed out, objects is +0.00% and loopfn stays +0.39%; with the `unwind` argument moved into `throw!`, loopfn is +1.5% and objects +2.6%; with a one-word publish (`fp << 32 | pc`), loopfn +1.2%, objects +2.0%, native loop +1.5%, `new Map` +0.4%. No variant put every row inside +0.5%.
- **Native loop.** 17 instructions for each native call is more than three stores. Hypothesis: the macro copies in the call arms change the spills of `ci`, `fp` and `pc`.

## Not tested

Windows VM, the Teams and Outlook journeys, deep stacks with `Error.stackTraceLimit = Infinity` in a nested machine, host-made errors, `captureStackTrace` on a frozen object.
