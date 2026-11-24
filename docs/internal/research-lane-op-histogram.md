# Research: what the bytecode lane executes, and what a narrow JIT could remove

Date: 2026-11-17 (second round). Base: `c59afb9a` (merge of labels, for-in, for-of; 99.7% of calls on the lane) plus the `bc-hist` instrument. No engine behavior changed.
Round one (base `f2adb72194`, 37% of calls on the lane) is kept as the "old" column. Its tables are replaced.
Purpose: input for a design doc on a narrow template JIT. A JIT removes dispatch and decode work only. So this note measures (1) which operations run, (2) which pairs and functions are hot, (3) how much cost is outside the dispatch.

## Method

- Machine: AMD Ryzen 9 5900HX, 16 threads, Linux. Other agents ran at the same time. Only counts are used (operation counts and instruction counts), never wall time.
- Instrument: cargo feature `bc-hist` of `bare-js` (off by default). With it on, the loop counts executed operations, consecutive pairs and triples, operations and entries per function, and misses of the `GetField` and `SetField` inline caches. It prints `bchist ...` lines when the interpreter drops. `tools/js/bc_hist.sh [-e EXAMPLE] ARGS` builds (own target dir) and runs it.
- Loop ops: on the measured branch, a `Throw` word with `b` from 1 to 5 was a loop op of `for-in` and `for-of`. The instrument counted it under its own name (`IterOpen`, `ForInOpen`, `IterNext`, `IterClose`, `IterCloseCatch`). Round one would have counted these as `Throw`. The `bc_rare` ops (destructuring, spread, `GetEnvX`) have their own opcodes and count under their names. A `Wide` prefix never reached the loop: code with a wide form stayed on the tree walker.
- Port to main (`9ba3373e`): main has its own opcodes `IterOpen`, `ForInOpen`, `IterNext` and `IterClose`, so the instrument counts them by `Op::name` and needs no special ids. Main runs the `Wide` prefix in the loop: the instrument counts a wide word once, as `Wide`. The tables below are from the measured branch, not from main.
- Feature off equals base: `BARE_JS_EXEC=byte rows.sh target` (min of 3, scale 10) gives loopfn 827.05M, fib 56.67M, calls 1644.28M, loop 2489.04M, closures 479.59M. These equal the numbers of the branch before the instrument, to the last digit (spread 0.000%).
- Runs, all `bchist` counts with `BARE_JS_EXEC=auto` (`byte` totals differ by under 0.002%):
  - Page: `type_load /data/bare-tmp/jit-page '#a1' 0` and `... 50` (React 19 production bundle). Also 5 keys for callgrind.
  - Full-coverage fixture: `reactlike.js` from branch `baseline-jit-design` (`spike/jit-cranelift/measure`), `GROUPS=6 ROWS=26 RENDERS=6`, with the `runjs` runner of that branch built in a scratch dir. Its counts did not change since round one (1,700,145 operations): the fixture was already at 100% coverage.
  - `eval_load` cases `loop loopfn calls fib closures objects webpack` (scale 10), `scripts_load many few cross host` (scale 1). `form_load gates` runs no bytecode (no `bcstats` line), so it is not in the tables.
- Instruction counts: `ic.sh` (min of 3 `perf stat`, spread under 0.01%).
- Counts are deterministic: byte and auto runs of the page gave 59,546,976 and 59,545,948 operations.
- Cost outside the loop: `valgrind --tool=callgrind` on a release build with symbols and line tables (`CARGO_PROFILE_RELEASE_STRIP=false`, `DEBUG=1`; same code as the normal build). Page: 0 keys and 5 keys. Fixture: 1 and 6 renders. "Steady" is the difference (page 5 keys minus 0 keys, 844.3M instructions; fixture 193.7M). Exclusive cost per symbol is grouped by name patterns of my own, so every group row is a hypothesis. The totals are exact. The group "other" is 3% to 8%.
- Loop lines: callgrind line data of `bc_loop` gives the dispatch lines (`let w = ops[pc]`, `match w as u8`, the `Ok(())` after the arm) and line 0 (no source line: compiler code for decode, loop and spills).

## Old and new, main numbers

| | Old (37% of calls on the lane) | New (99.7%) |
|---|---|---|
| Page `auto`, load, instructions | 608.84M | 485.18M |
| Page `auto`, 50 keys, instructions | 13,865.5M | 8,473.0M |
| Page, executed ops, 50 keys | 9.08M | 59.55M |
| Page, executed ops, load | 437k | 1.853M |
| Ops per lane call (50 keys) | 14.4 | 34.8 |
| Ops that give 90% (page, 50 keys) | 20 | 21 |
| Best 8 fused pairs, dispatches removed (page, 50 keys) | 14.2% | 16.4% |
| Dispatch+decode, instructions per op | 24 (micro) | 22.7 (page, line data) |
| Loop share of page instructions, steady | 3.6% | 36.0% |
| JIT bound, share of page instructions at 50 keys (hypothesis) | 1.5% (4.1% at full coverage) | 7.6% to 16.0% (all ops) |

## Tables

### Executed operations (share of all executed operations)

| Rank | Fixture (1.70M ops) | Page, 50 keys (59.5M ops) | Page, load only (1.85M ops) |
|---|---|---|---|
| 1 | Mov 17.8% | Mov 13.8% | Mov 13.5% |
| 2 | GetField 15.7% | GetField 11.5% | GetField 8.8% |
| 3 | SetField 9.9% | GetEnv 8.1% | JmpIf 7.6% |
| 4 | LoadNull 7.9% | JmpIfNot 6.6% | StrictEq 7.5% |
| 5 | GetGlobal 5.0% | LoadNull 5.6% | GetEnv 7.4% |
| 6 | Jmp 3.7% | JmpIf 5.5% | LoadK 6.4% |
| 7 | JnSNe 3.6% | StrictEq 5.4% | JmpIfNot 5.7% |
| 8 | LoadI 3.4% | LoadI 4.9% | LoadI 4.9% |
| 9 | CallR 2.9% | LoadK 4.3% | LoadNull 4.7% |
| 10 | GetElem 2.6% | CallR 3.6% | CallR 3.6% |
| ops for 90% | 20 | 21 | 21 |

Lists for 90% (in rank order):
- Page, 50 keys (21): Mov GetField GetEnv JmpIfNot LoadNull JmpIf StrictEq LoadI LoadK CallR Jmp SetField StrictNe GetEnvX JnSNe Ret JnSEq SetEnv Not DefField GetElem.
- Page, load (21): Mov GetField JmpIf StrictEq GetEnv LoadK JmpIfNot LoadI LoadNull CallR SetField Jmp GetEnvX StrictNe JnSNe Ret LoadThis SetEnv DefField RetUndef Not.
- Fixture (20): Mov GetField SetField LoadNull GetGlobal Jmp JnSNe LoadI CallR GetElem LoadThis JnSEq JmpIfNot DefField LoadK BitOr Ret Inc StrictEq JnLt.
- The new ops are minor: `IterNext` 0.71%, `ForInOpen` 0.12%, `GetEnvX` 2.2%, `SetEnv` 1.7%, `ToKey` 0.46%, `Typeof` 0.56%. Loop ops are 0.9% together; round one would have counted them as `Throw`. `Throw` itself is absent from the list.
- `GetEnv`, `GetEnvX`, `SetEnv` are 12.0% of the page ops at 50 keys. Round one had `GetEnv` at 6.6%.

`eval_load` and `scripts_load` micro cases (operations, top ops): loop 36.0M (GetGlobal 22%, SetGlobal 11%, LoadK 11%); loopfn 12.0M (6 ops at 16.7%: JnLtK Add Inc Jmp MulK ModK); calls 19.0M (GetGlobal 26%); fib 0.51M (Ret GetGlobal CallR JnLtK 18% each); closures 2.2M (GetGlobal 23%); objects 8.0M (GetGlobal 25%); webpack 42.0k ops (GetElem 15.5%, GetField 13.1%, GetGlobal 11.9%, Mov 10.7%). `scripts_load` many, few, cross, host: 1.2k, 2.5k, 184k and 120k ops, GetGlobal first in all. These cases are global-variable heavy and not typical of the page.

### Top pairs and triples (share of executed operations)

| Fixture | Page, 50 keys | Page, load |
|---|---|---|
| Mov>GetField 4.9% | StrictEq>JmpIf 4.7% | StrictEq>JmpIf 6.8% |
| SetField>Mov 4.9% | Mov>Mov 3.3% | LoadK>StrictEq 4.4% |
| Mov>Mov 3.1% | LoadI>StrictEq 2.4% | JmpIf>LoadK 4.1% |
| GetField>SetField 3.0% | Mov>GetField 2.3% | Mov>Mov 3.7% |
| Mov>SetField 2.8% | LoadK>StrictEq 2.2% | LoadI>StrictEq 2.3% |
| LoadNull>JnSNe 2.7% | Mov>CallR 2.1% | Mov>CallR 2.0% |

Top triples: fixture SetField>Mov>GetField 3.3%, Mov>GetField>SetField 3.0%. Page: LoadK>StrictEq>JmpIf 2.2%, LoadI>StrictEq>JmpIf 2.2%, then rotations of the same two shapes (1.8% to 1.9% each), Mov>Mov>Mov 1.1%. The six shapes around `x === constant` and a branch add up to about 11.5% of the page ops. That is one source pattern: a compare of a register with a constant, then a jump when true. The generator fuses the compare with a jump only when the jump is taken on false (`JnSEq`, `JnSNe`). The jump-when-true form (`StrictEq` then `JmpIf`) is not fused. Hypothesis: this is a cheap generator change that needs no JIT.

### Fused pairs: dispatches removed

Greedy pick by count, with the rule that no operation is the head of one chosen pair and the tail of another. Runs of such pairs cannot overlap, so the sum is exact (counted, not modeled). One fused operation replaces one pair.

| Chosen pairs | Fixture | Page, load only | Page, 50 keys |
|---|---|---|---|
| 4 | 12.1% | 13.8% | 11.9% |
| 8 | 18.0% | 17.7% | 16.4% |
| 16 | 24.1% | 21.6% | 20.8% |
| 32 | 29.4% | 25.5% | 25.2% |

Old page, 50 keys: 8.6%, 14.2%, 21.7%, 27.9%. Savings (hypothesis): one removed dispatch saves 10.8 instructions (the three dispatch lines) to 22.7 (with line 0). On the page at 50 keys, 8 pairs save 9.75M x (10.8 to 22.7) = 105M to 221M of 8,473M, that is 1.2% to 2.6%. 32 pairs save 1.9% to 4.0%.

### Inline cache misses (counted at the slow path; first fills included)

| | GetField misses | SetField misses |
|---|---|---|
| Fixture | 3,757 of 266,441 (1.4%) | 47,554 of 167,643 (28.4%) |
| Page, 50 keys | 744,932 of 6,847,754 (10.9%) | 71,870 of 1,825,304 (3.9%) |
| Page, load only | 25,263 of 163,190 (15.5%) | 28,766 of 66,880 (43.0%) |

Old page, 50 keys: 13.7% and 10.2%.

### Hot functions

| | Fixture (18 functions) | Page, 50 keys (468 functions) | Page, load only (422) |
|---|---|---|---|
| Top 1 | reconcileChildren 28.1% | `@6705` 10.4% (36,902 calls, 168 ops per call) | `@6705` 8.4% (877 calls, 177 ops per call) |
| Top 5 cumulative | 70.5% | 34.6% | 29.8% |
| Top 10 cumulative | 92.7% | 52.4% | 44.9% |
| Top 25 cumulative | all 18 | 74.0% | 68.0% |

Old page, 50 keys: top 1 22.7%, top 5 52.0%, top 25 89.7%. The work is now much less concentrated: the lane runs the large functions (Fl 650 ops per call, TE 279, `@6705` 168) that the tree walker ran before. Hot functions average 20 to 650 ops per call; the page mean is 34.8 ops per call.

### Where the cost is (callgrind, release build with symbols, exclusive instructions)

Steady = page 5 keys minus 0 keys (844.3M); fixture 6 renders minus 1 (193.7M). Load = 0 keys (489.6M). Group rows are hypothesis (name patterns).

| Group | Fixture steady | Page steady (5 keys) | Page load |
|---|---|---|---|
| VM loop: dispatch, decode and arm code (`bc_vm.rs` lines) | 32.9% | 28.1% | 15.3% |
| VM loop: inlined std code (bounds checks, `Vec`, copies) | 10.7% | 6.6% | 3.5% |
| VM loop: inline-cache hit code (inlined `heap_shape.rs`) | 2.7% | 1.3% | 0.5% |
| Call setup (`bc_call`, `bc_enter`, `bc_run_rf`, script switch, `bind_call`) | 3.0% | 16.4% | 9.1% |
| Property slow paths, shape lookup, names | 15.8% | 14.4% | 10.2% |
| Object define and allocation | 13.7% | 5.8% | 4.9% |
| malloc, free, libc | 7.4% | 5.4% | 8.5% |
| GC (collect, DOM trace) | 0.0% | 5.5% | 0.0% |
| Operators and coercion (`binary`, `fmod`, `loose_eq`) | 4.5% | 4.2% | 2.3% |
| Strings, names, hash maps | 4.6% | 3.1% | 6.6% |
| Iterator, spread, destructuring, loop ops (`bc_catch`, `bc_rare`, `for_in_keys`) | 0.8% | 3.3% | 2.0% |
| Scope and environment (`bc_rare_env`, `find_in`, `env_declare`, `new_env`) | 0.5% | 1.9% | 1.1% |
| Compile: scope data, codegen, verify (exclusive) | 0.0% | 0.3% | 8.6% |
| Lex, parse, regexp | 0.0% | 0.3% | 19.6% |
| Other | 3.4% | 3.4% | 7.9% |

The page at 50 keys is 8,473M instructions, and callgrind data exists only to 5 keys. The loop total (dispatch rows, std rows and IC row) is 398.5M of 1,334M at 5 keys, and 304M of the 844M steady (36.0%). At 7.66M ops (5 keys) it is 52 instructions per op. Round one: the loop was 3.6% of the page, with 37% of calls on the lane. Now it is the largest single group, and it is where dispatch lives.

Cost of the dispatch, from line data of `bc_loop`, page, 5 keys, 7.66M ops:
- `let w = ops[pc]`: 3.0 instructions per op. `match w as u8`: 6.9. `Ok(())`: 0.9. Total of the three lines: 10.8.
- Line 0 in `bc_vm.rs` (decode, loop and spills, no source line): 11.9. Total with line 0: 22.7. The fixture gives the same split: 3.0, 7.0, 1.0 and 13.2, total 24.2. Round one gave 24.1 on a micro.
- Whether line 0 is all dispatch is not known (hypothesis). So the dispatch cost is 10.8 (certain lines) to 22.7 (all of line 0) instructions per op.

## Answers

(a) **Ops for 90%.** 21 on the page at 50 keys and at load, 20 on the fixture. 15 of them are the same on page and fixture. The page has `GetEnv`, `GetEnvX`, `SetEnv`, `JmpIf`, `StrictNe`, `Not` where the fixture has `GetGlobal`, `LoadThis`, `Inc`, `JnLt`, `BitOr`. A JIT needs about 24 simple ops for 91% to 93% of the work. The new ops (loop ops, destructuring, spread) are under 1% together and can stay as helper calls.

(b) **Fused pairs and triples.** The best 8 fused pairs remove 16.4% of dispatches at 50 keys (17.7% at load), the best 32 remove 25.2%. No pair is above 4.7%. The one visible group is `x === constant` then jump-if-true: `StrictEq>JmpIf` 4.7% plus `LoadK/LoadI>StrictEq` 4.6% (about 11.5% as triples). Saved instructions (hypothesis): 1.2% to 2.6% of the page for 8 pairs, 1.9% to 4.0% for 32.

(c) **Upper bound for a JIT of the hot ops (hypothesis).** The JIT removes dispatch, decode and operand handling: 10.8 to 22.7 instructions per op.
- All ops: load 1.853M ops = 20M to 42M of 485M, that is 4.1% to 8.7%. 50 keys: 59.5M ops = 643M to 1,352M of 8,473M, that is 7.6% to 16.0%.
- Hot ops only (the 24 simple ops, 92% of ops) in the top 25 functions only (74% of ops at 50 keys; 68% at load): 68% of the all-ops bound. 50 keys: 5.2% to 10.9%. Load: 2.6% to 5.4% (68% x 92% = 63% of the all-ops bound; before the cost of compiling). Per-op work in the arms (field access, `Value` copies, bounds checks) stays unless the JIT also specializes it.
- The bound is not a gain: a JIT adds compile cost and enters and leaves the loop at every call to a helper or an uncompiled function. The hot functions run 20 to 650 ops per call.
- Old bound: 1.5% of page instructions at 37% coverage, 4.1% at full coverage. At 99.7% coverage the measured range is 7.6% to 16.0%, so the old high estimate was low.

(d) **Concentration.** Top 5 functions: 34.6% of ops at 50 keys (29.8% at load; fixture 70.5%). Top 10: 52.4% (44.9%; fixture 92.7%). Top 25: 74.0% (68.0%). The long tail is large: 468 functions ran on the page at 50 keys. A tier-up threshold would compile a few dozen functions, and cover about 70%.

(e) **Compile phases.** Exclusive cost of scope data, codegen and verify is 8.6% of the load run (42M of 489.6M). Inclusive cost, with the allocation and hashing they call: `bc_compile` 58.6M (12.0%) plus `bc_finish` 15.5M (3.2%) = 15.1% of load. At 5 keys the same inclusive cost is 79.1M of 1,334M (5.9%). 422 functions compile at load and 468 at 50 keys, so the compile cost stays about 75M to 85M. That is about 1.0% of the 50-key run (hypothesis). At load, lane compile (15.1%) plus lex and parse (19.6%) is 35% of all instructions. A JIT would add its own compile cost on top of this.

(f) **Environments.** `GetEnv`, `GetEnvX`, `SetEnv` and `InitEnv` are 12.0% of the page ops at 50 keys (11.3% at load). `PushEnv` and `PopEnv` are 0.003% (1,844 times in total). The arm code costs 7.8M instructions (the arms of `GetEnv`, `SetEnv`, `InitEnv`), `bc_rare_env` (the wide `GetEnvX` forms) 9.0M and `new_env` 0.65M, in the 5-key run of 1,334M: 1.3%. With the dispatch of these ops (0.92M ops x 22.7 = 21M) it is 2.9% (hypothesis). Environment creation and release are inlined into call setup (`bc_enter` is 23.4M, 1.75%, and cannot be split here), so the bound for all environment cost is about 1.3% to 4.6% (hypothesis). Environments are not a large lever. `GetEnv` is a cheap arm; the cost is in the dispatch, which a JIT or a fused op would remove.

Other findings:
- Call setup is the second group: 16.4% of the steady page (`bc_call` 50.9M, `select` 26.6M, `bc_run_rf` 24.6M, `bc_enter` 23.4M, `call_fn_inner` 28.3M, `call_script` 18.0M, in the 5-key run). `Interp::select` alone is 2.0%: it swaps the whole script state (`prog`, `cc`, the `scripts` slot) at a call across scripts. A page has many scripts. Hypothesis: about 107 instructions per call at 247,757 calls (callgrind caller data; not checked per call).
- Property slow paths and define are 14.4% and 5.8% of the steady page. The `GetField` miss rate is 10.9% (744,932 misses at 50 keys); the share of the property group that is the miss path is not measured.
- GC is 5.5% steady on the page (collect 12.2M, `Host::trace` 7.5M, others); 0% on the fixture.
- Fixture: dispatch-related loop rows are 46% of the steady instructions (32.9 + 10.7 + 2.7), more than on the page, because the fixture has little call setup and no GC.

## Recommendation

A JIT of the hot ops can remove at most 5% to 11% of the page instructions at 50 keys (7.6% to 16% for all ops) and 3% to 5% at load, before its own compile cost: a real but modest lever, much smaller than the 36% loop share.
Do the cheaper levers first: fuse compare and jump-if-true (hypothesis 0.5% to 1.1% of the page), cut call setup including `select` (16% of the steady page), and the property slow path (14%); build the JIT after that, and only if it also cuts arm work (inline-cache hits, `Value` copies), not only the dispatch.

## Not tested

- The page with more than 50 keys for callgrind (callgrind ran at 0 and 5 keys; 50-key shares are from op counts and `perf stat` totals), the private React dashboard journey, and other pages.
- Real cost of a fused operation or of compiled code: all savings are counts times a per-dispatch estimate from line data.
- Whether line 0 of the loop is all dispatch (it has no source line).
- Triples and pairs across a call (a pair spans `CallR` and the first operation of the callee).
- Symbol grouping is by name pattern, so the group rows are approximate. The split of environment cost inside `bc_enter` is not known.
- `form_load gates` (no lane code runs), `exec_diff.sh`, the unit tests and the cost matrix: not run. The change is behind a feature that is off by default; the narrow rows with it off equal the base numbers. Clippy: not run.
