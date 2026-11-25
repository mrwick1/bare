# Error.prototype.stack, version 2

Status: ships as cost debt under rapid mode (owner, 2026-11-25). The cost rows are over the rule. Version 1 is in `/data/bare-tmp/teams-outlook-2026-11-25/error-stack-v1.diff`.

## Design

- The loop has no capture hook. A native call stores `regs` (register 0 of the frame) and `pc` in `es` (`publish!` in `interp.rs`: `native_call!`, `Op::New`, `NewSpread`, `CallSuper`). The code index comes from the function in the frame record. `Vm::run` stores the base address of its stack and puts the old values back when it ends. A reallocation of the stack clears `top` (`ensure!`, `compile_stub`).
- `error_ctor` and `Error.captureStackTrace` walk the frames from `cur` (`error_stack.rs`, `capture_here`). Every read of the stack is bounds-checked, because a published frame can be one that has gone away.
- `unwind` calls `stack_for_throw`: an error that has no frames yet (made by an op or by a native) gets the frames of the throw point.
- Frames are `(code index, pc)` pairs in one `u16` string, in the hidden own slot `\0stk`. The keys are interned once. The limit read is cached by shape (an absent `stackTraceLimit` is cached too).
- `stack` is one accessor on `Error.prototype`. The getter makes the text at the first read and keeps it as an own data property. `captureStackTrace` puts the same accessor as an own property on its target.
- An error with no frame (host-made, or swallowed by a native) reads as `Name: message`.

## Tests

- `error_stack_tests.rs` (6 tests) and `script_tests.rs` (83 tests) pass.
- test262 `built-ins/{Error,NativeErrors,AggregateError}` (212 files, checkout `/data/claude-tmp/test262`, runner built from this tree): base 157 pass, 28 fail, 27 skip. New: 165 pass, 20 fail, 27 skip. The 8 new passes are `Error/prototype/stack/getter-*` tests. The remaining `stack/` failures need the proposal rules (enumerable own property from the setter, `TypeError` for a non-object receiver, no text for an object without error data). Not built.
- `tools/fixtures/js/test262-js2-baseline.txt` is not changed.
- Miri: `cargo +nightly miri test -p bare-vex --lib -- error_stack_tests::` passes (6 tests). A first version that kept a pointer to the `Vec` failed Miri (stacked borrows). The pointer is now `regs`, the one the loop already uses.

## Over-rule rows (cost debt)

The owner session copies these rows into `cost-debt.md`. Every row below +0.5% is fine. Rows over +0.5% under both allocators: fib, objects, gen `next`, promise `then`, native-call loop, `new Map`, `forEach`, and the three error rows. A follow-up PR owns the fix.

## Cost (archMachine, `perf stat -e instructions:u`, minimum of 3, base = origin/main bd47d563)

Rows from `interp_load` and `script_run` on small JS files (`/data/claude-tmp/.../scratchpad/js`). The tree already builds the same at `-C codegen-units=1` (the counts are equal), so the cgu=1 exception does not apply.

| Row | glibc base | glibc new | glibc Δ | jemalloc base | jemalloc new | jemalloc Δ |
| --- | --- | --- | --- | --- | --- | --- |
| interp_load loopfn 3000000 | 776,506,986 | 785,512,373 | +1.160% | 779,913,079 | 788,917,818 | +1.155% |
| interp_load calls 1000000 | 550,556,437 | 550,562,328 | +0.001% | 553,930,822 | 553,935,124 | +0.001% |
| interp_load fib 27 | 240,929,779 | 243,474,959 | +1.056% | 244,334,447 | 246,881,876 | +1.043% |
| interp_load closures 1000000 | 1,742,907,542 | 1,743,486,459 | +0.033% | 1,746,266,952 | 1,746,845,409 | +0.033% |
| interp_load objects 1000000 | 700,841,725 | 714,795,231 | +1.991% | 704,214,910 | 718,200,258 | +1.986% |
| 100k `try { throw new Error }` | 145,676,107 | 222,640,126 | +52.832% | 149,053,139 | 226,305,462 | +51.829% |
| 100k `new Error` (no throw) | 132,489,656 | 197,823,349 | +49.312% | 135,869,724 | 201,432,398 | +48.254% |
| 100k `try { undefined() }` | 482,526,298 | 548,833,026 | +13.742% | 543,954,485 | 607,213,084 | +11.629% |
| 100k `new Map()` | 259,806,059 | 261,115,879 | +0.504% | 263,182,597 | 264,527,612 | +0.511% |
| 1M `Math.abs` native calls | 720,476,409 | 731,419,630 | +1.519% | 723,854,697 | 734,831,703 | +1.516% |
| 100k `forEach` with a callback | 326,956,851 | 329,865,062 | +0.889% | 330,337,230 | 333,244,399 | +0.880% |
| js2_gen_load next 100000 | 168,518,385 | 169,561,214 | +0.619% | 171,895,217 | 172,937,295 | +0.606% |
| js2_promise_load then 100000 | 151,796,034 | 153,502,727 | +1.124% | 155,163,895 | 156,870,425 | +1.100% |

## Where the cost comes from

- **Error rows (inherent).** One error now makes one `Str` and one extra property. Measured with a temporary switch: the frame walk is about 76 instructions per error, `Str::new` about 118, the property add about 199. The rest is the cache check and the setup. Version 1 cost about 1300 instructions per error. This version costs about 540. A stack that has frames cannot cost 0.
- **Hot rows (layout).** The deltas are exact multiples of the turns (loopfn +3,005,000 for 3,000,000 turns), so they are code changes in `Vm::exec`, not noise. They move when any line of `exec` changes: with the publish stubbed out, objects is +0.00% and loopfn stays +0.39%; with the `unwind` argument moved into `throw!`, loopfn is +1.5% and objects +2.6%; with the final publish (`regs` and `pc`) the table above applies. The earlier publish (`cur`, three words) gave loopfn +0.39%, objects +1.00%, native loop +2.36%. No variant put every row inside +0.5%.
- **Native loop.** 17 instructions for each native call is more than three stores. Hypothesis: the macro copies in the call arms change the spills of `ci`, `fp` and `pc`.

## Not tested

Windows VM, the stale `top` of a native that the loop did not publish for (a getter: it gives the last published frame), the Teams and Outlook journeys, deep stacks with `Error.stackTraceLimit = Infinity` in a nested machine, host-made errors, `captureStackTrace` on a frozen object.
