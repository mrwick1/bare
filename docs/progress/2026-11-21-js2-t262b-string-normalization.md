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

## Cost against #882

The release profile uses one codegen unit. The minimum of three runs for each allocator shows one stack row above the limit. Hypothesis: the additional prototype shapes and metadata increase realm initialization work. Record the startup row in [cost debt](cost-debt.md).

| Row | glibc base | glibc new | Change | jemalloc base | jemalloc new | Change |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| `startup` | 726,975 | 734,860 | +1.085% | 4,429,351 | 4,475,217 | +1.036% |
| `loopfn` | 821,296,086 | 779,302,770 | -5.113% | 825,058,555 | 783,105,172 | -5.085% |
| `calls` | 552,346,228 | 536,353,724 | -2.895% | 556,076,353 | 540,123,163 | -2.869% |
| `fib` | 247,393,810 | 240,089,851 | -2.952% | 251,155,003 | 243,891,178 | -2.892% |
| `closures` | 1,896,665,038 | 1,869,667,875 | -1.423% | 1,900,282,315 | 1,873,345,003 | -1.418% |
| `objects` | 710,186,343 | 705,194,653 | -0.703% | 713,946,903 | 708,994,243 | -0.694% |
