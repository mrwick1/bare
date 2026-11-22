# 2026-11-22 — Adapt the js2 task collection trigger

The default in-task trigger starts at 16 MiB of growth. [The post-#909 report](2026-11-22-js2-memory-peaks.md) found 0.60 MiB live at the first collection in `objects_10`. The same policy made no collection in `throws_10`. A fixed 4 MiB trigger added three collections and 5.37% more instructions to the closure microbenchmark.

The heap counts bytes made available by block refills and large mappings. The mark phase already measures retained bytes. Both designs below reuse those counts and the cached `task_limit`. They do constant work at the end of collection. Small cells stay in 64 KiB blocks with lazy sweep. Large cells keep their own mappings. The between-task limit still comes from 8 MiB or 1.75 times the last live size, whichever is larger.

Lua 5.4's pause uses a multiple of the bytes after the previous collection. An older V8 policy raised its old-generation limit when young survival stayed high. Its current heap controller estimates a growth factor from allocation and collection rates. SpiderMonkey uses a retained-size multiplier with a floor and a factor that varies with zone size and collection frequency. Its balanced limit also uses allocation and collection rates. [Lua 5.4](https://www.lua.org/manual/5.4/manual.html#2.5.1) [V8 survival policy](https://chromium.googlesource.com/v8/v8.git/+/89e18f5599cb4cd462cb1ed324addd7388fb4d60/src/heap.cc#805) [V8 heap controller](https://chromium.googlesource.com/v8/v8/+/refs/heads/main/src/heap/heap-controller.cc) [SpiderMonkey scheduling](https://raw.githubusercontent.com/mozilla-firefox/firefox/main/js/src/gc/Scheduling.cpp)

## Designs

- Retained-size multiplier. Start at 2 MiB. After each collection, collect again when the last live bytes plus growth reach the larger of 4 MiB and five times live. This uses no new measurement or state. A large live graph gets more time to grow. There is no way to distinguish a growing live graph from a stable graph that produces garbage between collections.
- Retained growth. Start with a 512 KiB sample. After each collection, compare the increase in live bytes with the growth counter. If at least half the growth survives, give the heap the larger of 16 MiB and twice live as headroom. Otherwise wait for the larger of 4 MiB total and 1.75 times live. This uses the previous mark count and the growth counter before either is reset. It also preserves the explicit fixed `Trigger` configuration. Use this design first because the early sample costs less to mark on retained workloads.

These estimates are hypotheses. RSS includes the process and can exceed the trigger from stale blocks, a large allocation, or a native call that holds the collector. Counts refer to in-task collections.

| Row | Retained-size multiplier, peak MiB / collections | Retained growth, peak MiB / collections |
|---|---:|---:|
| `objects_10` | 8–10 / 4–6 | 8–10 / 5–7 |
| `throws_10` | 7–9 / 2 | 7–9 / 2 |
| `closures_10` | 10–12 / 1 | 10–12 / 1 |
| `interp_load loopfn 3000000` | Baseline / 0 | Baseline / 0 |
| `interp_load calls 1000000` | Baseline / 0 | Baseline / 0 |
| `interp_load fib 27` | Baseline / 0 | Baseline / 0 |
| `interp_load closures 1000000` | Near baseline / 3 | Near baseline / 3 |
| `interp_load objects 1000000` | 7–9 / 8 | 7–9 / 8 |
| `js2_gen_load next 100000` | 7–9 / 1–3 | 7–9 / 1–3 |
| `js2_promise_load then 100000` | Near baseline / 1–3 | Near baseline / 1–3 |
| `js2_lex_load small 1 1`, `js2_parse_load full stmts 1` | Baseline / 0 | Baseline / 0 |

## Frozen baseline

The base is #909 at `48d03db9`, which contains #908. On archMachine, the median of three fresh processes was 16.94 MiB for `objects_10` and 10.54 MiB for `throws_10`. Both reproduce the post-#909 report within 1%. The five default instruction counts reproduce within 0.001%. The post-#909 report has no `closures_10` value. This rerun measured 11.04 MiB. The original engine decision benchmark used an earlier build and measured 14.06 MiB.

## Attempt 1

The 512 KiB sample with a 50% survival cutoff reduced median RSS from 16.75 to 13.64 MiB for `objects_10`, from 10.49 to 9.39 MiB for `throws_10`, and from 10.95 to 10.20 MiB for `closures_10`. The five interpreter microbenchmarks stayed within the default instruction gate. The closure microbenchmark went from three collections to four and used 5.78% fewer instructions.

The full-script closure row used 4.34% more instructions. `js2_promise_load then 100000` used 11.16% more instructions with glibc and 10.70% more with jemalloc. The supplemental `heap_load task 1000000` row used 1.95% more with glibc and 1.76% more with jemalloc. The other three matrix entries and the forced-collection, heap allocation controls passed.

## Attempt 2

The allocation trace showed a first collection at about 571 KiB of counted growth, before the closure workloads had retained user objects. The next collection marked 2.47 MiB in `closures_10` and 3.93 MiB in the promise row. The post-#909 policy made no collection in either case. In `objects_10`, the largest mapped total was at the transition to a larger array backing store.

Sample at 832 KiB to leave room for the initial block refills and collect while the live graph is small. Count a net increase of one-sixth of growth as evidence for a growing live graph. The growth counter charges a whole free list on refill, so the measured fraction can be lower than the fraction of allocated cells retained. If the graph grows, keep the original 16 MiB of initial headroom or twice live for larger graphs. Otherwise aim for 2 MiB total or 1.75 times live. Do not treat the free cells prepared by the sweep at the start of collection as allocations. The fixed-trigger allocator control keeps its explicit 16 MiB initial limit.

## Peak RSS

Linux on archMachine (Ryzen 9 5900HX, Rust 1.96.0, glibc 2.44 and perf 7.3.1). The final control at `8a18cb54` integrates #909 with the built-in install order from main. Its Git tree matches main at `14237fe1`. Each tree has its own Cargo target. Build with four jobs. Run the frozen engine-decision scripts with the same launcher. Check each result. For every RSS sample, copy the binary with `cp --reflink=never` and run it as a fresh process under the static `wait4` wrapper from `/data/claude-tmp/qjs-base/h/rss`. The table gives the median and range of three runs in MiB.

| Row | Fixed limit | Adaptive limit | In-task collections |
|---|---:|---:|---:|
| `objects_10` | 16.965 [16.945–16.992] | 9.090 [8.996–9.137] | 0 → 6 |
| `throws_10` | 10.512 [10.359–10.539] | 7.461 [7.406–7.504] | 0 → 3 |
| `closures_10` | 11.031 [10.992–11.113] | 10.160 [10.121–10.418] | 0 → 2 |
| Empty process | 5.453 [5.395–5.699] | 5.246 [5.070–5.504] | — |

`objects_10` uses 46.4% less RSS and is within 0.11 MiB of the old engine's 8.98 MiB. `throws_10` uses 29.0% less RSS. QuickJS direct measured 6.22 MiB for objects and 3.07 MiB for throws in the [engine decision benchmark](2026-11-21-engine-decision-benchmark.md). The collection counts come from a separate run that prints the realm counter after checking the result.

## Instructions

Run `perf stat -e instructions:u` three times for each build and input. The absolute counts show the range in millions at default glibc settings. Deltas compare minima. All five micro rows passed with default glibc and with `glibc.malloc.tcache_count=65535`. The micro closure collection count went from three to four. The micro object count went from one to 16.

| Row | Fixed limit, M | Adaptive limit, M | Default change | `codegen-units=1` change |
|---|---:|---:|---:|---:|
| `interp_load loopfn 3000000` | 866.302–866.302 | 866.300–866.300 | <0.001% | <0.001% |
| `interp_load calls 1000000` | 571.352–571.354 | 571.350–571.351 | <0.001% | <0.001% |
| `interp_load fib 27` | 248.675–248.676 | 248.673–248.674 | -0.001% | +0.001% |
| `interp_load closures 1000000` | 1942.711–1942.713 | 1814.291–1814.291 | -6.610% | -6.610% |
| `interp_load objects 1000000` | 744.201–744.201 | 746.542–746.542 | +0.315% | +0.315% |
| `objects_10` | 937.147–937.170 | 940.550–940.578 | +0.363% | +0.366% |
| `throws_10` | 299.126–299.127 | 299.617–299.617 | +0.164% | +0.164% |
| `closures_10` | 194.720–194.721 | 199.380–199.381 | +2.393% | +2.393% |

Run `tools/js/cost_matrix.sh -j4 -a -g '^examples/js2_'` on both build pairs. Four published rows match. The direct `gc_load` and `heap_load` controls are supplemental because `cost_rows.txt` does not list them. Run each through the same matrix worker. The matrix uses the minimum of three runs with the glibc thread-cache setting. It runs jemalloc when the glibc increase exceeds 0.5%.

| Row | Default glibc | Default jemalloc | CGU 1 glibc | CGU 1 jemalloc |
|---|---:|---:|---:|---:|
| `js2_gen_load next 100000` | +0.150% | — | +0.150% | — |
| `js2_lex_load small 1 1` | <0.001% | — | <0.001% | — |
| `js2_parse_load full stmts 1` | <0.001% | — | -0.002% | — |
| `js2_promise_load then 100000` | +1.164% | +1.116% | +1.165% | +1.116% |
| `gc_load chain 200000 20` | <0.001% | — | <0.001% | — |
| `gc_load tree 200000 20` | <0.001% | — | <0.001% | — |
| `heap_load heap 1000000` | +0.001% | — | +0.004% | — |
| `heap_load task 1000000` | +3.656% | +3.367% | +3.657% | +3.365% |
| `heap_load try 1000000` | -0.001% | — | -0.001% | — |

## Cost debt at the original head

- `closures_10` rises by 2.393% on both builds. It ran two collections on the adaptive trigger and zero on the fixed trigger. Hypothesis: marking the retained closure graph accounts for much of the added work.
- `js2_promise_load then 100000` rises by 1.164% with glibc and 1.116% with jemalloc. CGU 1 gives 1.165% and 1.116%. Hypothesis: the early mark walks pending reactions on a chain that the fixed trigger leaves alone.
- The supplemental `heap_load task 1000000` row rises by 3.656% with glibc and 3.367% with jemalloc. CGU 1 gives 3.657% and 3.365%. Collections went from one to 15. Mapped bytes at exit fell from 14.63 to 1.50 MiB.

Attempt 2 reaches the memory target and still exceeds the instruction gate. Leave auto-merge off.

## Correctness

The test for the early sample, growth, and shrink failed before the change and passed afterward. The second test checks that completing the lazy sweep before marking leaves the sampled growth unchanged. The heap, collector, and roots filters passed 12, 17, and 8 tests. The js2 typecheck passed.

The targeted test262 run covered Promise, WeakMap, WeakSet, array push and forEach, generator statements, and try statements. Both js2 builds passed 1,193 tests, failed 26, and skipped 154 in 34 directories. The adaptive build lost zero passes.

## Browser ramp

Build headless `bare-drive` with the profiling profile and frame pointers. Use the engine-decision `style_100k` fixture with `BARE_JS_EXEC=new` and `BARE_SEED=1`. Use a fresh binary copy and the static `wait4` wrapper for each run. All six runs passed the completion check. Median peak RSS fell from 45.918 [45.906–45.961] MiB to 32.355 [32.211–32.477] MiB. This is a 29.5% reduction.

Raw samples and scripts are in `/data/bare-tmp/r22/gc-trigger/` on archMachine. Local checks omit Windows, Miri, the full workspace suite, the rest of test262, public browser journeys and idle-machine timing.

## CPU follow-up profile

Callgrind measured `14237fe1` and the original #911 head `e2a7c4a6` on archMachine. Both trees used the profiling build and glibc with `tcache_count=65535`. The table gives thousands of instructions per collection. Lazy sweep runs on allocation refills and is shown separately as a total.

| Row | Collections, main → #911 | Collection per call, main → #911 | Mark drain, #911 | Shape and weak atom sweep, #911 | Start sweep, #911 | Finish block walk, #911 | Lazy sweep total, main → #911 |
|---|---:|---:|---:|---:|---:|---:|---:|
| `objects_10` | 0 → 6 | — → 554.1 | 353.5 | 113.3 | 62.3 | 16.5 | 0 → 225.9 |
| `throws_10` | 0 → 3 | — → 98.7 | 15.3 | 19.3 | 37.3 | 18.2 | 0 → 230.3 |
| `closures_10` | 0 → 2 | — → 2,181.3 | 2,109.5 | 19.3 | 29.1 | 14.5 | 0 → 302.9 |
| `interp_load closures` | 3 → 4 | 99,324.8 → 44,098.8 | 43,908.6 | 19.0 | 43.0 | 119.8 | 17,029.8 → 10,234.6 |
| `interp_load objects` | 1 → 16 | 195.2 → 130.3 | 15.0 | 19.4 | 66.1 | 21.5 | 97.8 → 481.9 |
| `js2_promise_load then` | 0 → 1 | — → 1,672.8 | 1,634.2 | 19.2 | 0.1 | 10.1 | 0 → 108.6 |
| `heap_load task` | 1 → 15 | 316.0 → 184.1 | 82.3 | 0.5 | 0.1 | 19.0 | 98.0 → 422.0 |
| `js2_gen_load next` | 0 → 2 | — → 102.1 | 16.8 | 20.9 | 38.8 | 17.1 | 0 → 53.8 |

The promise and standalone closure regressions came mainly from tracing live cells. The allocator task spent about 62,000 instructions per collection walking roots before the drain. Its total includes the first allocation after each collection. The controls `loopfn`, `calls`, `fib`, lexing and parsing run without a collection. The `gc_load` rows force 20 collections. The `heap_load heap` and `try` rows force 10 mark/sweep cycles and do not walk JS roots. None of the profiled rows entered the ephemeron table traversal.

## CPU follow-up attempt 1

Count the marked cells in each small block once. Sweep skips empty and fully live blocks and links only the free cells of mixed blocks. Reuse the block lists at the end of the mark. Move the first survival sample to 512 KiB. If startup growth has low survival, sample again after 832 KiB of growth. If a small live set stays the same size across two collections, allow 12 KiB of growth per live cell, up to the retained-growth limit.

The closure row passed the CPU rule at +0.274%. The promise row used +0.300% with glibc and +0.287% with jemalloc. The supplemental allocator task ran four collections and used +0.501% with glibc and +0.406% with jemalloc. The median peak RSS was 9.047 MiB for objects, 7.266 MiB for throws, 11.035 MiB for closures and 32.297 MiB for style. Closure RSS exceeded the 10.668 MiB ceiling.

## CPU follow-up attempt 2

Unmap a dense array's detached large elements record after the replacement succeeds. Each large mapping records its index in the heap's list. The list removes a detached record by swapping in the last entry and updating that entry's index. The collector compacts the same list in place and repairs the indices. Small records still wait for sweep.

Increase the stable-set budget to 16 KiB per live cell. The supplemental allocator task ran three collections and ended with 13.625 MiB mapped. The original trigger ran 15 collections and ended with 1.500 MiB mapped. The four RSS gates do not include this allocator control.

Compare the default release builds against main at `fc2fe21e`. Build the js2 examples and the Test262 runner together on both trees. Each instruction result uses the minimum of three `perf stat -e instructions:u` runs. Both allocators use the cost matrix's environment. The raw logs keep the minimum and maximum for each tree and allocator. The standalone scripts also pass with the unmodified glibc thread cache.

| Row | Glibc change | Jemalloc change |
|---|---:|---:|
| `js2_gen_load next 100000` | +0.146% | +0.140% |
| `js2_lex_load small 1 1` | -0.000% | +0.000% |
| `js2_parse_load full stmts 1` | +0.002% | +0.019% |
| `js2_promise_load then 100000` | +0.299% | +0.287% |
| `interp_load loopfn 3000000` | +0.000% | +0.000% |
| `interp_load calls 1000000` | +0.000% | +0.001% |
| `interp_load fib 27` | +0.001% | +0.001% |
| `interp_load closures 1000000` | -8.385% | -8.356% |
| `interp_load objects 1000000` | +0.181% | +0.175% |
| `gc_load chain 200000 20` | -2.872% | -2.844% |
| `gc_load tree 200000 20` | -3.724% | -3.705% |
| `heap_load heap 1000000` | -1.426% | -1.164% |
| `heap_load try 1000000` | -1.435% | -1.165% |
| `heap_load task 1000000` | +0.227% | +0.157% |
| `objects_10` | +0.387% | +0.381% |
| `throws_10` | +0.224% | +0.218% |
| `closures_10` | +0.272% | +0.262% |

The maximum increase was 0.387%. Both allocators passed every row on default release builds. The follow-up has no measured cost debt.

### Peak RSS after the follow-up

The RSS gate allows up to 5% above the held head. The table gives the median and range of three fresh processes in MiB. The same scripts checked the standalone results and the browser completion marker.

| Row | Held head | Follow-up | Change from held head |
|---|---:|---:|---:|
| `objects_10` | 9.090 | 8.453 [8.137–8.586] | -7.0% |
| `throws_10` | 7.461 | 7.152 [7.113–7.211] | -4.1% |
| `closures_10` | 10.160 | 10.109 [10.023–10.121] | -0.5% |
| `style_100k` | 32.355 | 32.480 [32.430–32.691] | +0.4% |

### Correctness after the follow-up

The survival sample test failed before the new scheduling rule. The dense array test failed before eager release and passed afterward. The original WeakSet test counted only the last collection. An earlier adaptive collection had cleared some of its dead entries and left 168 of the expected 200 for the explicit collection. The test helper now runs the script with a fixed trigger and collects once. The separate churn tests still cover live entries across in-task collections.

The heap, collector, roots, object and collection filters passed 13, 17, 8, 26 and 12 tests. Both `bare-js2` and `bare` passed typechecking. The targeted Test262 run covered Array, Promise, WeakMap, WeakSet, generator and try in 80 directories. Both js2 builds passed 3,796, failed 40 and skipped 406. The follow-up lost zero passes.

Raw data is in `/data/bare-tmp/r22/gc-cpu/` on archMachine. Local checks omit Windows, Miri, the full workspace suite and the rest of Test262.
