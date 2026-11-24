# Scan borrowed JSON input and intern keys from spans

## Before

JSON.parse flattens the source, then copies all code units into a new UTF-16 vector. Each nonempty key grows a fresh UTF-16 vector and allocates one JS string. The property path classifies that string with `key_of`. An atom miss can promote the same string. An atom hit leaves the temporary JS string for collection. A canonical decimal key on a plain object also allocates a decimal Rust string and probes for its atom. On a miss it allocates a second JS string, the atom. The generic property definition path checks the receiver and descriptor rules for each key.

OLD interns from a slice or reusable scratch and caches short repeated keys. QuickJS calls `JS_ValueToAtom` on each key token. Its integer atoms can encode the index directly. js2 stores plain-object indices as name atoms.

## Change

Dispatch on the width of the flat source. Scan quoted text in place. Reuse one UTF-16 buffer to decode escaped tokens. Intern from a source span or the decoded units. Keep a fixed cache of recent decimal indices and their atoms. A key hit allocates no string. A miss allocates the atom only. Update duplicate values in their original slots. Append array values to the result as they arrive.

The scanner uses stack space proportional to nesting and scratch proportional to the largest escaped token. UTF-16 input still transcodes individual number tokens for the floating-point parser. A rope can need one flattening copy.

## Verification

The allocation regression failed on the base. The four language guards passed there. All five pass with borrowed spans. The guards cover UTF-16, escape aliases, duplicate order, index bounds, `__proto__`, reviver order and nesting.

`cargo check --locked -p bare-js2 -p bare` passes.

Test262 `built-ins/JSON/parse` has 63 passes, 0 failures and 14 configured skips on both trees. The per-file results match.

## Public journeys

Compare main `a17eab43` and the branch from that base. Use release `bare-drive` on archMachine (AMD Ryzen 9 5900HX, Rust 1.96.0, perf 7.3.1). Use `BARE_SEED=1`, `LC_ALL=C` and `GLIBC_TUNABLES=glibc.malloc.tcache_count=65535`. Rotate the build order. Run Rust Book and Rust-lang.org live. Replay the DEV and Python.org tapes with a virtual clock. A Rust Book recording matched the live reply count and bytes. Its virtual replay used about four times the live instructions on the first base, so use live results here.

Each row gives the minimum and range of three passing `instructions:u` counts, in millions. All counters ran for 100% of the measurement. The report gave the same reply count on both js2 builds.

| Journey | Main js2 | Branch js2 | Replies |
| --- | ---: | ---: | ---: |
| Rust Book search | 4,189.41 [4,189.41–4,192.01] | 3,825.10 [3,825.10–3,829.15] | 21 |
| DEV home | 3,326.50 [3,326.50–3,327.55] | 3,324.30 [3,324.30–3,324.55] | 16 |
| Python.org search | 1,372.24 [1,372.24–1,372.31] | 1,372.20 [1,372.20–1,372.35] | 17 |
| Rust-lang.org home | 96.81 [96.81–97.75] | 96.83 [96.83–98.43] | 14 |

The Rust Book minimum fell by 364.31 M instructions (8.7%). OLD on main took 2,325.66 M [2,325.66–2,337.69]. It also took 21 replies. One OLD run differed by three network bytes. None of the other journey minima rose by more than its run spread.

Rust Book peak RSS uses the median of three `wait4` runs, in KiB. Main js2 used 101,676 [101,628–101,736]. Branch js2 used 101,188 [101,188–101,268]. OLD used 107,128 [107,000–107,384].

Defer JSON B. The measured journey gain from JSON A exceeds the design's 200–340 M combined estimate. Profile the remaining shape and dictionary work before deciding if staging pays for its extra memory.

## Cost rows

Use default release builds. Measure three counts per build and allocator. Compare the minima. The four JSON rows already exist in `tools/js/cost_rows.txt`. Run them with `cost_matrix.sh -g`. Run the five `interp_load` rows separately.

| Row | glibc | jemalloc |
| --- | ---: | ---: |
| `builtins_load json 1` | -0.003% | 0.000% |
| `builtins_load json 10` | 0.000% | +0.001% |
| `wrapper_load json 1000 3000` | +0.002% | +0.001% |
| `wrapper_load json 10000 30000` | 0.000% | -0.002% |
| `interp_load loopfn 3000000` | 0.0000% | 0.0000% |
| `interp_load calls 1000000` | 0.0000% | +0.0001% |
| `interp_load fib 27` | +0.0001% | 0.0000% |
| `interp_load closures 1000000` | 0.0000% | 0.0000% |
| `interp_load objects 1000000` | 0.0000% | +0.0001% |

All nine rows pass the +0.5% gate under both allocators.

Run the js2 JSON micro with `script_run --eval`. Parse 2,000 nine-key records 20 times. Include repeated names, decimal keys, an escaped string and nested arrays. Verify the checksum 50,000. glibc takes 863.41 M [863.41–863.42] on main and 388.58 M [388.58–388.58] on the branch. jemalloc takes 912.48 M [912.48–912.49] on main and 392.00 M [392.00–392.00] on the branch.

## Not tested

Local verification did not cover Windows, Miri, the full workspace suite, or Test262 outside JSON.parse.
