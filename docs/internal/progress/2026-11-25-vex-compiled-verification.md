# Verify Vex compiler output on debug and conformance paths

Debug builds check code before installing a script, a cache hit, a prelude stub, or a lazy replacement. The Test262 runner enables the `verify-compiled` feature in release. A fuzz build can use the same feature. The public VM constructor still checks caller-supplied bytecode in all builds.

## Instruction counts

archMachine, AMD Ryzen 9 5900HX. Release builds with the lockfile. Each value is the minimum of three runs of `perf stat -e instructions:u`. Runs alternate baseline and changed binaries. The glibc runs use `glibc.malloc.tcache_count=65535`. The second set preloads jemalloc.

The public constructor comparison uses `73f3107c` and the merged PR `02153294`. Each row checks the result. The largest increase from minimum to maximum within one group was 0.005%.

| interp_load case | Input | glibc change | jemalloc change |
| --- | ---: | ---: | ---: |
| loopfn | 100000 | +0.053% | +0.056% |
| calls | 100000 | +0.045% | +0.045% |
| fib | 22 | +0.118% | +0.113% |
| closures | 100000 | +0.028% | +0.047% |
| objects | 100000 | +0.032% | +0.030% |

The `script_run --eval` comparison uses the release binary at `02153294` and the feature-enabled release binary built from these changes. The inputs are the `eval_` fixtures. Each run checks its result against Node. The largest increase from minimum to maximum within one group was 0.116%.

| Fixture | glibc before | glibc after | jemalloc before | jemalloc after |
| --- | ---: | ---: | ---: | ---: |
| calls | 63,286,969 | 63,327,615 | 66,629,194 | 66,671,596 |
| closures | 12,665,742 | 12,746,450 | 16,041,861 | 16,123,629 |
| loop | 73,822,992 | 73,856,931 | 77,169,142 | 77,202,370 |
| objects | 59,472,435 | 59,661,867 | 62,942,937 | 63,154,088 |
| webpack | 5,893,673 | 7,355,812 | 9,375,786 | 10,898,357 |

Full compiler verification increases the webpack fixture by 24.81% under glibc and 16.24% under jemalloc. These figures cover the opt-in build. Keep full compiler verification out of the default release build.

## Conformance

The pinned subset used Promise.resolve, class statements, for-of statements, and optional chaining. The two runners produced identical per-test output. Both passed 465, failed 21, and skipped 19. The debug script, cache, module, and verifier root tests passed.

Coverage excludes the full Test262 suite, fuzz execution, recorded browser journeys, wall time, and Windows.
