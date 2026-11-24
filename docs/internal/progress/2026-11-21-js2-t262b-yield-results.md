# 2026-11-21: Forward js2 delegated iterator results

Return the original iterator result from a suspended yield star. Read its value only when the iterator finishes or a value consumer asks for it. Open the delegated iterator on the cold path.

The pinned test262 subset has 26742 js2 passes and 110 old-only cases. This closes 4 old-only cases. No js2 test pass was lost. The nine focused yield unit tests pass. Moving iterator setup to the cold path removed the measured hot-dispatch increase.

## Cost

archMachine, release profile, minimum of three instruction-count runs with perf. Base: `6d7fe938`. Startup runs `var x = 1;`. The input is 3,000,000 for `loopfn`, 1,000,000 for `calls`, `closures` and `objects`, and 27 for `fib`. The largest spread is 0.064%.

| Row | glibc base | glibc new | Change | jemalloc base | jemalloc new | Change |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| `startup` | 734,786 | 737,166 | +0.324% | 4,475,643 | 4,478,368 | +0.061% |
| `loopfn` | 779,302,425 | 779,302,491 | +0.000% | 783,105,501 | 783,105,410 | -0.000% |
| `calls` | 536,352,837 | 536,353,353 | +0.000% | 540,122,897 | 540,122,785 | -0.000% |
| `fib` | 240,089,766 | 240,089,079 | -0.000% | 243,891,691 | 243,892,430 | +0.000% |
| `closures` | 1,869,665,640 | 1,869,667,594 | +0.000% | 1,873,343,263 | 1,873,345,333 | +0.000% |
| `objects` | 705,194,761 | 705,194,741 | -0.000% | 708,994,021 | 708,994,875 | +0.000% |

Not tested: Windows, Miri or browser journeys.
