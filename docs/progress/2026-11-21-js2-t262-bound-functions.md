# Construct bound functions and set their metadata at bind time

A bound function is a constructor when its target is a constructor. A constructor call prepends the bound arguments in order and uses the target as `new.target` when the bound function was the new target. The bind operation reads the target length and name immediately. Prelude callbacks bind only the receiver.

## Test262

This change fixes 24 old-only cases. It fixes 20 in `Function/prototype/bind`, three in class subclassing and one in Promise. The full baseline has 26,031 old passes, 26,366 new passes and 471 old-only cases. No new pass was lost outside the generated RegExp property escape directory, which had wall-clock timeouts.

## Cost

User-mode instruction counts from `perf stat` on archMachine. Each cell is the minimum of three release runs. Base is the primitive prototype change. Startup runs `script_run` on `var x = 1;`. No row exceeds +0.5%.

| Row | glibc base | glibc new | Change | jemalloc base | jemalloc new | Change |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| `startup` | 731,344 | 731,120 | -0.031% | 4,433,910 | 4,434,261 | +0.008% |
| `loopfn` | 887,309,455 | 887,308,954 | -0.000% | 891,071,490 | 891,071,782 | +0.000% |
| `calls` | 599,368,260 | 599,368,566 | +0.000% | 603,097,721 | 603,098,139 | +0.000% |
| `fib` | 262,983,337 | 262,982,762 | -0.000% | 266,744,474 | 266,743,965 | -0.000% |
| `closures` | 1,971,218,439 | 1,971,217,894 | -0.000% | 1,974,892,024 | 1,974,892,341 | +0.000% |
| `objects` | 713,145,253 | 713,144,784 | -0.000% | 716,904,374 | 716,903,667 | -0.000% |

## Tests

The constructor regression failed before the fix. An array callback with a throwing metadata getter returned 7 on the base, threw after the initial fix, and returned 7 after the prelude fix. The 16 tests in `builtins_more_tests`, the generated prelude facts check and the js2 typecheck pass. Windows, Miri and browser journeys were not run locally.
