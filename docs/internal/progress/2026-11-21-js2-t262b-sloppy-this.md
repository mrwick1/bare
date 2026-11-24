# 2026-11-21: Box sloppy js2 receivers

Box a primitive receiver on the first sloppy this read. Reuse the wrapper on later reads in the same call. The allocation runs on the cold path.

The pinned test262 subset has 26705 js2 passes and 137 old-only cases. This closes 10 old-only cases. No js2 test pass was lost. The first hot-dispatch version raised loopfn by 4.018% with glibc and 3.999% with jemalloc. The cold path removes that increase.

## Cost

archMachine, release profile, minimum of three instruction-count runs with perf. Base: `a4add820`. Startup runs `var x = 1;`. The input is 3,000,000 for `loopfn`, 1,000,000 for `calls`, `closures` and `objects`, and 27 for `fib`. The largest spread is 0.092%.

| Row | glibc base | glibc new | Change | jemalloc base | jemalloc new | Change |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| `startup` | 731,991 | 731,626 | -0.050% | 4,472,680 | 4,472,252 | -0.010% |
| `loopfn` | 821,301,257 | 821,299,638 | -0.000% | 825,068,462 | 825,066,515 | -0.000% |
| `calls` | 552,352,495 | 552,350,308 | -0.000% | 556,088,427 | 556,085,523 | -0.001% |
| `fib` | 247,399,480 | 247,397,024 | -0.001% | 251,199,872 | 251,197,474 | -0.001% |
| `closures` | 1,896,671,037 | 1,896,668,726 | -0.000% | 1,900,288,416 | 1,900,286,430 | -0.000% |
| `objects` | 710,193,861 | 710,191,796 | -0.000% | 713,992,589 | 713,989,847 | -0.000% |

Not tested: Windows, Miri or browser journeys.
