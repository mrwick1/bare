# Guard direct array push in js2

A plain array with one argument can append directly to its dense elements record. The guard checks the receiver and the stock prototype chain. The realm caches the named-index proof at the prototype epoch. Each hit still checks the indexed state of `Array.prototype`. Other cases use the generic push path.

## Measurements

archMachine, Ryzen 9 5900HX, Linux 7.2.15-zen1-1-zen, Rust 1.96.0. Both builds use the release profile and glibc. The comparison uses main at `9c8e9f28` and the guarded append on that base. Each instruction range is three fresh `perf stat -e instructions:u` runs. RSS is the median and range of three fresh binary copies measured by a static `wait4` parent. See the [decision benchmark](2026-11-21-engine-decision-benchmark.md) for the frozen inputs and checksums.

| Input | js2 before, M instructions | js2 after, M instructions | QuickJS direct, M instructions | js2 RSS before → after, MiB (range) |
|---|---:|---:|---:|---:|
| `objects_1` | 97.384–97.389 | 68.030–68.034 | 53.475 | 7.223 (7.219–7.234) → 7.168 (7.152–7.277) |
| `objects_10` | 937.125–937.160 | 638.305–638.342 | 512.457 | 16.949 (16.906–16.977) → 16.938 (16.852–17.035) |

Callgrind measures the push phase as the push prefix minus the enumeration prefix. It fell from 461.256 M to 164.036 M instructions. Both scales produced the expected completion values. The unit test checks that stock appends leave the sampled index absent from the runtime atom table.

| Cost row | glibc change | jemalloc change |
|---|---:|---:|
| `interp_load loopfn 3000000` | +0.0000% | +0.0001% |
| `interp_load calls 1000000` | -0.0000% | +0.0052% |
| `interp_load fib 27` | +0.0001% | -0.0003% |
| `interp_load closures 1000000` | -0.0001% | -0.0007% |
| `interp_load objects 1000000` | -0.0000% | +0.0000% |
| `js2_gen_load next 100000` | -0.000% | +0.000% |
| `js2_lex_load small 1 1` | +0.000% | -0.000% |
| `js2_parse_load full stmts 1` | -0.004% | +0.002% |
| `js2_promise_load then 100000` | +0.000% | -0.000% |

The cost table compares each minimum of three. All rows pass the +0.5% gate under both allocators.

## Tests

The stock-prototype watch test failed before the change and passed with the guard. The fallback tests cover multiple arguments, holes, sparse storage, length overflow, frozen arrays, read-only length, array-likes, arguments objects, host index writes, and prototype changes after warmup.

The four push tests, 25 object tests, and 18 shape tests passed. `cargo check --locked -p bare-js2 -p bare` passed.

| test262 directory | Pass / fail / skip before and after | Lost passes |
|---|---:|---:|
| `built-ins/Array/prototype/push` | 24 / 0 / 0 | 0 |
| `built-ins/Array/length` | 29 / 0 / 1 | 0 |
| `built-ins/Object/defineProperty` | 1,122 / 6 / 3 | 0 |
| `built-ins/Object/freeze` | 48 / 1 / 4 | 0 |
| `built-ins/Object/preventExtensions` | 38 / 1 / 1 | 0 |
| `built-ins/Object/setPrototypeOf` | 11 / 0 / 1 | 0 |

Not tested locally: full test262, browser journeys, Windows, and Miri.
