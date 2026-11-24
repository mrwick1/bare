# Research: where the page spends instructions after the calls work

Date: 2026-11-17. Base: origin/main `4d4b5e5442` (PR #639: calls of other scripts and natives stay in the loop). No engine code changed.
Inputs: `docs/research-lane-op-histogram.md` (the profile before the calls work), `docs/design-lane-calls.md`, `docs/design-heap-objects.md`, `docs/design-baseline-jit.md` (branch `baseline-jit-design`).
Purpose: re-rank the next levers. Repo rule (`CLAUDE.md`): find the design that removes a kind of work. This note gives the data. It is not a design.

## Summary

- The call path is no longer a lever. Call setup fell from 16.4% to 3.0% of the steady page. `select` fell from 2.0% to 0.2%.
- The bytecode loop is now 45% to 48% of the steady page. The property slow paths are 27% to 29%. Together they are three quarters of the work.
- A narrow JIT of the hot ops can remove at most 5.5% to 12.9% of the page at 50 keys, and 2.5% to 5.7% at load, before its own compile cost (hypothesis). It cannot touch the property slow paths, GC, the out-of-line operators or parsing.
- The biggest lever per effort is a fast path for ToInt32 in the bitwise operators. Measured: a 4-line change cuts the page at 50 keys by 1.72% (7167.75M to 7044.42M) and load by 0.83%.
- The next levers are "make the work unneeded" designs for the slow paths, each about 4% of the page at 50 keys: a for-in key cache, copy by shape (`Object.assign`, spread, rest), the pre-shaped literal (B1 `NewLit`), and keyed element access. These are larger than any single JIT step per unit of effort (hypothesis).

## Method

- Machine: AMD Ryzen 9 5900HX, Linux. Other agents ran at the same time. Only instruction counts are used.
- Page: `type_load /data/bare-tmp/jit-page '#a1' N`, `BARE_JS_EXEC=auto`, the public React 19 page. N = 0 (load), 5 and 50 keys.
- `perf stat` (`/data/bare-tmp/jit-briefs/ic.sh`, min of 3, normal release build): load 441.23M (spread 0.013%), 5 keys 1152.28M (0.006%), 50 keys 7167.75M (0.000%). These equal the brief's numbers.
- `bcstats` at 50 keys: `compiled=468 fallbacks=19 runs=111617 calls_lane=1713027 calls_tree=4720 natives=488357`.
- Callgrind: release build with `CARGO_PROFILE_RELEASE_DEBUG=line-tables-only CARGO_PROFILE_RELEASE_STRIP=false`, own target dir; same code. `GLIBC_TUNABLES=glibc.malloc.tcache_count=65535` as in `ic.sh`. One run each (callgrind is exact for a run). Totals: load 445.97M, 5 keys 1158.53M, 50 keys 7176.60M.
- Steady: 5 keys minus 0 keys (712.56M) and 50 keys minus 0 keys (6730.62M). The 50-key callgrind run is new; the earlier notes had only 5 keys.
- Groups: exclusive cost per function, sorted into groups by name and file patterns of my own (`/data/claude-tmp/reprofile/py/group.py`, not committed). Group rows are a hypothesis; totals are exact. The patterns differ from the old note, so compare old and new only at the level of the large groups.
- Inclusive costs (a function and all it calls) come from the callgrind call edges, with call counts. Inclusive rows overlap with the groups; do not add them.
- Op histogram: feature `bc-hist`, `tools/js/bc_hist.sh` with its own target dir, `BARE_JS_EXEC=auto`, at 0, 5 and 50 keys.

## 1. Where the instructions go (callgrind, exclusive)

| Group | Load (446.0M) | Steady, 5 keys (712.6M) | Steady, 50 keys (6730.6M) | Whole run, 50 keys (7176.6M) |
|---|---|---|---|---|
| 1 Bytecode loop (`bc_loop`) | 22.4% | 45.4% | 47.7% | 46.2% |
| of it: `bc_vm.rs` lines (dispatch, decode, arms) | 17.5% | 35.4% | 37.2% | 36.0% |
| of it: inline-cache hit code (inlined `heap_shape.rs`) | 0.7% | 1.4% | 1.5% | 1.4% |
| of it: inlined std, heap and value code | 4.2% | 8.6% | 9.1% | 8.8% |
| 2 Call setup (`bc_call_r`, `call_native`, `call_fn_inner`, `call_script`, `bc_enter`, `bc_run_rf`, `bind_call`) | 2.2% | 3.0% | 3.1% | 3.1% |
| 3 Property slow paths | 17.9% | 27.4% | 28.8% | 28.1% |
| of it: define and allocation | 6.6% | 8.4% | 8.8% | 8.7% |
| of it: get, set, lookup, IC fill | 8.5% | 14.2% | 14.9% | 14.5% |
| of it: keys and names (`name_key`, `to_property_key`) | 1.4% | 2.5% | 2.7% | 2.6% |
| of it: key enumeration (`for_in_keys`, `own_keys`) | 0.8% | 1.9% | 2.0% | 2.0% |
| 4 GC (collect, mark, sweep, host trace) | 0.1% | 5.8% | 4.9% | 4.6% |
| 5 Parse and compile (lex, parse, scope, codegen, verify, regexp) | 29.3% | 0.6% | 0.1% | 1.9% |
| 6 Builtins (JS natives and host: DOM, style, layout) | 3.3% | 1.4% | 1.0% | 1.2% |
| 7 `select` and `bc_sync` | 0.2% | 0.2% | 0.2% | 0.2% |
| 8 Other | 24.5% | 14.7% | 14.2% | 14.8% |
| of it: malloc, free, libc, `Vec` growth | 8.8% | 4.3% | 3.5% | 3.9% |
| of it: operators, coercion, strings (`binary`, `fmod`, `trunc`) | 3.7% | 6.6% | 7.0% | 6.8% |
| of it: tree walker and engine glue | 3.6% | 1.8% | 1.8% | 1.9% |
| of it: hash maps | 4.1% | 1.1% | 1.0% | 1.2% |

Old and new (steady, 5 keys minus 0 keys):

| | Before the calls work (`c59afb9a`, old note) | Now (`4d4b5e5442`) |
|---|---|---|
| Steady instructions | 844.3M | 712.6M |
| Loop share | 36.0% | 45.4% |
| Call setup share | 16.4% (with `select`) | 3.0%, plus `select` 0.2% |
| GC share | 5.5% | 5.8% |
| Executed ops (5 keys minus 0) | 7.66M (5 keys) | 5.32M |
| Loop instructions per op | 52 | 60.8 |

The loop costs more per op now because inline calls run in it. Call setup moved into the `CallR` arm.

### 1.1 The large inclusive rows (50 keys minus load, 6730.6M)

| Path | Calls | Inclusive | Per call | Share of steady |
|---|---|---|---|---|
| `bc_call_r` (natives and other callees from `CallR`) | 490,344 | 1504.7M | 3,069 | 22.4% (the callee's work included) |
| `call_native` from `bc_call_r` | 470,983 | 614.0M | 1,304 | 9.1% (the native's work included) |
| `bc_get_field` (GetField slow path) | 719,630 | 382.5M | 532 | 5.7% |
| GC (`Interp::collect`, 13 collections in the 50-key run) | 13 | 373.1M (whole run) | 28.7M | 5.2% of the whole run |
| `bc_def_field` (`DefField`) | 649,397 | 312.1M | 481 | 4.6% |
| `ao_assign` (`Object.assign`) | 16,050 | 169.6M | 10,568 | 2.5% |
| `copy_props` (object spread and rest) | 22,150 | 120.5M | 5,440 | 1.8% |
| `for_in_keys` (for-in) | 69,625 | 288.8M | 4,148 | 4.3% |
| `Interp::binary` from `bc_loop` (operators without an arm fast path) | 1,554,307 | 283.4M | 182 | 4.2% |
| `bc_get_elem` (`GetElem`) | 648,771 | 251.0M | 387 | 3.7% |
| `put_prop` from `bc_loop` (keyed store; hypothesis: `SetElem`, 271,867 ops) | 263,492 | 234.4M | 890 | 3.5% |
| `select` | about 29,000 per 5 keys | 12.3M | | 0.2% |

Details found in these rows:
- **Bitwise operators.** `BitAnd`, `BitOr` and the other bitwise ops have no arm fast path. They call `Interp::binary`, which calls `to_int32` for each operand. `to_uint32` (`value.rs:236`) always runs `trunc` and `rem_euclid` (`fmod`): 1,986,942 `fmod` calls and 97.4M (`fmod` 67.6M, `trunc` 29.8M) in the steady 50 keys. The operands are almost always already in the i32 range (hypothesis; the experiment below agrees).
- **GetField slow path.** Of 719,630 calls, 689,826 reach `get_ic`. Only 252,028 are real misses (`ic_miss`, 124.3M, 493 each). The rest are prototype-chain hits that run out of line (`get_ic` self and `name_key`). 77,077 calls run a getter (`call_fn_inner`, 90.3M). The bc-hist "miss" count (744,932 of 6,847,754, 10.9%) counts all of these. The real miss rate is 3.7%.
- **for-in.** Each `for_in_keys` call builds the key list again: `property_keys` and `own_keys` (79.7M), a `HashMap` insert per key for the seen set (69.7M), `peek_own_property` per key (69.5M), `Vec` growth (30.1M). That is 4,148 instructions per loop entry.
- **DefField.** 481 instructions each: `Interp::def` 240.1M (into `define_own_property`), `name_key` 41.6M.
- **Keyed access.** `bc_get_elem` runs `to_property_key` (40.3M) and `get_prop` (165.3M) for every `GetElem`. There is no keyed cache. The key mix (array index or string) was not measured.
- **GC.** 13 collections in the 50-key run (12 from `ui_key`, 1 from `Run::resume`), 28.7M each. The load-only run has none.

## 2. Op histogram

`bchist total_ops`: load 1,661,304; 5 keys 6,984,991; 50 keys 54,544,824 (the old note: 59.55M at 50 keys; the generator now fuses more compares and jumps).

### 2.1 Executed ops

| Rank | Load (1.66M) | 50 keys (54.5M) |
|---|---|---|
| 1 | Mov 15.3% | Mov 15.7% |
| 2 | GetField 9.8% | GetField 12.6% |
| 3 | GetEnv 8.3% | GetEnv 8.8% |
| 4 | JnSNe 7.2% | LoadNull 6.1% |
| 5 | LoadK 7.1% | JmpIfNot 5.1% |
| 6 | LoadNull 5.2% | JnSNe 5.0% |
| 7 | JmpIfNot 4.9% | LoadK 4.7% |
| 8 | CallR 4.0% | CallR 3.9% |
| 9 | SetField 4.0% | SetField 3.3% |
| 10 | LoadI 3.1% | Jmp 3.3% |
| Ops for 90% | 22 | 22 |

Ops for 90%, 50 keys (rank order): Mov GetField GetEnv LoadNull JmpIfNot JnSNe LoadK CallR SetField Jmp LoadI JnSNeK Wide StrictNe JnSEq Ret SetEnv JmpIf DefField GetElem RetUndef BitAnd.
Load: the same set without GetElem, with LoadThis. 5 keys: the same set as 50 keys.
`Wide` is 2.46% of ops at 50 keys (1,341,215 prefix words). Every wide prefix is one extra dispatch.

### 2.2 Top pairs and triples (50 keys)

| Pair | Share | Triple | Share |
|---|---|---|---|
| Mov>Mov | 3.57% | LoadK>JnSNe>LoadK | 2.05% |
| Mov>GetField | 2.53% | JnSNe>LoadK>JnSNe | 2.05% |
| Mov>CallR | 2.33% | JnSNeK>JnSNeK>JnSNeK | 1.66% |
| LoadNull>JnSNe | 2.33% | Mov>Mov>Mov | 1.17% |
| LoadK>JnSNe | 2.26% | Mov>GetField>SetField | 1.11% |
| JnSNe>LoadK | 2.05% | Mov>Mov>CallR | 1.10% |
| SetField>Mov | 2.03% | SetField>Mov>GetField | 1.09% |
| GetEnv>GetField | 2.00% | Jmp>LoadNull>JnSNe | 1.07% |
| JnSNeK>JnSNeK | 1.98% | | |
| StrictNe>JmpIfNot | 1.92% | | |

The old top pair `StrictEq>JmpIf` (4.7%) is gone. The new top triples are chains of `LoadK>JnSNe` and `JnSNeK`: the shape of a `switch` over constants, tested case by case (hypothesis from the op shape; the source was not checked).

### 2.3 Fused pairs: dispatches removed (bc-hist greedy count)

| Chosen pairs | Load | 5 keys | 50 keys |
|---|---|---|---|
| 4 | 12.55% | 10.63% | 10.16% |
| 8 | 17.62% | 16.37% | 16.17% |
| 16 | 21.92% | 20.65% | 20.64% |
| 32 | 25.76% | 25.18% | 25.08% |

### 2.4 Hot functions (share of executed ops)

| | Load (422 functions) | 50 keys (468 functions) |
|---|---|---|
| Top 1 | `@6705` 9.1% | `@6705` 11.0% (36,902 calls, 163 ops per call) |
| Top 5 | 27.1% | 35.6% |
| Top 10 | 42.5% | 51.6% |
| Top 25 | 66.3% | 73.3% |

### 2.5 Cost of one dispatch (`bc_loop` line data)

At load, 5 keys and 50 keys the numbers agree. 50 keys: `let w = ops[pc]` (line 1125), `match w as u8` (1258) and `Ok(())` (1540) cost 599.3M for 54.54M ops: **11.0 instructions per op**. Line 0 (no source line: decode, loop, spills) is 802.9M: 14.7 per op. Together **25.7**. The old note: 10.8 and 22.7. Whether line 0 is all dispatch is not known.

## 3. Experiment: ToInt32 fast path (throwaway, not committed)

Change: three lines at the start of `to_uint32` in `value.rs`: `let i = n as i32; if i as f64 == n { return i as u32; }`. NaN fails the compare, and -0 gives 0, so the result is the same for every input (checked by reading; no unit test run).
Built in a copy of the normal target dir. `ic.sh`, min of 3:

| | Base `4d4b5e5442` | Experiment | Change |
|---|---|---|---|
| Page load | 441.23M | 437.58M | -0.83% |
| Page 50 keys | 7167.75M | 7044.42M | -1.72% |

The page output was the same (evaluator steps, value length, render counts). The change was reverted.
A direct arm for `BitAnd` and `BitOr` (two numbers that are i32: one `and` or `or`, no call) would also skip the `Interp::binary` call. The rest of the 283.4M row after the experiment is a hypothesis: about 1% more of the page at 50 keys.

## 4. Ranked levers

Shares are of the whole 50-key run (7176.6M callgrind) unless noted. "Removable" is the inclusive row (an upper bound). "Gain" is a hypothesis unless marked measured. Effort: S up to 3 days, M about 1 week, L 1.5 to 3 weeks, XL more (as in `engine-plan.md`).

| Rank | Lever | Removable (measured row) | Expected gain at 50 keys | Load | Effort | Basis |
|---|---|---|---|---|---|---|
| 1 | ToInt32 fast path, then a bitwise arm | `binary` from the loop 283.4M (3.9%) | **1.72% measured** (step 1); about 1% more (arm) | 0.83% measured | S | experiment |
| 2 | for-in key cache keyed by shape (V8 enum cache idea) | `for_in_keys` 288.8M (4.0%) | 3% to 3.5% | small | M | hypothesis |
| 3 | B1 pre-shaped literal `NewLit` (`design-heap-objects.md`) | `bc_def_field` 312.1M (4.3%), plus 673k `DefField` dispatches | about 3% | 1% to 2% (24,102 `DefField` at load) | M | hypothesis (design estimate under 100 per key) |
| 4 | Copy by shape (`Object.assign`, spread, rest) | `ao_assign` 169.6M + `copy_props` 120.5M (4.0%) | 2% to 3% | small | M | hypothesis (same-shape share not measured) |
| 5 | Keyed element caches (`GetElem`, `SetElem`) | 251.0M + 234.4M (6.8%) | 2% to 4% | small | M to L | hypothesis (key mix not measured) |
| 6 | GetField: inline prototype hits, no `name_key` before the hit, a polymorphic entry | `get_ic` + `name_key` 222.9M (3.1%) | 1.5% to 2.5% | small | S to M | hypothesis (real misses 3.7%; polymorphism not measured) |
| 7 | GC: fewer collections while typing | 373.1M (5.2%), 13 collections | 1% to 3% | 0 | M | hypothesis (depends on the trigger and RSS) |
| 8 | Fused pairs (8 pairs) | 8.82M dispatches | 1.4% to 3.2% | 1.2% to 2.8% | M | counted dispatches times 11.0 to 25.7 |
| 9 | Narrow baseline JIT of the hot ops | see section 5 | 5.5% to 12.9% bound, minus compile cost | 2.5% to 5.7% bound | XL | hypothesis |
| 10 | Wide operands without a prefix dispatch | 1.34M dispatches (2.46%) | 0.2% to 0.5% | | S to M | counted dispatches |
| 11 | Call-site cache (`design-lane-calls.md` B) | `bc_target` lookup, about 20 to 30 per lane call | 0.5% to 0.7% | | S | hypothesis |
| 12 | Cheaper `select` | 12.3M steady (0.2%) | under 0.2% | under 0.2% | S | measured upper bound |
| 13 | Parse and compile | steady 0.1%; load 29.3% exclusive | about 0 | the largest load lever | L | measured share |

Notes on the levers:
- Rank 2 to 5 remove a kind of work: the key list, the define per key, the copy per key, the key coercion. Each needs a design doc first (`CLAUDE.md`).
- Rank 3, 4 and 6 are steps of `design-heap-objects.md` (B1, C) or close to its option A. That design said the page gain was small, because 68% of typing then ran on the tree walker. That is no longer true: `calls_tree` is 4,720 of 1,717,747 calls.
- Rank 7: a collection costs 28.7M, of which the DOM host trace is a part (`Host::trace` 7.5M per 5 keys). A cut in collections raises peak RSS. The RSS rule applies.
- Load: parse and compile is 29.3% of load (exclusive). Inclusive: `parse_script_lazy_used` 70.5M (15.8%), `bc_compile_in` 59.3M plus `bc_finish` 16.4M (17.0%). `regexp::Parser::add_class` alone is 14.6M (3.3% of load). These are the load levers; a JIT adds to them.

## 5. The JIT: upper bound and what it cannot touch

A template JIT (`design-baseline-jit.md`, option C) removes the dispatch and the operand decode of the ops it covers: 11.0 to 25.7 instructions per op (section 2.5).

| | All ops | Covered: 22 ops for 90% in the top 25 functions (0.90 x 0.733 at 50 keys, 0.90 x 0.663 at load) |
|---|---|---|
| 50 keys (54.54M ops of 7176.6M) | 600M to 1,402M: 8.4% to 19.5% | 5.5% to 12.9% |
| Load (1.66M ops of 446.0M) | 18.3M to 42.7M: 4.1% to 9.6% | 2.5% to 5.7% |

- The covered share assumes op mix and function heat are independent (hypothesis).
- The bound is before the JIT's compile cost and the exits to helpers. `CallR` to a native, `GetField` misses and every slow path leave the code.
- A JIT that also specializes the arm code could remove part of the inlined std code (bounds checks, `Value` copies, 8.8%) and of the IC hit code (1.4%). Not measured; hypothesis.
- A JIT cannot touch: the property slow paths (28.1%), GC (4.6% exclusive, 5.2% inclusive), the out-of-line operators and coercion (6.8%), natives and host code, malloc (3.9%), call setup outside the loop (3.1%), and parse and compile (29.3% of load).
- Effort XL with a new crate, executable memory and per-architecture templates. Per unit of effort, ranks 1 to 6 give more.

## 6. Answer: the biggest lever per effort

The ToInt32 fast path: 1.72% of the page at 50 keys and 0.83% at load, measured, for a 4-line change (rank 1). After it, the slow-path designs of rank 2 to 5 each target about 4% of the page with about one week of work each. Their sum (about 10% to 13%, hypothesis) is in the range of the whole JIT bound, at lower risk and without a compile cost. Build the JIT only after those, and re-profile first.

## 7. Not tested

- Unit tests, clippy, `exec_diff.sh` and the cost matrix: not run (no kept code). The experiment of section 3 ran only the page.
- The narrow rows (`rows.sh`) and peak RSS for the experiment.
- The private React dashboard journey and other pages.
- The key mix of `GetElem` and `SetElem`, the polymorphism of the `GetField` misses, the same-shape share of `Object.assign` and spread, and the source of the `switch`-like chains.
- Whether line 0 of `bc_loop` is all dispatch.
- The GC trigger policy and its effect on RSS.
- Wall time. The `bc-hist` build with the feature off was not compared against the normal build (the instrument is off by default and the normal build gave the brief's numbers).
- Group rows are by name pattern (approximate). Inclusive rows overlap.
