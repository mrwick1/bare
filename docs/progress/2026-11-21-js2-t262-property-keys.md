# Keep converted keys for property updates

A computed read followed by a write now keeps its converted key in a temporary register. It uses the same key for the write, even if the right-hand side changes the key variable. The indexed integer read still uses the direct element path.

## Test262

This change fixes 19 old-only cases. It fixes 12 compound assignments, three logical assignments and four increment or decrement cases. The full baseline run has 26,031 old passes, 26,385 new passes and 452 old-only cases.

## Cost

User-mode instruction counts from `perf stat` on archMachine. Each cell is the minimum of three release runs. Base is the bound function change. Startup runs `script_run` on `var x = 1;`. No row exceeds +0.5%.

| Row | glibc base | glibc new | Change | jemalloc base | jemalloc new | Change |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| `startup` | 732,028 | 731,984 | -0.006% | 4,435,211 | 4,434,956 | -0.006% |
| `loopfn` | 887,309,822 | 863,309,701 | -2.705% | 891,072,826 | 867,073,255 | -2.693% |
| `calls` | 599,368,476 | 595,369,098 | -0.667% | 603,099,796 | 599,099,079 | -0.663% |
| `fib` | 262,983,400 | 262,029,968 | -0.363% | 266,746,163 | 265,793,164 | -0.357% |
| `closures` | 1,971,216,308 | 1,973,648,021 | +0.123% | 1,974,834,365 | 1,977,265,589 | +0.123% |
| `objects` | 713,146,060 | 709,211,931 | -0.552% | 716,905,467 | 712,971,164 | -0.549% |

## Tests

The regression test failed before the fix. It passes with and without forced collection. The update bytecode test and js2 typecheck pass. The baseline and cost runs preceded the merge of the host setter change in #871. Windows, Miri and browser journeys were not run locally.
