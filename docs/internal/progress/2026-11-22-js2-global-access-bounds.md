# 2026-11-22 — js2 global cache access

Use the verifier's global site bounds for hot cache lookups. Match the feedback cache length to the current code on entry and reload. Lazy compilation also resets feedback for the replaced stub. Run cache misses through the cold interpreter at the original instruction PC. The cold path retains the global name lookup, accessors, strict writes and reentry.

Baseline `2be8e4a9cee9ef69dd60eebed0a215f8ef322c68`. Measured tree `be2120dd2b21da92c1082e3d4106026a5446c476`. Machine `archMachine`, Ryzen 9 5900HX, Linux 7.2.15-zen1-1-zen, Rust 1.96.0.

Use the pinned [script-completion inputs](2026-11-21-engine-decision-benchmark.md). The scale-10 calls and loop checksums are 35741 and 63. The release builds use locked dependencies. The workspace already builds js2 with one codegen unit. The control sets `RUSTFLAGS="-C codegen-units=1"` for all crates on both builds.

Report the minimum and spread of three fresh `perf stat -e instructions:u` runs with at least 99% counter coverage. Memory runs use glibc. Take the highest sampled RSS of the executable mappings from `/proc/PID/smaps` across three runs with ASLR disabled. Capture mappings at checksum, memory release calls and exit. Use the median of three separate `wait4` runs for process peak RSS. Use Massif for the peak of the heap with and without allocator overhead.

| Script | glibc base [spread] | glibc patch [spread] | jemalloc base [spread] | jemalloc patch [spread] |
|---|---:|---:|---:|---:|
| `calls_1` | 69,105,816 [379] | 65,709,137 [194] | 72,477,795 [894] | 69,080,746 [469] |
| `calls_10` | 682,906,315 [933] | 648,908,405 [861] | 686,307,641 [750] | 652,310,094 [1,356] |
| `loop_1` | 86,847,593 [338] | 79,048,818 [1,305] | 90,221,448 [538] | 82,423,362 [1,303] |
| `loop_10` | 860,846,546 [1,111] | 782,848,874 [580] | 864,250,960 [1,105] | 786,253,930 [1,003] |
| `empty_1` | 710,821 [723] | 710,943 [605] | 4,054,231 [1,182] | 4,054,029 [672] |
| `local_call` | 524,836,729 [507] | 520,837,179 [225] | 528,234,938 [748] | 524,235,224 [759] |
| `local_loop` | 468,789,805 [456] | 450,789,524 [598] | 472,158,710 [1,429] | 454,158,505 [650] |
| `fib_10` | 34,361,561 [543] | 34,316,098 [889] | 37,762,829 [1,659] | 37,718,333 [1,570] |
| `closures_10` | 120,642,521 [1,209] | 116,897,711 [654] | 124,013,359 [1,559] | 120,268,658 [272] |
| `throws_10` | 282,982,756 [1,470] | 273,058,163 [1,044] | 286,389,441 [726] | 276,464,596 [562] |
| `objects_10` | 571,239,595 [96,770] | 558,808,615 [126,233] | 574,705,099 [355,815] | 562,315,633 [143,108] |
| `webpack_10` | 43,800,661 [10,053] | 43,719,090 [4,185] | 46,967,291 [4,932] | 46,884,772 [8,615] |

| Script | Executable RSS, KiB | Anonymous RSS, KiB | wait4 RSS, KiB | Peak heap, bytes | Heap with overhead, bytes |
|---|---:|---:|---:|---:|---:|
| `calls_10` | 1,860 → 1,924 | 916 → 912 | 5,304 → 5,432 | 42,275 → 42,275 | 43,760 → 43,760 |
| `loop_10` | 1,924 → 1,924 | 916 → 912 | 5,432 → 5,432 | 41,494 → 41,494 | 42,848 → 42,832 |

| `interp_load` row | Default glibc / jemalloc | One codegen unit glibc / jemalloc |
|---|---:|---:|
| `loopfn` | -2.440% / -2.426% | -2.440% / -2.426% |
| `calls` | +0.532% / +0.528% | +0.532% / +0.528% |
| `fib` | +0.127% / +0.117% | +0.124% / +0.117% |
| `closures` | -0.661% / -0.636% | -0.660% / -0.636% |
| `objects` | -1.905% / -1.835% | -1.905% / -1.834% |

| Matrix row | Default glibc / jemalloc | One codegen unit glibc / jemalloc |
|---|---:|---:|
| `js2_gen_load next 100000` | -0.295% / -0.285% | -0.296% / -0.285% |
| `js2_lex_load small 1 1` | -0.000% / +0.004% | +0.001% / +0.004% |
| `js2_parse_load full stmts 1` | +0.002% / +0.025% | +0.037% / +0.003% |
| `js2_promise_load then 100000` | -0.457% / -0.438% | -0.458% / -0.438% |

Executable RSS grew by 64 KiB on calls and stayed flat on loop. Shared executable mappings stayed flat on both rows. Peak requested heap stayed at 42,275 bytes on calls and 41,494 bytes on loop.

Typecheck passed for `bare-js2` and `bare`. The narrow run passed 40 interpreter, 2 runtime, and 17 verifier tests. These counts include feedback rebuild, cache refill, and narrow and wide site validation tests.

Targeted test262 covered seven directories for assignments, global code, identifier resolution, and variable declarations. Both trees had 650 pass, 53 fail, and 16 skip. Comparing the passing filenames found zero lost passes.

## Cost debt

`interp_load calls` grew by 0.532% on glibc and 0.528% on jemalloc. The one-codegen-unit control had the same deltas at the shown precision. Hypothesis: the hot dispatch layout changes the instruction count of this function-local call workload. The other eight cost rows pass.

Untested locally include the full workspace suite, browser journeys, Windows, and Miri.
