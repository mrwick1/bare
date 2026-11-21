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

## Build large constant trees from a recipe

The old engine's `data_lit::scan` keeps a source span for a JSON-shaped expression. `Build::value` reads the span directly into heap values. The V8 team also measured lower cold-load cost for large JSON data decoded once. [V8 JSON measurements](https://v8.dev/blog/cost-of-javascript-2019#json)

js2 still parses and checks the source as JavaScript. After the first change, the general compiler still generates constant loads, property or element writes, and their sites and root maps. The VM dispatches those instructions on each evaluation.

- **Heap template.** Build one tree of ordinary objects and arrays per realm on first use. Store and trace O(n) template cells and slots. A deep clone takes O(n) time on each evaluation. The first evaluation constructs the template and allocates the clone. V8's compiler also prepares boilerplate data from literals. [V8 background compilation](https://v8.dev/blog/background-compilation)
- **Flat recipe.** Lower the parsed tree in O(n) time to one contiguous slice of tagged scalars, shared strings and container counts. Store O(n) metadata outside the realm heap. Build each result in O(n) time with a bounded call stack and one reusable scratch buffer for open arrays. Each evaluation allocates the result containers once.

Use the recipe for constant object and array literals of at least 1 KiB. It removes all per-entry bytecode and feedback. It also avoids template allocation, tracing and cloning on the first evaluation of a large script such as the Python search index. The parser still checks the same source and performs the same early-error checks. The compiler uses the general path for a prototype setter, a computed key, a hole, a spread, an accessor or another dynamic expression.

`NewDataLit` runs behind one safepoint. The builder uses the normal property definition and array allocation methods. It defines keys in source order and builds fresh containers on every evaluation. The tests check duplicate keys, numeric key order, attributes, prototypes, negative zero, a lone surrogate, nested identity under frequent collection, and the general path for holes and an escaped prototype setter.

### Python documentation journeys

Use the same machine and release settings as above. Compare `547e3f0f` with `7e706817`. Run `bare-drive` against the live `tools/drive/public/python-docs-search*.steps` inputs with `BARE_JS_EXEC=new` and `BARE_SEED=1`. All three runs per version passed each journey. The instruction counts include the whole driver process.

| Journey | Before, M (min–max) | After, M (min–max) | Change at minimum |
| --- | ---: | ---: | ---: |
| `python-docs-search` | 2,593.3–2,598.4 | 1,405.7–1,408.7 | -45.8% |
| `python-docs-search-results` | 3,420.8–4,245.8 | 2,389.4–2,871.1 | -30.2% |

The results journey recorded the same 879,258-byte search index reply in all six runs. Other live replies varied. The search journey checks navigation and the page title. Its step log did not record the index reply. The driver reported median peak RSS of 116.4 to 71.1 MiB for search and 116.3 to 79.8 MiB for search results under the instruction-run allocator settings.

### Controls

Use the same inputs and method as the first change. The frozen Python index has 553 documents and SHA-256 `291d8f2385e08688f4ac16e61059bcf551607d60d85bf5a4f3ee9367dc1621e3`.

| Row | glibc before | glibc after | Change | jemalloc before | jemalloc after | Change | Max spread |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| `startup` | 729,055 | 728,393 | -0.091% | 4,069,234 | 4,068,459 | -0.019% | 897 |
| `object_1000` | 9,665,206 | 3,963,254 | -58.995% | 13,210,311 | 7,466,552 | -43.479% | 71,142 |
| `object_2000` | 18,656,083 | 7,221,473 | -61.292% | 22,251,477 | 10,748,289 | -51.696% | 31,743 |
| `object_4000` | 36,561,510 | 13,595,871 | -62.814% | 40,185,422 | 17,164,967 | -57.286% | 89,044 |
| `object_8000` | 72,289,140 | 26,323,821 | -63.585% | 76,015,264 | 29,966,868 | -60.578% | 31,205 |
| `bundle_like` | 126,960,660 | 127,045,918 | +0.067% | 130,629,217 | 130,712,978 | +0.064% | 16,141 |
| `eval_calls` | 71,156,380 | 71,157,250 | +0.001% | 74,531,926 | 74,531,313 | -0.001% | 1,120 |
| `eval_closures` | 12,875,243 | 12,875,663 | +0.003% | 16,257,706 | 16,256,829 | -0.005% | 30,147 |
| `eval_concat` | 11,720,363 | 11,719,171 | -0.010% | 15,068,208 | 15,067,512 | -0.005% | 1,691 |
| `eval_fib` | 4,014,491 | 4,017,985 | +0.087% | 7,384,677 | 7,388,787 | +0.056% | 770 |
| `eval_loop` | 87,007,501 | 87,004,886 | -0.003% | 90,353,004 | 90,353,308 | +0.000% | 1,396 |
| `eval_loopfn` | 47,839,163 | 47,838,077 | -0.002% | 51,206,141 | 51,204,896 | -0.002% | 1,297 |
| `eval_objects` | 66,923,551 | 67,022,665 | +0.148% | 70,472,766 | 70,538,526 | +0.093% | 35,294 |
| `eval_throws` | 29,473,280 | 29,479,806 | +0.022% | 32,862,093 | 32,839,008 | -0.070% | 32,565 |
| `eval_webpack` | 6,661,080 | 6,674,791 | +0.206% | 10,206,710 | 10,211,105 | +0.043% | 1,919 |
| `micro_loopfn_1000000` | 291,500,972 | 291,500,969 | -0.000% | 291,530,004 | 291,530,917 | +0.000% | 32,011 |
| `micro_calls_1000000` | 588,500,727 | 591,501,599 | +0.510% | 588,561,088 | 591,560,983 | +0.510% | 3,440 |
| `micro_fib_28` | 419,995,158 | 419,994,754 | -0.000% | 420,056,891 | 420,055,812 | -0.000% | 31,475 |
| `micro_closures_100000` | 164,616,562 | 164,716,233 | +0.061% | 164,646,051 | 164,746,127 | +0.061% | 759 |
| `micro_objects_100000` | 70,573,431 | 70,572,427 | -0.001% | 70,608,452 | 70,608,497 | +0.000% | 1,155 |
| `python_payload` | 1,938,370,108 | 739,911,348 | -61.828% | 1,986,401,306 | 747,684,089 | -62.360% | 373,936 |

The default `interp_load calls` row rose by 0.510% on both allocators. With `-C codegen-units=1`, glibc went from 565,500,926 to 565,500,462 instructions, and jemalloc went from 565,530,363 to 565,530,192. Both changes are within 0.001%. The row uses hand-written bytecode. Hypothesis: the additional opcode changes register allocation or layout in the interpreter. All other startup, eval and micro rows were within 0.21% in the default build.

### Checks

`cargo test --locked -p bare-js2 --lib compile` passed 105 tests on the measured revision. `cargo check --locked -p bare-js2` passed.
`TEST262=/data/claude-tmp/test262 tools/js/test262_js2.sh -u` reported 26,263 js2 passes, the same count as the matched baseline. Two generated RegExp tests hit the changed run's directory timeout. Both passed on separate checks. The run lost zero passes after those checks.

Windows and Miri were not run locally.
