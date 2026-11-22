# 2026-11-22 — Build js2 for-in snapshots from internal keys

The ordinary snapshot reads shape entries and indexed elements directly. It uses a seen set only when a prototype has enumerable string keys. The set records non-enumerable keys too, including the virtual array length, to preserve shadowing. The snapshot stores indices as numbers and formats each index when it yields the key. The iterator still checks each candidate against the current descriptors. The shape cursor and the host-aware snapshot remain available.

## Guard counts

A separate release build counted each opening on the live Rust Book search at `9a187630`. The journey passed with 21 replies. Each row records the first receiver condition and the prototype chain result. An indexed receiver fails the shape guard before it checks the prototype chain.

| Receiver | Prototype chain | Shape guard | Opens |
|---|---|---|---:|
| Dense array | Empty indexed prototype, no enumerable string keys | Miss | 19,107 |
| Dense array | Exotic object or pending enumeration work | Miss | 1 |
| Dictionary shape | No enumerable string keys | Miss | 27 |
| Shape with an index name | No enumerable string keys | Miss | 16 |
| Other tree shape | Exotic object or pending enumeration work | Miss | 2 |
| Other tree shape | No enumerable string keys | Hit | 150 |

The probe found no sparse arrays, holes, arguments objects, or prototypes with enumerable string keys. Total openings were 19,303, with 19,153 snapshot calls.

## Journey cost

The base is `9a187630`. Both release builds use symbols and frame pointers. All runs use archMachine, a Ryzen 9 5900HX, Rust 1.96.0, Linux 7.2.15-zen1-1-zen, and perf 7.3.1. Run `perf stat -e instructions:u` with `BARE_SEED=1`, `LC_ALL=C`, and `GLIBC_TUNABLES=glibc.malloc.tcache_count=65535`. Rotate build and engine order. Each cell gives the minimum and range of three passing runs in millions of instructions. All runs in a row have matching reply counts and bytes.

| Journey and engine | Main min [range] | Branch min [range] | Replies |
|---|---:|---:|---:|
| Rust Book search live, js2 | 3,107.391 [3,107.391–3,120.054] | 1,865.578 [1,865.578–1,874.430] | 21 |
| Rust Book search live, OLD | 2,369.967 [2,369.967–2,380.728] | 2,369.744 [2,369.744–2,379.567] | 21 |
| DEV home replay, js2 | 3,363.261 [3,363.261–3,364.149] | 3,362.960 [3,362.960–3,363.452] | 16 |
| Python.org search replay, js2 | 1,219.829 [1,219.829–1,219.933] | 1,218.095 [1,218.095–1,218.195] | 17 |
| Rust-lang.org home live, js2 | 89.489 [89.489–90.323] | 79.117 [79.117–88.862] | 14 |
| React `#a1` typing, 50 keys, js2 | 5,680.599 [5,680.599–5,684.446] | 5,679.344 [5,679.344–5,682.492] | — |

Rust Book js2 uses 39.96% fewer instructions and 0.79× the branch OLD count. DEV and Python use the #927 tapes and virtual clock. Both React builds produce 50 characters and the expected render counts. All branch React trials are below #930's 5,711.663 M minimum.

Callgrind 3.25.1 attributes 1,263.09 M inclusive instructions to snapshot setup on main and 25.17 M on the branch. Both profiles pass with 21 replies, 870,514 bytes, and 19,153 snapshot calls. `for_in_next` uses 22.51 M before and 23.02 M after over 58,066 calls. The profiles use the live Rust Book steps with a 120-second timeout. The OLD snapshot path used 13.91 M in [the #927 profile](../design-js2-real-pages.md#enumerate-ordinary-objects-without-walking-clean-prototypes).

Peak RSS uses the `wait4` wrapper and three live samples per allocator setting, build, and engine. Values are median [min–max] in KiB. The js2 change with default glibc is within the base run spread.

| Rust Book | Main | Branch |
|---|---:|---:|
| js2, large glibc thread cache | 99,336 [99,068–99,488] | 86,640 [86,312–87,176] |
| OLD, large glibc thread cache | 106,364 [106,068–106,792] | 106,480 [106,232–106,788] |
| js2, default glibc | 86,304 [85,748–86,452] | 86,444 [86,312–86,584] |
| OLD, default glibc | 105,804 [105,628–106,192] | 106,496 [105,852–106,524] |

## Cost rows

Use default release builds. The five `interp_load` rows use the minimum of three trials per allocator and build. The two `wrapper_load forin` rows use `cost_matrix.sh -aJ -g 'wrapper_load forin'`. The wrapper example runs the old engine.

| Row | glibc change | jemalloc change |
|---|---:|---:|
| `interp_load loopfn 3000000` | +0.0001% | −0.0001% |
| `interp_load calls 1000000` | −0.0001% | +0.0002% |
| `interp_load fib 27` | +0.0001% | +0.0002% |
| `interp_load closures 1000000` | <0.0001% | <0.0001% |
| `interp_load objects 1000000` | <0.0001% | <0.0001% |
| `wrapper_load forin 1000 3000` | −0.001% | +0.003% |
| `wrapper_load forin 10000 30000` | <0.001% | <0.001% |

All seven rows pass the +0.5% limit under both allocators.

## Correctness

The new internal-index test fails on main after it materializes the prototype tables. All 10 focused tests pass on the branch. They cover property order, shadowing, mutation after opening, dictionary shapes, indexed elements and descriptors, Proxy, typed arrays, arguments, strings, and host-backed prototypes. `cargo check --locked -p bare-js2 -p bare` passes.

The js2 test262 runner uses the repository skip file and test262 at `e4fbf577`. The passing files match on both builds.

| test262 directory | Main pass / fail / skip | Branch pass / fail / skip |
|---|---:|---:|
| `language/statements/for-in` | 112 / 6 / 1 | 112 / 6 / 1 |
| `built-ins/Object/keys` | 55 / 0 / 4 | 55 / 0 / 4 |

Raw measurements are in `/data/bare-tmp/r22/js2-forin-snapshot/` on archMachine. Local checks omit Windows, Miri, the full workspace suite, the rest of test262, and windowed input.
