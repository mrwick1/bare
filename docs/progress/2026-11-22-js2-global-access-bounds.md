# 2026-11-22 — js2 global cache access

Use the verifier's global site bounds for hot cache lookups. Match the feedback cache length to the current code on entry and reload. Lazy compilation also resets feedback for the replaced stub. Run cache misses through the cold interpreter at the original instruction PC. The cold path retains the global name lookup, accessors, strict writes and reentry.

Baseline `744954298e5d0d294aab423ec780c39cac2a8855`. Implementation `c1aa04aef193c4297ce0920d755be42f551b058d`. Machine `archMachine`, Ryzen 9 5900HX, Linux 7.2.15-zen1-1-zen, Rust 1.96.0.

Use the pinned [script-completion inputs](2026-11-21-engine-decision-benchmark.md). The scale-10 calls and loop checksums are 35741 and 63. The release builds use locked dependencies. The workspace already builds js2 with one codegen unit. The control sets `RUSTFLAGS="-C codegen-units=1"` for all crates on both builds.

Report the minimum and spread of three fresh `perf stat -e instructions:u` runs with at least 99% counter coverage. Memory runs use glibc. Take the highest sampled RSS of the executable mappings from `/proc/PID/smaps` across three runs with ASLR disabled. Capture mappings at checksum, memory release calls and exit. Use the median of three separate `wait4` runs for process peak RSS. Use Massif for the peak of the heap with and without allocator overhead.

| Script | glibc base [spread] | glibc patch [spread] | jemalloc base [spread] | jemalloc patch [spread] |
|---|---:|---:|---:|---:|
| `calls_1` | 69,620,132 [942] | 65,420,943 [728] | 72,992,593 [486] | 68,793,607 [709] |
| `calls_10` | 688,820,052 [286] | 646,820,614 [750] | 692,222,507 [1,472] | 650,222,909 [3,402] |
| `loop_1` | 88,178,252 [43] | 78,577,712 [946] | 91,553,626 [733] | 81,953,181 [840] |
| `loop_10` | 874,778,253 [35] | 778,777,680 [630] | 878,183,675 [304] | 782,183,557 [993] |
| `empty_1` | 713,256 [512] | 711,251 [181] | 4,056,763 [569] | 4,054,010 [412] |
| `local_call` | 530,838,985 [886] | 518,836,749 [729] | 534,236,969 [1,070] | 522,236,261 [800] |
| `local_loop` | 474,792,097 [862] | 446,789,622 [801] | 478,161,209 [880] | 450,158,942 [489] |
| `fib_10` | 34,039,081 [307] | 34,130,119 [2,263] | 37,441,629 [1,555] | 37,531,879 [1,427] |
| `closures_10` | 119,769,888 [931] | 116,222,707 [853] | 123,142,878 [30,650] | 119,596,267 [435] |
| `throws_10` | 286,446,452 [1,339] | 271,782,434 [874] | 289,856,408 [628] | 275,192,893 [315] |
| `objects_10` | 590,007,418 [711,600] | 571,721,296 [422,695] | 593,490,352 [324,850] | 575,287,696 [360,023] |
| `webpack_10` | 43,171,680 [6,971] | 43,081,674 [6,343] | 46,351,006 [6,667] | 46,260,380 [7,104] |

| Script | Executable RSS, KiB | Anonymous RSS, KiB | wait4 RSS, KiB | Peak heap, bytes | Heap with overhead, bytes |
|---|---:|---:|---:|---:|---:|
| `calls_10` | 1,856 → 1,980 | 916 → 912 | 5,300 → 5,424 | 41,240 → 41,240 | 42,760 → 42,760 |
| `loop_10` | 1,856 → 1,980 | 916 → 912 | 5,300 → 5,424 | 39,956 → 39,956 | 41,312 → 41,296 |

| `interp_load` row | Default glibc / jemalloc | One codegen unit glibc / jemalloc |
|---|---:|---:|
| `loopfn` | -4.500% / -4.474% | -4.500% / -4.474% |
| `calls` | -1.400% / -1.392% | -1.400% / -1.392% |
| `fib` | +0.505% / +0.461% | +0.503% / +0.461% |
| `closures` | -0.426% / -0.409% | -0.426% / -0.410% |
| `objects` | -3.753% / -3.616% | -3.753% / -3.617% |

| Matrix row | Default glibc / jemalloc | One codegen unit glibc / jemalloc |
|---|---:|---:|
| `js2_gen_load next 100000` | -1.000% / -0.963% | -1.000% / -0.964% |
| `js2_lex_load small 1 1` | -0.000% / +0.003% | +0.000% / +0.003% |
| `js2_parse_load full stmts 1` | +0.031% / +0.038% | -0.032% / +0.016% |
| `js2_promise_load then 100000` | -0.784% / -0.752% | -0.784% / -0.752% |

The binary gained 124 KiB of executable RSS on both target rows. Across all executable mappings, including shared libraries, RSS went from 3,520 to 3,644 KiB. The owner accepts 128 KiB for Fix 1. This leaves 4 KiB for later steps. Peak requested heap stayed flat on both rows. All nine cost rows meet the rule. The glibc fib row exceeds 0.5% in both builds. The jemalloc fib row stays below the threshold in both builds.

Typecheck passed for `bare-js2` and `bare`. The narrow run passed 40 interpreter, 2 runtime, and 17 verifier tests. These counts include the feedback rebuild, cache refill, and narrow and wide site validation tests.

Targeted test262 covered seven directories for assignments, global code, identifier resolution, and variable declarations. Both trees had 650 pass, 53 fail, and 16 skip. The passing filename comparison found zero lost passes. The baseline files stay as recorded on main.

The final merge from `9c7ebd53` adds a progress document. The measured source stays at the comparison above.

Untested locally include the full workspace suite, browser journeys, Windows, and Miri.
