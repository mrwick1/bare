# Process Object.fromEntries entries in iterator order

The method reads and defines each entry before it asks the iterator for another one. If an entry throws, the method closes the iterator and keeps the original error.

## Test262

This change fixes 6 old-only cases. The full baseline has 26,031 old passes, 26,583 new passes and 254 old-only cases.

## Cost

User-mode instruction counts from `perf stat` on archMachine. Each cell is the minimum of three release runs. Base is the array species change. Startup runs `script_run` on `var x = 1;`. No row exceeds +0.5%.

| Row | glibc base | glibc new | Change | jemalloc base | jemalloc new | Change |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| `startup` | 732,984 | 735,328 | +0.320% | 4,436,397 | 4,438,880 | +0.056% |
| `loopfn` | 887,311,026 | 887,311,427 | +0.000% | 891,074,102 | 891,074,468 | +0.000% |
| `calls` | 602,369,143 | 602,369,255 | +0.000% | 606,100,462 | 606,100,973 | +0.000% |
| `fib` | 262,984,513 | 262,984,795 | +0.000% | 266,747,679 | 266,747,419 | -0.000% |
| `closures` | 1,974,232,210 | 1,974,232,152 | -0.000% | 1,977,856,704 | 1,977,866,601 | +0.001% |
| `objects` | 713,145,362 | 713,144,971 | -0.000% | 716,906,587 | 716,906,900 | +0.000% |

## Tests

The regression failed before the fix. The ordering and close cases and the js2 typecheck pass. The baseline and cost runs preceded #861 and #866. Windows, Miri and browser journeys were not run locally.
