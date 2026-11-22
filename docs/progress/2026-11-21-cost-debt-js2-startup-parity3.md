# Defer parity stack startup work in js2

#884 made `Array.prototype` an indexed object with zero inline property slots. Its constructor link and lazy table marker used out-of-line storage. The prototype keeps its indexed layout and has two inline slots. The global `Array` and `Array.prototype.constructor` make and share the same intrinsic on first use. The intrinsic makes its species getter and registers the static table at that point.

#888 added a shape transition for the own `arguments.length` property during realm setup. Use the first-use allocation from #896. Pin the shape when the first arguments object needs it.

#892 made `Function.prototype` callable and eagerly defined its `length` and `name`. Keep the prototype callable. Use its existing lazy table for those two data properties so reads through the prototype chain also see them.

The table entries follow the lazy built-in design. The arguments shape follows the #896 startup fix. [Lazy built-in design](../design-js2-lazy-builtins.md) · [#896 startup fix](https://github.com/mrwick1/bare/pull/896)

## Instruction counts

Use Rust 1.96.0 release builds on archMachine. Run `script_run` with `var x = 1;`. Each result is the minimum of three `perf stat -e instructions:u` runs. The spread is the maximum minus the minimum. Set `GLIBC_TUNABLES=glibc.malloc.tcache_count=65535` and preload `libjemalloc.so.2` for the jemalloc runs. The release profile sets one codegen unit for js2. These runs use the default settings for the other crates.

| Allocator |      #882 |      #901 |     Fixed | #901 vs #882 | Fixed vs #882 | Spread (#882 / #901 / fixed) |
| --------- | --------: | --------: | --------: | -----------: | ------------: | ---------------------------: |
| glibc     |   726,682 |   734,702 |   726,004 |      +1.104% |       -0.093% |            1,254 / 885 / 537 |
| jemalloc  | 4,429,832 | 4,476,126 | 4,430,301 |      +1.045% |       +0.011% |            1,149 / 886 / 662 |

The first candidate deferred the arguments shape and function metadata. It used 731,505 glibc instructions and 4,433,721 jemalloc instructions. Deferring the Array constructor and keeping the prototype slots inline brought glibc within the +0.5% limit.

#889 was open during measurement. Merge its head `be974c75` into main `cf3d1bed` locally (`0696ee44`). Compare with that tree plus the parity stack and the fix (`879579f7`).

| Allocator | Main with #889 | Main with #889, parity and fix |  Change | Spread (base / fixed) |
| --------- | -------------: | -----------------------------: | ------: | --------------------: |
| glibc     |        730,118 |                        726,728 | -0.464% |           286 / 1,180 |
| jemalloc  |      4,432,737 |                      4,430,438 | -0.052% |           945 / 1,089 |

## Stack bisection

Split the stack at #888 and #897. Measure the adjacent heads to locate the larger steps. Each pair uses three runs per head and allocator.

| Stack step                         | glibc before | glibc after | jemalloc before | jemalloc after |
| ---------------------------------- | -----------: | ----------: | --------------: | -------------: |
| #883 `7deb1ed3` to #884 `24eb8ec9` |      728,047 |     731,865 |       4,429,472 |      4,437,380 |
| #887 `6f908410` to #888 `a4add820` |      729,517 |     731,557 |       4,434,913 |      4,470,869 |
| #891 `8a81dd56` to #892 `73b7970f` |      731,306 |     730,847 |       4,470,702 |      4,472,614 |
| #895 `6d7fe938` to #897 `e46c9247` |      733,935 |     735,662 |       4,474,402 |      4,476,823 |

One glibc Callgrind run per head puts the inclusive cost of `Realm::new` at 235,921 instructions on #883 and 236,960 on #884. The largest jemalloc step is the arguments shape in #888. #892 adds two eager property definitions. Its net glibc change varies between this run and the parity stack report. #883 adds the string tags on Math and JSON.

#897 raises the process count. Its change runs in the generator path. The inclusive cost of `Realm::new` changes by 64 instructions in a glibc Callgrind comparison. Hypothesis: the remaining difference comes from code layout or allocation effects.

## Interpreter rows

Compare the #901 head with the fix. Use `interp_load loopfn 3000000`, `interp_load fib 27` and 1,000,000 iterations for each other row. The largest spread is 2,252 instructions.

| Row        |    glibc #901 |   glibc fixed |  Change | jemalloc #901 | jemalloc fixed |  Change |
| ---------- | ------------: | ------------: | ------: | ------------: | -------------: | ------: |
| `loopfn`   |   779,303,251 |   779,295,611 | -0.001% |   783,104,916 |    783,059,569 | -0.006% |
| `calls`    |   536,353,229 |   536,347,569 | -0.001% |   540,123,072 |    540,078,883 | -0.008% |
| `fib`      |   240,089,589 |   240,084,807 | -0.002% |   243,891,914 |    243,847,048 | -0.018% |
| `closures` | 1,869,667,692 | 1,869,665,675 | -0.000% | 1,873,345,542 |  1,873,281,139 | -0.003% |
| `objects`  |   705,195,122 |   705,186,064 | -0.001% |   708,994,482 |    708,948,514 | -0.006% |

## Checks

41 focused Rust tests pass on Linux. They cover lazy properties, arguments and Array behavior. The new startup tests fail before the corresponding changes and pass after them. The Array script tests pass with forced collection. The local merge with #889 passes 23 lazy property tests. The js2 typecheck passes.

Windows, Miri, test262, browser journeys, the full Rust suite and a forced single-unit build of all crates were not run locally.
