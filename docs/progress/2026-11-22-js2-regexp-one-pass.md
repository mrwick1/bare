# 2026-11-22 — Reuse the validated regexp parse in js2

## Step 1 profile

PR #927 measured 80.55 M instructions in all 329 calls to `regexp::check` on live Python.org. The 80 M cache estimate used that whole cost as an upper bound. It did not measure the checks that a cache would skip.

On the Python.org tape, re-read the two matched Callgrind runs from [step 1](2026-11-22-js2-regexp-setup.md). Both runs passed with 17 replies and 332,686 bytes. The full parser under creation took 79,960,141 instructions in 329 calls before the cache. After the cache it took 79,822,135 instructions in 255 calls. The 74 skipped calls saved 138,006 parser instructions, about 1,865 each. `Parser::add_class` still ran 717 times and took 121.33 M self instructions. The native instruction minimum changed by 0.047 M, within the sample spread.

The step 1 tape spent 46.31 M instructions in 118 program compiles. The second full parse accounted for 45.18 M. The cache shares work within one realm. The expensive repeated source and flag pairs on this tape largely occur across distinct realms.

## Implementation

Run the full parser once on a cache miss at wrapper creation. Emit from the validated tree and share the immutable image by source and flags. Each wrapper retains the image across collection and cache eviction. Each `lastIndex` starts at zero.

Keep the early literal check at script load and the full syntax and parser checks at creation. Save an emit limit in the wrapper and throw its `SyntaxError` at first use. If the heap refuses an image, retain the existing retry and allocation error at first use.

The test counter measures full runtime parses. The red run measured four for two new patterns and 34 for 33 new patterns after eviction. The green run measured two and 33. All 65 focused unit tests passed. They cover Unicode and Unicode sets, ignore case, dotAll, sticky matching, indices, named captures, error timing, independent wrapper state, eviction and collection. The `bare-js2` and `bare` typecheck passed.

## Journeys

ArchMachine, AMD Ryzen 9 5900HX, Rust 1.96.0, perf 7.3.1. The frozen base `7ef32f22` has the same source tree as main after #933 merged at `7b91fe3f`. Both release builds have debug symbols. Run js2 with `BARE_SEED=1`, `LC_ALL=C`, and `GLIBC_TUNABLES=glibc.malloc.tcache_count=65535`. Rotate the base and branch order. Replay Python.org and DEV from #927 and Rust Book from #929 with the virtual clock. Run Rust-lang.org live.

Instruction counts give the minimum and range in millions from three samples per build on each tape and four live samples. Every report passed with full counter coverage. Peak RSS gives the wait4 median and range in MiB from the same number of separate runs. The reply count and byte total match in every report.

| Journey | Replies / bytes | Main instructions | Branch instructions | Main peak RSS | Branch peak RSS |
|---|---:|---:|---:|---:|---:|
| Python.org tape | 17 / 332,686 | 1,372.067 [1,372.067–1,372.214] | 1,328.335 [1,328.335–1,328.447] | 38.148 [37.961–38.156] | 37.711 [37.465–38.168] |
| DEV tape | 16 / 214,521 | 3,324.734 [3,324.734–3,325.333] | 3,322.665 [3,322.665–3,322.739] | 101.059 [100.703–104.246] | 102.887 [102.422–104.723] |
| Rust-lang.org live | 14 / 42,114 | 96.819 [96.819–97.393] | 97.375 [97.375–97.445] | 22.797 [22.785–22.922] | 23.113 [22.754–23.355] |
| Rust Book tape | 21 / 870,650 | 17,553.881 [17,553.881–17,562.367] | 17,549.750 [17,549.750–17,556.730] | 125.973 [125.797–126.711] | 126.340 [126.113–126.406] |

The Python.org minimum fell by 43.73 M instructions and its RSS median fell by 0.44 MiB. The Rust-lang.org minimum rose by 0.56 M in a 0.57 M base spread. Its RSS median rose by 0.32 MiB. DEV's minimum fell by 2.07 M with eager emission. Retain eager emission for this change.

On the matched Python.org Callgrind pair, 373 full parses became 255. `Parser::add_class` fell from 717 to 502 calls and from 121.33 M to 77.39 M self instructions. The eager emitter took 2.52 M inclusive instructions across 255 calls. The native result includes the cost of emitting for patterns that the journey never matches.

On the matched DEV Callgrind pair, the base parsed 356 times and compiled 124 programs. The branch emitted 232 programs from the creation parse. The base compile scope took 6.05 M instructions, including a 3.98 M second parse. Eager emission took 4.55 M across all 232 programs. The whole matched Callgrind journey fell by 1.84 M.

## Compatibility

Run js2 via `crates/js-diff` on test262 `e4fbf57745c6aae3d1f12f181ecef78b1665d812` with the repository skip rules. The main and branch passing sets match.

| Directory | Pass | Fail | Skip | Lost passes |
|---|---:|---:|---:|---:|
| `built-ins/RegExp` | 1,125 | 95 | 659 | 0 |
| `language/literals/regexp` | 97 | 4 | 139 | 0 |

## Cost rows

All 16 regexp rows pass under glibc and jemalloc. These cover `builtins_load` scan, replace, split, routes, zero, and gc, plus `arrobj_load replaceall` and `web_apis_load matchall`, at scales 1 and 10. The largest increase was 0.002%.

The five `interp_load` rows pass under both allocators. Each count is the minimum of three runs.

| Row | glibc instructions | jemalloc instructions |
|---|---:|---:|
| `loopfn 3000000` | 800,475,032 → 800,474,874 | 803,875,782 → 803,876,140 |
| `calls 1000000` | 555,513,909 → 555,514,331 | 558,883,358 → 558,883,742 |
| `fib 27` | 241,521,885 → 241,521,774 | 244,921,013 → 244,920,654 |
| `closures 1000000` | 1,739,273,902 → 1,739,273,744 | 1,742,626,253 → 1,742,626,533 |
| `objects 1000000` | 699,862,984 → 699,863,449 | 703,233,430 → 703,232,983 |

Not tested locally: Windows, the full workspace suite, the full test262 suite, idle latency, or a separate implementation of deferred emission.
