# 2026-11-21: js2 arguments length

Store arguments.length as an ordinary data property. Keep indexed storage independent from that property. Function.apply uses the same array-like argument reads as Reflect.apply.

The pinned test262 subset has 26,695 js2 passes and 147 old-only cases. This closes 11 old-only cases. No js2 test pass was lost after retrying a generated RegExp timeout.

## Cost

archMachine, release profile, minimum of three instruction-count runs with perf. Base: `6f908410`. Startup runs `var x = 1;`. The input is 3,000,000 for `loopfn`, 1,000,000 for `calls`, `closures` and `objects`, and 27 for `fib`. The largest spread is 0.120%.

| Row | glibc base | glibc new | Change | jemalloc base | jemalloc new | Change |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| `startup` | 730,270 | 731,973 | +0.233% | 4,436,393 | 4,472,843 | +0.822% |
| `loopfn` | 821,298,323 | 821,301,170 | +0.000% | 825,065,754 | 825,068,353 | +0.000% |
| `calls` | 552,348,874 | 552,351,988 | +0.001% | 556,084,825 | 556,087,852 | +0.001% |
| `fib` | 247,395,681 | 247,399,479 | +0.002% | 251,162,692 | 251,199,345 | +0.015% |
| `closures` | 1,896,668,854 | 1,896,670,848 | +0.000% | 1,900,285,787 | 1,900,285,797 | +0.000% |
| `objects` | 710,191,395 | 710,193,866 | +0.000% | 713,989,264 | 713,992,484 | +0.000% |

Not tested: Windows, Miri or browser journeys.
