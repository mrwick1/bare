# 2026-11-21: js2 indexed writes and array method order

Run an inherited setter for a missing index. Propagate write errors from built-ins. Box primitive Array receivers and read indexes in the order set by each method. Sort scratch lists and the differential output buffer create own elements.

The pinned test262 subset has 26,661 js2 passes and 178 old-only cases. This closes 22 cases from the preceding snapshot. Two old-engine Promise cases also pass after the differential print fix. One generated RegExp test timed out in the directory run and passed when run alone. The generated baseline uses the retry. The prelude facts checks pass.

## Cost

archMachine, release profile, minimum of three instruction-count runs with perf. Base: `24eb8ec9`. Startup runs `var x = 1;`. The input is 3,000,000 for `loopfn`, 1,000,000 for `calls`, `closures` and `objects`, and 27 for `fib`. The largest spread is 0.044%.

| Row | glibc base | glibc new | Change | jemalloc base | jemalloc new | Change |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| `startup` | 733,713 | 731,262 | -0.334% | 4,442,162 | 4,439,165 | -0.067% |
| `loopfn` | 821,299,250 | 821,299,768 | +0.000% | 825,037,469 | 825,067,878 | +0.004% |
| `calls` | 552,350,053 | 552,349,950 | -0.000% | 556,087,561 | 556,086,341 | -0.000% |
| `fib` | 247,396,789 | 247,396,791 | +0.000% | 251,165,247 | 251,165,259 | +0.000% |
| `closures` | 1,896,667,640 | 1,896,670,366 | +0.000% | 1,900,287,189 | 1,900,284,981 | -0.000% |
| `objects` | 710,192,476 | 710,192,812 | +0.000% | 713,991,662 | 713,991,239 | -0.000% |

Not tested: Windows, Miri or browser journeys.
