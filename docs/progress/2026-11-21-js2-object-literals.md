# 2026-11-21 — Reduce the cost of js2 object literals

## Skip the unused duplicate-key scan

Only the compact `NewObjLit` opcode needs a list of unique names. It takes up to 64 properties. The compiler now scans only literals within that limit and stops at the first key that requires individual property definitions. Large literals go straight to those definitions. Each definition runs in source order. A repeated key still uses the last value, and prototype setters and accessors keep their own operations.

The 2,500-to-20,000-key compile test took 124 ms to 6.38 s before the change, a 52× increase. It passed the 16× limit after the change. The test uses a dynamic value to exercise the general compiler path. The semantics test covers duplicate data keys, an accessor overwrite, enumeration order and a prototype setter on both sides of the 64-property limit.

Release builds on archMachine, Ryzen 9 5900HX, Linux 7.2.15-zen1-1-zen, rustc 1.96.0. Compare `da9ba9af` with `55007294`. Counts are the minimum of three `perf stat -e instructions:u` runs per allocator, with the glibc cache setting from `tools/js/instr.sh`. The spread is the largest max minus min of the four groups of three runs. The object inputs have distinct quoted keys and check the last value. The eval rows use the checked-in scripts and Node's expected results. Startup runs `var x = 1;`. The `interp_load` rows subtract the zero-turn minimum. The Python payload runs the captured `searchindex.js` and checks its document count with a stub `Search.setIndex`.

| Row | glibc before | glibc after | Change | jemalloc before | jemalloc after | Change | Max spread |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| `startup` | 727,019 | 727,946 | +0.128% | 4,067,379 | 4,067,726 | +0.009% | 934 |
| `object_1000` | 27,123,432 | 9,651,894 | -64.415% | 30,640,147 | 13,201,986 | -56.913% | 30,608 |
| `object_2000` | 86,409,049 | 18,630,633 | -78.439% | 90,043,286 | 22,253,850 | -75.285% | 30,207 |
| `object_4000` | 303,505,231 | 36,509,471 | -87.971% | 307,202,886 | 40,101,131 | -86.946% | 65,925 |
| `object_8000` | 1,133,853,774 | 72,206,301 | -93.632% | 1,135,939,264 | 75,904,989 | -93.318% | 149,888 |
| `bundle_like` | 127,323,716 | 127,326,409 | +0.002% | 130,962,483 | 130,963,584 | +0.001% | 43,614 |
| `eval_calls` | 71,154,924 | 71,155,955 | +0.001% | 74,529,366 | 74,530,165 | +0.001% | 1,114 |
| `eval_closures` | 12,872,848 | 12,873,558 | +0.006% | 16,254,035 | 16,255,290 | +0.008% | 30,303 |
| `eval_concat` | 11,716,976 | 11,716,249 | -0.006% | 15,064,775 | 15,066,599 | +0.012% | 3,059 |
| `eval_fib` | 4,011,746 | 4,012,211 | +0.012% | 7,381,850 | 7,382,749 | +0.012% | 1,505 |
| `eval_loop` | 87,003,673 | 87,003,847 | +0.000% | 90,350,450 | 90,351,213 | +0.001% | 924 |
| `eval_loopfn` | 47,836,235 | 47,837,502 | +0.003% | 51,203,307 | 51,203,564 | +0.001% | 1,367 |
| `eval_objects` | 67,146,545 | 67,146,770 | +0.000% | 70,666,853 | 70,665,006 | -0.003% | 68,802 |
| `eval_throws` | 29,477,921 | 29,478,274 | +0.001% | 32,837,787 | 32,866,902 | +0.089% | 28,160 |
| `eval_webpack` | 6,652,950 | 6,650,791 | -0.032% | 10,192,189 | 10,188,328 | -0.038% | 2,429 |
| `micro_loopfn_1000000` | 291,500,548 | 291,499,821 | -0.000% | 291,531,559 | 291,531,234 | -0.000% | 30,935 |
| `micro_calls_1000000` | 588,500,863 | 588,500,369 | -0.000% | 588,530,712 | 588,561,946 | +0.005% | 31,637 |
| `micro_fib_28` | 419,995,392 | 419,994,013 | -0.000% | 420,054,989 | 420,086,907 | +0.008% | 2,026 |
| `micro_closures_100000` | 164,616,668 | 164,615,755 | -0.001% | 164,646,265 | 164,645,668 | -0.000% | 872 |
| `micro_objects_100000` | 70,574,821 | 70,574,656 | -0.000% | 70,608,410 | 70,609,500 | +0.002% | 584 |
| `python_payload` | 6,517,678,518 | 1,934,005,338 | -70.327% | 6,569,630,511 | 1,982,033,322 | -69.830% | 887,272 |

The 8,000-key control took 7.48× the instructions of the 1,000-key control after the change. Startup, eval and micro rows were within 0.13% of the base on both allocators.

`cargo test --locked -p bare-js2 --lib compile` passed 103 tests on the measured revision.
`cargo check --locked -p bare-js2` passed.
test262 reported 26,122 js2 passes before and after on the same 779 directories. Two generated RegExp tests hit the directory timeout in the changed run. Both passed when run separately with the changed binary. The comparison lost zero passes after those checks.

After the merge of #853, CI found three remaining RegExp checks that used the removed flag. These sites now use the existing object kind check.

Windows and Miri were not run locally.
