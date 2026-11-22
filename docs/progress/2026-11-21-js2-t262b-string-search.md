# 2026-11-21: Reject RegExp search values in js2 String methods

Apply the RegExp check before converting the search value in startsWith, endsWith and includes. Respect a Symbol.match override.

The pinned test262 subset has 26747 js2 passes and 105 old-only cases. This closes 1 old-only cases. No js2 test pass was lost. The focused startsWith case passes.

## Cost

archMachine, release profile, minimum of three instruction-count runs with perf. Base: `65340f92`. Startup runs `var x = 1;`. The input is 3,000,000 for `loopfn`, 1,000,000 for `calls`, `closures` and `objects`, and 27 for `fib`. The largest spread is 0.143%.

| Row | glibc base | glibc new | Change | jemalloc base | jemalloc new | Change |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| `startup` | 736,265 | 734,513 | -0.238% | 4,478,153 | 4,476,444 | -0.038% |
| `loopfn` | 779,302,974 | 779,302,823 | -0.000% | 783,105,499 | 783,104,258 | -0.000% |
| `calls` | 536,353,136 | 536,353,509 | +0.000% | 540,123,124 | 540,122,712 | -0.000% |
| `fib` | 240,089,156 | 240,089,413 | +0.000% | 243,892,227 | 243,891,389 | -0.000% |
| `closures` | 1,869,667,713 | 1,869,667,525 | -0.000% | 1,873,344,533 | 1,873,345,892 | +0.000% |
| `objects` | 705,194,255 | 705,194,677 | +0.000% | 708,994,737 | 708,994,439 | -0.000% |

Not tested: Windows, Miri or browser journeys.
