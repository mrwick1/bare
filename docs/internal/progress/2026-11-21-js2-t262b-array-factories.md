# 2026-11-21: js2 Array factories

Use the receiver when it is a constructor. Define each result element as an own data property. Read the length once for array-like inputs. An iterable uses its captured iterator methods and is closed on a mapper or definition error.

The pinned test262 subset has 26,682 js2 passes and 158 old-only cases. This closes eight old-only cases. No js2 test pass was lost after retrying a generated RegExp timeout.

## Cost

archMachine, release profile, minimum of three instruction-count runs with perf. Base: `f273d199`. Startup runs `var x = 1;`. The input is 3,000,000 for `loopfn`, 1,000,000 for `calls`, `closures` and `objects`, and 27 for `fib`. The largest spread is 0.155%.

| Row | glibc base | glibc new | Change | jemalloc base | jemalloc new | Change |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| `startup` | 732,730 | 730,472 | -0.308% | 4,438,735 | 4,437,174 | -0.035% |
| `loopfn` | 821,298,707 | 821,299,130 | +0.000% | 825,065,783 | 825,065,945 | +0.000% |
| `calls` | 552,349,493 | 552,348,841 | -0.000% | 556,084,909 | 556,084,560 | -0.000% |
| `fib` | 247,395,368 | 247,395,757 | +0.000% | 251,163,141 | 251,163,011 | -0.000% |
| `closures` | 1,896,667,924 | 1,896,669,837 | +0.000% | 1,900,285,481 | 1,900,285,276 | -0.000% |
| `objects` | 710,191,132 | 710,191,739 | +0.000% | 713,989,985 | 713,989,082 | -0.000% |

Not tested: Windows, Miri or browser journeys.
