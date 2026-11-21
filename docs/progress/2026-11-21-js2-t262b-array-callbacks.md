# 2026-11-21: Honor js2 Array callback and sort conversions

Check that an index exists before reading it for a skipping callback. The default sort comparison converts its two values with the string hint.

The pinned test262 subset has 26745 js2 passes and 107 old-only cases. This closes 3 old-only cases. No js2 test pass was lost. All three prelude facts checks pass.

## Cost

archMachine, release profile, minimum of three instruction-count runs with perf. Base: `e46c9247`. Startup runs `var x = 1;`. The input is 3,000,000 for `loopfn`, 1,000,000 for `calls`, `closures` and `objects`, and 27 for `fib`. The largest spread is 0.203%.

| Row | glibc base | glibc new | Change | jemalloc base | jemalloc new | Change |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| `startup` | 737,250 | 736,419 | -0.113% | 4,478,107 | 4,479,400 | +0.029% |
| `loopfn` | 779,302,387 | 779,303,187 | +0.000% | 783,104,532 | 783,105,046 | +0.000% |
| `calls` | 536,353,157 | 536,352,996 | -0.000% | 540,122,876 | 540,122,874 | -0.000% |
| `fib` | 240,089,480 | 240,089,867 | +0.000% | 243,891,715 | 243,891,969 | +0.000% |
| `closures` | 1,869,667,843 | 1,869,667,758 | -0.000% | 1,873,344,760 | 1,873,342,709 | -0.000% |
| `objects` | 705,194,290 | 705,194,574 | +0.000% | 708,994,657 | 708,994,771 | +0.000% |

Not tested: Windows, Miri or browser journeys.
