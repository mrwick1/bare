# 2026-11-21: Bind js2 super receivers before fields

Bind the result of super before running derived field initializers. Observe accessors during heritage lookup and preserve the reference error from deleting a super property.

The pinned test262 subset has 26731 js2 passes and 121 old-only cases. This closes 8 old-only cases. No js2 test pass was lost. All seven focused super unit tests pass.

## Cost

archMachine, release profile, minimum of three instruction-count runs with perf. Base: `73b7970f`. Startup runs `var x = 1;`. The input is 3,000,000 for `loopfn`, 1,000,000 for `calls`, `closures` and `objects`, and 27 for `fib`. The largest spread is 0.061%.

| Row | glibc base | glibc new | Change | jemalloc base | jemalloc new | Change |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| `startup` | 733,332 | 734,196 | +0.118% | 4,474,179 | 4,476,334 | +0.048% |
| `loopfn` | 821,301,822 | 779,302,382 | -5.114% | 825,102,612 | 783,105,800 | -5.090% |
| `calls` | 552,351,390 | 536,352,968 | -2.896% | 556,121,441 | 540,122,600 | -2.877% |
| `fib` | 247,397,968 | 240,089,826 | -2.954% | 251,199,963 | 243,891,573 | -2.909% |
| `closures` | 1,896,996,064 | 1,869,667,280 | -1.441% | 1,900,343,233 | 1,873,345,275 | -1.421% |
| `objects` | 710,194,100 | 705,194,743 | -0.704% | 713,992,873 | 708,994,264 | -0.700% |

Not tested: Windows, Miri or browser journeys.
