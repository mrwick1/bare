# 2026-11-21: Expose js2 string and accessor descriptors

Box primitive strings before collecting their own descriptors. Give the lazy prototype accessors the standard getter and setter names.

The pinned test262 subset has 26735 js2 passes and 117 old-only cases. This closes 4 old-only cases. No js2 test pass was lost. The four focused descriptor cases pass.

## Cost

archMachine, release profile, minimum of three instruction-count runs with perf. Base: `e92607af`. Startup runs `var x = 1;`. The input is 3,000,000 for `loopfn`, 1,000,000 for `calls`, `closures` and `objects`, and 27 for `fib`. The largest spread is 0.083%.

| Row | glibc base | glibc new | Change | jemalloc base | jemalloc new | Change |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| `startup` | 734,389 | 734,296 | -0.013% | 4,475,812 | 4,476,498 | +0.015% |
| `loopfn` | 779,302,695 | 779,303,138 | +0.000% | 783,104,445 | 783,104,570 | +0.000% |
| `calls` | 536,353,293 | 536,352,679 | -0.000% | 540,122,809 | 540,123,279 | +0.000% |
| `fib` | 240,089,779 | 240,089,536 | -0.000% | 243,892,069 | 243,891,664 | -0.000% |
| `closures` | 1,869,667,981 | 1,869,668,189 | +0.000% | 1,873,345,152 | 1,873,343,062 | -0.000% |
| `objects` | 705,194,534 | 705,194,497 | -0.000% | 708,994,037 | 708,993,829 | -0.000% |

Not tested: Windows, Miri or browser journeys.
