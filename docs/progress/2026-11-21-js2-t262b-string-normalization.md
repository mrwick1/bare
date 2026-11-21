# 2026-11-21: Add js2 compatibility normalization

Support NFKC and NFKD with the bundled Unicode data. Preserve lone surrogates. Stack startup against #882 is +1.085% with glibc and +1.036% with jemalloc.

The pinned test262 subset has 26748 js2 passes and 104 old-only cases. This closes 1 old-only cases. No js2 test pass was lost. The canonical Unicode data test passes. Both engines ran on the final build and the old pass list matched. The release profile already uses one codegen unit.

## Cost

archMachine, release profile, minimum of three instruction-count runs with perf. Base: `cc37e932`. Startup runs `var x = 1;`. The input is 3,000,000 for `loopfn`, 1,000,000 for `calls`, `closures` and `objects`, and 27 for `fib`. The largest spread is 0.208%.

| Row | glibc base | glibc new | Change | jemalloc base | jemalloc new | Change |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| `startup` | 734,600 | 734,619 | +0.003% | 4,476,141 | 4,476,370 | +0.005% |
| `loopfn` | 779,303,045 | 779,302,799 | -0.000% | 783,105,967 | 783,106,285 | +0.000% |
| `calls` | 536,353,633 | 536,353,560 | -0.000% | 540,123,230 | 540,123,771 | +0.000% |
| `fib` | 240,089,734 | 240,089,927 | +0.000% | 243,892,383 | 243,892,298 | -0.000% |
| `closures` | 1,869,668,274 | 1,869,668,224 | -0.000% | 1,873,344,928 | 1,873,345,714 | +0.000% |
| `objects` | 705,194,979 | 705,195,407 | +0.000% | 708,994,997 | 708,995,245 | +0.000% |

Not tested: Windows, Miri or browser journeys.
