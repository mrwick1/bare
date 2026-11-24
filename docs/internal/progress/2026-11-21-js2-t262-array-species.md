# Create array results through Symbol.species

Array methods read the constructor and Symbol.species in order. They respect custom constructors. Results use own data properties, including when an existing property is configurable and read-only. Array has its species getter.

## Test262

This change fixes 48 old-only cases. The full baseline has 26,031 old passes, 26,577 new passes and 260 old-only cases.

## Cost

User-mode instruction counts from `perf stat` on archMachine. Each cell is the minimum of three release runs. Base is the prelude constructor change. Startup runs `script_run` on `var x = 1;`. No row exceeds +0.5%.

| Row | glibc base | glibc new | Change | jemalloc base | jemalloc new | Change |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| `startup` | 731,274 | 731,885 | +0.084% | 4,433,775 | 4,435,213 | +0.032% |
| `loopfn` | 887,309,065 | 887,310,360 | +0.000% | 891,071,959 | 891,072,391 | +0.000% |
| `calls` | 602,368,100 | 602,367,873 | -0.000% | 606,097,921 | 606,098,834 | +0.000% |
| `fib` | 262,983,129 | 262,984,105 | +0.000% | 266,744,684 | 266,745,571 | +0.000% |
| `closures` | 1,974,218,074 | 1,974,231,776 | +0.001% | 1,977,852,536 | 1,977,853,980 | +0.000% |
| `objects` | 713,144,838 | 713,144,033 | -0.000% | 716,903,931 | 716,905,415 | +0.000% |

## Tests

The species regression failed before the fix. The attribute regression, prelude facts check and js2 typecheck pass. The final baseline recovers all four descriptor cases that the first species run lost. The baseline and cost runs preceded #861 and #866. Windows, Miri and browser journeys were not run locally.
