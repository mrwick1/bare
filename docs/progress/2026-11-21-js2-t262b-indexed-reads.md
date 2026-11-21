# 2026-11-21: js2 indexed prototype reads

Create Array.prototype as an array. Read the length and indexed elements that a prototype supplies. Recognize the virtual own keys of String wrappers in property existence checks.

The pinned test262 subset has 26,639 js2 passes and 200 old-only cases. This change closes 21 old-only cases. No js2 test pass was lost.

## Cost

archMachine, release profile, minimum of three instruction-count runs with perf. Base: `7deb1ed3`. Startup runs `var x = 1;`. The input is 3,000,000 for `loopfn`, 1,000,000 for `calls`, `closures` and `objects`, and 27 for `fib`. The largest spread is 0.135%.

| Row | glibc base | glibc new | Change | jemalloc base | jemalloc new | Change |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| `startup` | 728,239 | 733,441 | +0.714% | 4,432,132 | 4,440,135 | +0.181% |
| `loopfn` | 821,297,351 | 821,298,741 | +0.000% | 825,059,573 | 825,067,076 | +0.001% |
| `calls` | 552,347,840 | 552,350,165 | +0.000% | 556,079,493 | 556,085,710 | +0.001% |
| `fib` | 247,394,317 | 247,396,035 | +0.001% | 251,157,050 | 251,164,287 | +0.003% |
| `closures` | 1,896,680,622 | 1,896,670,078 | -0.001% | 1,900,284,365 | 1,900,285,771 | +0.000% |
| `objects` | 710,188,253 | 710,191,646 | +0.000% | 713,949,597 | 713,990,730 | +0.006% |

Not tested: Windows, Miri or browser journeys.
