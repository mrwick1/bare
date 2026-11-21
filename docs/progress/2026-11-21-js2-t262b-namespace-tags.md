# 2026-11-21: js2 Math and JSON string tags

Define the own `Symbol.toStringTag` property on Math and JSON. The property is configurable. Array callbacks that receive these objects now see their correct tags.

All 17 focused test262 cases fail on the base and pass with this change. The pinned subset has 26,618 js2 passes and 221 old-only cases. No js2 test pass was lost.

## Cost

archMachine, release profile, minimum of three `perf stat -e instructions:u` runs per case. The base includes #882 at `0b4d249a`. Startup runs an empty script. The loop, call, closure, object and Fibonacci inputs are 3,000,000, 1,000,000, 1,000,000, 1,000,000 and 27.

| Row | glibc base | glibc new | Change | jemalloc base | jemalloc new | Change |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| `startup` | 728,719 | 729,151 | +0.059% | 4,431,776 | 4,433,308 | +0.035% |
| `loopfn` | 821,296,896 | 821,297,779 | +0.000% | 825,059,993 | 825,032,246 | -0.003% |
| `calls` | 552,347,499 | 552,348,482 | +0.000% | 556,079,881 | 556,080,477 | +0.000% |
| `fib` | 247,394,989 | 247,394,892 | -0.000% | 251,158,252 | 251,159,477 | +0.000% |
| `closures` | 1,896,666,494 | 1,896,681,314 | +0.001% | 1,900,284,982 | 1,900,284,671 | -0.000% |
| `objects` | 710,186,271 | 710,188,451 | +0.000% | 713,949,261 | 713,951,090 | +0.000% |

The largest within-case spread is 0.067%. No row exceeds the cost limit.

Not tested: Windows, Miri or browser journeys.
