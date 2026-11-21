# 2026-11-21: Complete js2 function prototype properties

Make Function.prototype callable and give it length and name in order. Materialize the own prototype of ordinary functions before lookup. Dynamic Function constructors use the new target prototype.

The pinned test262 subset has 26722 js2 passes and 129 old-only cases. This closes 8 old-only cases. No js2 test pass was lost. The focused Function constructor unit test passes.

## Cost

archMachine, release profile, minimum of three instruction-count runs with perf. Base: `8a81dd56`. Startup runs `var x = 1;`. The input is 3,000,000 for `loopfn`, 1,000,000 for `calls`, `closures` and `objects`, and 27 for `fib`. The largest spread is 0.136%.

| Row | glibc base | glibc new | Change | jemalloc base | jemalloc new | Change |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| `startup` | 731,924 | 733,202 | +0.175% | 4,472,277 | 4,474,276 | +0.045% |
| `loopfn` | 821,298,853 | 821,301,543 | +0.000% | 825,066,576 | 825,103,102 | +0.004% |
| `calls` | 552,349,777 | 552,351,798 | +0.000% | 556,085,167 | 556,121,212 | +0.006% |
| `fib` | 247,396,762 | 247,398,232 | +0.001% | 251,197,783 | 251,199,447 | +0.001% |
| `closures` | 1,896,666,311 | 1,896,993,854 | +0.017% | 1,900,286,866 | 1,900,343,810 | +0.003% |
| `objects` | 710,192,155 | 710,193,999 | +0.000% | 713,990,281 | 713,993,035 | +0.000% |

Not tested: Windows, Miri or browser journeys.
