# Respect array iterator overrides in bytecode iteration

Array iteration uses a direct indexed step when the array and iterator use the original methods. Otherwise the engine reads the iterator and calls its next method. A missing direct element uses ordinary property lookup. Early exit calls a return method added to the array iterator prototype.

## Test262

This change fixes 97 old-only cases. It fixes all 48 class destructuring cases. The full baseline run has 26,031 old passes, 26,482 new passes and 355 old-only cases.

## Cost

User-mode instruction counts from `perf stat` on archMachine. Each cell is the minimum of three release runs. Base is the computed property update change. Startup runs `script_run` on `var x = 1;`. The baseline and cost runs preceded the merge of #871.

| Row | glibc base | glibc new | Change | jemalloc base | jemalloc new | Change |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| `startup` | 731,991 | 732,244 | +0.035% | 4,436,026 | 4,435,542 | -0.011% |
| `loopfn` | 863,309,775 | 887,310,247 | +2.780% | 867,073,260 | 891,072,861 | +2.768% |
| `calls` | 595,368,413 | 602,366,700 | +1.175% | 599,099,396 | 606,099,316 | +1.168% |
| `fib` | 262,030,582 | 262,984,152 | +0.364% | 265,792,996 | 266,746,473 | +0.359% |
| `closures` | 1,973,648,112 | 1,974,218,242 | +0.029% | 1,977,265,312 | 1,977,835,716 | +0.029% |
| `objects` | 709,210,998 | 713,145,930 | +0.555% | 712,971,291 | 716,905,406 | +0.552% |

At one codegen unit, `loopfn` is +1.483% on glibc and +1.476% on jemalloc. The `calls` row is +1.098% and +1.091%. Both remain above the rule. The `objects` row is -0.976% and -0.971%. Hypothesis: the added iterator checks change register allocation in the shared interpreter dispatch even on programs that do no iteration.

## Tests

The iterator override regression failed before the fix. It passes with and without forced collection. The generator compiler tests, iterator bytecode tests, the iterator-close test and the js2 typecheck pass. Windows, Miri and browser journeys were not run locally.
