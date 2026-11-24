# Apply ArraySetLength in property assignments

An assignment rejects a read-only length before it converts the assigned value. A failed deletion makes the assignment fail. A descriptor uses both specified conversions and accepts the maximum array length.

## Test262

This change fixes 13 old-only cases. The full baseline has 26,031 old passes, 26,601 new passes and 238 old-only cases.

## Cost

User-mode instruction counts from `perf stat` on archMachine. Each cell is the minimum of three release runs. Base is the numeric string change. Startup runs `script_run` on `var x = 1;`. No row exceeds +0.5%.

| Row | glibc base | glibc new | Change | jemalloc base | jemalloc new | Change |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| `startup` | 732,660 | 732,975 | +0.043% | 4,436,285 | 4,436,270 | -0.000% |
| `loopfn` | 887,311,141 | 887,311,242 | +0.000% | 891,074,508 | 891,074,571 | +0.000% |
| `calls` | 602,368,567 | 602,368,678 | +0.000% | 606,100,343 | 606,100,353 | +0.000% |
| `fib` | 262,984,561 | 262,984,665 | +0.000% | 266,746,679 | 266,746,934 | +0.000% |
| `closures` | 1,974,230,547 | 1,974,231,417 | +0.000% | 1,977,834,394 | 1,977,836,549 | +0.000% |
| `objects` | 713,144,819 | 713,144,965 | +0.000% | 716,906,942 | 716,906,548 | -0.000% |

## Tests

The length regression failed before the fix. The focused array tests, differential corpus and js2 typecheck pass. The isolated cost run preceded #861 and #866. Windows, Miri and browser journeys were not run locally.

## Combined stack after the main merge

The pinned subset still has 26,031 old passes, 26,601 new passes and 238 old-only cases after #861 and #866. The baseline comes from that run. The full stack fixes 309 old-only passes across ten changes.

Base is main at `8d5da50e`. Both builds use the release profile from #866. Each cell is the minimum of three user-mode instruction counts on archMachine. No row exceeds +0.5%.

| Row | glibc base | glibc new | Change | jemalloc base | jemalloc new | Change |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| `startup` | 727,157 | 727,459 | +0.042% | 4,428,986 | 4,429,760 | +0.017% |
| `loopfn` | 821,292,709 | 821,295,908 | +0.000% | 825,055,329 | 825,057,724 | +0.000% |
| `calls` | 551,342,973 | 552,346,381 | +0.182% | 555,074,458 | 556,077,140 | +0.181% |
| `fib` | 246,754,408 | 247,393,654 | +0.259% | 250,516,712 | 251,155,369 | +0.255% |
| `closures` | 1,895,676,098 | 1,896,665,700 | +0.052% | 1,899,274,119 | 1,900,284,660 | +0.053% |
| `objects` | 707,183,987 | 710,186,413 | +0.425% | 710,944,355 | 713,947,316 | +0.422% |
