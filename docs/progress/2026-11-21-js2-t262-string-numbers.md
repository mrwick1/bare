# Parse numeric strings with JavaScript whitespace and radix prefixes

Number conversion flattens a rope before it reads the text. It trims ECMAScript whitespace and handles unsigned binary, octal and hexadecimal text. Long prefixed integers round to the nearest double.

## Test262

This change fixes 3 old-only cases. The full baseline has 26,031 old passes, 26,586 new passes and 251 old-only cases.

## Cost

User-mode instruction counts from `perf stat` on archMachine. Each cell is the minimum of three release runs. Base is the Object.fromEntries change. Startup runs `script_run` on `var x = 1;`. No row exceeds +0.5%.

| Row | glibc base | glibc new | Change | jemalloc base | jemalloc new | Change |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| `startup` | 734,770 | 732,346 | -0.330% | 4,437,303 | 4,434,453 | -0.064% |
| `loopfn` | 887,310,297 | 887,309,726 | -0.000% | 891,072,932 | 891,072,212 | -0.000% |
| `calls` | 602,368,340 | 602,368,391 | +0.000% | 606,099,502 | 606,097,848 | -0.000% |
| `fib` | 262,981,693 | 262,984,058 | +0.001% | 266,745,414 | 266,745,743 | +0.000% |
| `closures` | 1,974,230,955 | 1,974,229,831 | -0.000% | 1,977,854,313 | 1,977,852,691 | -0.000% |
| `objects` | 713,144,237 | 713,143,808 | -0.000% | 716,905,699 | 716,904,589 | -0.000% |

## Tests

The numeric conversion test failed before the fix. The regression and js2 typecheck pass. The baseline and cost runs preceded #861 and #866. Windows, Miri and browser journeys were not run locally.
