# 2026-11-21: Freeze js2 tagged template objects

Freeze both template arrays before the tag first receives them. Keep the raw property and the indexed elements read-only.

The pinned test262 subset has 26738 js2 passes and 114 old-only cases. This closes 3 old-only cases. No js2 test pass was lost. All three focused tagged template cases pass.

## Cost

archMachine, release profile, minimum of three instruction-count runs with perf. Base: `f7eae9a8`. Startup runs `var x = 1;`. The input is 3,000,000 for `loopfn`, 1,000,000 for `calls`, `closures` and `objects`, and 27 for `fib`. The largest spread is 0.172%.

| Row | glibc base | glibc new | Change | jemalloc base | jemalloc new | Change |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| `startup` | 734,079 | 733,913 | -0.023% | 4,476,008 | 4,475,788 | -0.005% |
| `loopfn` | 779,302,293 | 779,302,671 | +0.000% | 783,104,257 | 783,104,870 | +0.000% |
| `calls` | 536,352,956 | 536,353,449 | +0.000% | 540,122,552 | 540,123,178 | +0.000% |
| `fib` | 240,089,483 | 240,089,423 | -0.000% | 243,891,680 | 243,891,127 | -0.000% |
| `closures` | 1,869,667,647 | 1,869,668,008 | +0.000% | 1,873,344,819 | 1,873,343,783 | -0.000% |
| `objects` | 705,194,607 | 705,194,112 | -0.000% | 708,995,101 | 708,994,704 | -0.000% |

Not tested: Windows, Miri or browser journeys.
