# Give primitive prototypes their wrapped values

`Number.prototype`, `Boolean.prototype` and `String.prototype` hold the values 0, false and the empty string. They use the same wrapper layout as instances. An object that inherits from a primitive prototype has no wrapped value of its own.

## Test262

The focused run on main at `d4777ff0` had 58 cases that pass on the old engine only. This change fixes 52. It fixes all 37 cases in `Number/prototype/toString`. The cases failed before the engine could format the number because the receiver had no wrapped value.

The full run of `tools/js/test262_js2.sh -j4 -u` has 26,031 old passes and 26,338 new passes. There are 495 old-only passes. The refreshed table includes changes already on main from #859. The generated RegExp property escape directory had wall-clock timeouts on both engines.

## Cost

User-mode instruction counts from `perf stat` on archMachine. Each cell is the minimum of three release runs. Base is `d4777ff0`. Startup runs `script_run` on `var x = 1;`. The five loop rows run `interp_load`. No row exceeds +0.5%.

| Row | glibc base | glibc new | Change | jemalloc base | jemalloc new | Change |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| `startup` | 729,263 | 730,657 | +0.191% | 4,429,561 | 4,431,631 | +0.047% |
| `loopfn` | 887,307,397 | 887,309,050 | +0.000% | 891,067,942 | 891,069,919 | +0.000% |
| `calls` | 599,365,069 | 599,367,504 | +0.000% | 603,125,087 | 603,127,880 | +0.000% |
| `fib` | 262,979,968 | 262,982,472 | +0.001% | 266,741,254 | 266,742,305 | +0.000% |
| `closures` | 1,971,227,587 | 1,971,215,530 | -0.001% | 1,974,888,813 | 1,974,891,049 | +0.000% |
| `objects` | 713,141,529 | 713,145,099 | +0.001% | 716,899,331 | 716,902,501 | +0.000% |

## Tests

The regression test failed before the fix. The 15 tests in `builtins_more_tests` and `cargo check --locked -p bare-js2` pass. Windows, Miri and browser journeys were not run locally.
