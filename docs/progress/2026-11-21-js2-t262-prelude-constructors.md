# Make prelude built-ins non-constructible

Prelude built-ins use ordinary function declarations for their bodies. Their stubs have no construct slot. Compilation preserves that flag after the first call.

## Test262

This change fixes 22 old-only cases. The full baseline has 26,031 old passes, 26,529 new passes and 308 old-only cases.

## Cost

User-mode instruction counts from `perf stat` on archMachine. Each cell is the minimum of three release runs. Base is the JSON callback change. Startup runs `script_run` on `var x = 1;`. No row exceeds +0.5%.

| Row | glibc base | glibc new | Change | jemalloc base | jemalloc new | Change |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| `startup` | 732,035 | 732,309 | +0.037% | 4,435,645 | 4,436,243 | +0.013% |
| `loopfn` | 887,309,917 | 887,310,328 | +0.000% | 891,074,142 | 891,073,507 | -0.000% |
| `calls` | 602,368,800 | 602,368,957 | +0.000% | 606,099,468 | 606,099,810 | +0.000% |
| `fib` | 262,983,546 | 262,984,411 | +0.000% | 266,746,837 | 266,746,472 | -0.000% |
| `closures` | 1,974,217,785 | 1,974,216,332 | -0.000% | 1,977,854,724 | 1,977,836,307 | -0.001% |
| `objects` | 713,146,171 | 713,146,003 | -0.000% | 716,906,209 | 716,905,889 | -0.000% |

## Tests

The constructor test failed before the fix. It passes before and after the first call to the built-in. Ten prelude tests and the js2 typecheck pass. The baseline and cost runs preceded the merge of #871. Windows, Miri and browser journeys were not run locally.
