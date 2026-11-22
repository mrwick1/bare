# 2026-11-22 — Read js2 for-in keys from the opening shape

A for-in record can hold a tree shape, a cursor, and the source object. The shape supplies the opening key order. A hit builds no key list, array, or seen set. The shape stores whether it has any index names. The opening check certifies that the prototype chain has no enumerable properties and no pending host or built-in enumeration work. A lazy host prototype stays on the snapshot path until the host has handled its `Lazy::All` request for the current collection cycle.

Each step compares the source shape with the opening shape. After a change, the loop checks the current descriptors for the remaining opening keys. Added names stay out of the loop. Deleted names are skipped if no enumerable property with that name remains in the chain. Proxies, indexed or dictionary receivers, and host-backed receivers use the original snapshot. The GC traces the opening shape from live registers and saved generator or async registers. The shape keeps its atoms alive until the record is dead.

Eligible openings can collect at the existing idle threshold during adaptive tasks. This limits the growth that can accumulate when the loop saves the snapshot allocations. The check respects fixed task limits and holds on collection.

## Eligibility and collection

A scratch build at `f4ff9eed` kept the original snapshots and tagged live iterator records for the counters. It sampled React once at each input size. The shipping build omits the counters.

| Keys typed | Openings | Eligible | Shape guard failures | Collections with an open iterator |
| ---------: | -------: | -------: | -------------------: | --------------------------------: |
|          0 |    1,481 |    1,479 |                    0 |                                 0 |
|         50 |   72,406 |   72,358 |                    0 |                                11 |

All 11 collections found an eligible iterator. The original [React investigation](2026-11-22-react-typing-check.md) names the fixture and explains the OLD baseline.

## React typing

Compare frozen main `e97f1331` with this change on archMachine (Ryzen 9 5900HX, Linux 7.2.15-zen1-1-zen, Rust 1.96.0, glibc 2.44, perf 7.3.1). Build `bare --example type_load` with the profiling profile and `RUSTFLAGS='-C force-frame-pointers=yes'`. Run the frozen React typing fixture at `#a1` with zero or 50 keys. Use `BARE_SEED=1`, `LC_ALL=C`, and `GLIBC_TUNABLES=glibc.malloc.tcache_count=65535` for `perf stat -e instructions:u`. Each instruction row has three paired runs. It gives the minimum [min–max] in millions. All runs produce the expected value length and React render counts.

| Keys | OLD `auto` | js2 before | js2 after | js2 change | js2 / OLD before → after |
|---:|---:|---:|---:|---:|---:|
| 0 | 432.446 [432.446–432.461] | 470.708 [470.708–472.451] | 430.706 [430.706–430.942] | -8.50% | 1.089× → 0.996× |
| 50 | 6,375.475 [6,375.475–6,375.569] | 7,666.169 [7,666.169–7,676.256] | 5,711.663 [5,711.663–5,714.843] | -25.50% | 1.202× → 0.896× |

Measure peak RSS with the static `wait4` wrapper, using six alternating runs of each build for each allocator setting. The table gives median [min–max] in KiB.

| Keys and allocator | js2 before | js2 after |
|---|---:|---:|
| 0, default glibc | 23,136 [22,980–23,308] | 23,104 [23,048–23,500] |
| 50, default glibc | 29,128 [29,040–29,444] | 28,970 [28,660–29,308] |
| 0, large glibc thread cache | 24,486 [24,140–24,568] | 24,146 [24,004–24,304] |
| 50, large glibc thread cache | 46,166 [45,708–46,336] | 31,280 [30,996–31,488] |

The first frame-only marker build raised 50-key median RSS from 29,040 to 29,776 KiB with default glibc. In one debugger run per build, the highest mapped heap at collection rose from 8,650,752 to 8,978,432 bytes. Both readings came from the fourth collection. A trial with the idle-threshold check moved that collection forward and reduced its mapped heap to 8,585,216 bytes. With the final check, both the median and the highest measured 50-key RSS fall.

## Cost rows

Use the same frozen base and a default release build. For `interp_load`, use the minimum of three `perf stat -e instructions:u` runs per allocator. All spreads are below 0.001%. Run the four published js2 rows with `tools/js/cost_matrix.sh -j3 -aJ -g '^examples/js2_'`. Both allocators pass the +0.5% limit on all nine rows.

| Row                            |    glibc | jemalloc |
| ------------------------------ | -------: | -------: |
| `interp_load loopfn 3000000` | +0.0008% | +0.0009% |
| `interp_load calls 1000000` | +0.0012% | +0.0012% |
| `interp_load fib 27` | +0.0027% | +0.0027% |
| `interp_load closures 1000000` | -0.0912% | -0.0911% |
| `interp_load objects 1000000` | +0.0019% | +0.0018% |
| `js2_gen_load next 100000` | +0.301% | +0.290% |
| `js2_lex_load small 1 1` | +0.000% | -0.000% |
| `js2_parse_load full stmts 1` | -0.012% | -0.041% |
| `js2_promise_load then 100000` | +0.022% | +0.020% |

The first implementation checked the tag on every value traced by the GC. On `f4ff9eed`, the closure row rose 1.417% with glibc and 1.414% with jemalloc. The saved-register flag limits this work to frames. The final build leaves ordinary heap value tracing on the existing pointer path.

## Correctness

The plain-receiver test failed on the baseline because the opener returned a key array. Six focused tests pass with the shape cursor. They cover key order, shadowing, mutation, nested loops, collection after a shape change, Proxy and host enumeration, and lazy prototype requests across GC. The 46 existing interpreter, shape, GC, and heap tests in the targeted filters pass. `cargo check --locked -p bare-js2 -p bare` passes.

Use the `bare-js-diff` test262 runner at test262 `e4fbf577`. Use the repository skip file and permit the `Proxy` feature for the three Proxy directories. Both builds have 2,713 passes, 47 failures, and 25 skips. The passing filenames match.

| test262 directory                          | Pass / fail / skip on each build |
| ------------------------------------------ | -------------------------------: |
| `language/statements/for-in`               |                      112 / 6 / 1 |
| `language/statements/for-of`               |                    707 / 32 / 12 |
| `language/statements/for`                  |                      380 / 0 / 5 |
| `built-ins/Object/create`                  |                      320 / 0 / 0 |
| `built-ins/Object/defineProperty`          |                    1,122 / 9 / 0 |
| `built-ins/Object/setPrototypeOf`          |                       11 / 0 / 1 |
| `built-ins/Proxy/ownKeys`                  |                       25 / 0 / 2 |
| `built-ins/Proxy/getOwnPropertyDescriptor` |                       18 / 0 / 3 |
| `built-ins/Proxy/getPrototypeOf`           |                       18 / 0 / 1 |

Raw logs and scripts are in `/data/bare-tmp/r22/js2-for-in-shape/` on archMachine. Local checks omit Windows, Miri, the full workspace suite, the rest of test262, and windowed input.
