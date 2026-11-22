# 2026-11-22 — Reuse validated regexp setup in js2

Before the change, each literal occurrence gets an early syntax check at script parse time. Evaluating it N times runs the full runtime pattern parser N times. Matching every wrapper adds one full parse while the literal's source cell and flags stay in the 32-entry program cache. Matching no wrappers adds zero parses.

Calling `new RegExp(s)` N times runs the full runtime parser N times. Matching every wrapper adds one full parse while the same source cell stays in the program cache. If every call supplies a distinct string cell with equal content, matching every wrapper adds N full parses. Evicting an entry can cause another parse on the next wrapper's first match.

The realm now keeps up to 32 verified pairs of source content and flag bits. A pair can hold a compiled program. The realm marks both the source and program during collection. A miss runs the full syntax check. Each creation makes a fresh object and sets its own `lastIndex` to zero. The first match can reuse a resident program. The emit pass and its limits still apply at first use.

QuickJS emits bytecode in `lre_compile` and places it in the script's constants at literal compile time. Its `OP_regexp` allocates a wrapper around that bytecode. This step leaves the deferred emit pass in js2.

## Unit checks

The red run parsed 21 times for 20 evaluations and matches of one literal. It parsed four times for two equal sources with distinct cells across a collection. The green run parsed twice in each case. The literal test saw 39 pointer hits. The distinct-cell test saw two pointer hits and one content hit. All 62 regexp-related unit tests passed. Tests also cover different flags, early and runtime syntax errors, lazy emit errors, uncompiled entries after GC, and cache eviction.

## Journeys

ArchMachine, AMD Ryzen 9 5900HX, Rust 1.96.0, and perf 7.3.1. Compare frozen main `5540846b` with implementation `152722c7`. Both release builds have debug symbols. Use `BARE_SEED=1`, `LC_ALL=C`, and `GLIBC_TUNABLES=glibc.malloc.tcache_count=65535`. Rotate the order between builds and modes. Use the Python.org and DEV tapes from #927 and the Rust Book tape from #929. Run Rust-lang.org live.

The instruction results give the minimum and range in millions for three runs per cell. Rust-lang.org uses four matched runs per js2 cell. The OLD variant has four matched main runs and three branch runs. One OLD run had 17 replies and was excluded. The peak RSS results give the median and range in MiB across wait4 runs with the same trial counts, except that OLD Rust-lang.org has four matched RSS runs per build.

| Journey | Mode | Main instructions | Branch instructions | Main peak RSS | Branch peak RSS |
|---|---|---:|---:|---:|---:|
| Python.org tape | js2 | 1,372.198 [1,372.198–1,372.289] | 1,372.150 [1,372.150–1,372.413] | 38.875 [37.891–38.875] | 38.449 [38.441–38.918] |
| Rust-lang.org live | js2 | 96.884 [96.884–98.769] | 96.612 [96.612–96.766] | 23.639 [23.328–23.813] | 22.943 [22.570–23.297] |
| Rust Book tape | js2 | 17,559.465 [17,559.465–17,565.583] | 17,550.204 [17,550.204–17,554.592] | 126.008 [125.996–126.391] | 126.293 [126.012–126.469] |
| DEV tape | js2 | 3,327.083 [3,327.083–3,328.304] | 3,325.473 [3,325.473–3,326.615] | 106.500 [104.730–108.438] | 102.348 [101.156–106.734] |
| Python.org tape | OLD | 1,163.568 [1,163.568–1,163.774] | 1,163.464 [1,163.464–1,163.695] | 38.855 [38.836–39.086] | 38.734 [38.336–38.938] |
| Rust-lang.org live | OLD | 85.261 [85.261–85.836] | 85.643 [85.643–85.784] | 22.951 [22.840–23.211] | 22.512 [22.211–22.727] |
| Rust Book tape | OLD | 7,363.502 [7,363.502–7,363.695] | 7,363.472 [7,363.472–7,363.677] | 118.027 [118.008–118.340] | 118.453 [118.441–118.660] |
| DEV tape | OLD | 3,197.377 [3,197.377–3,198.063] | 3,197.204 [3,197.204–3,197.951] | 100.973 [99.078–103.266] | 101.227 [100.191–104.844] |

Every included instruction report passed with full counter coverage. The four journeys had 17, 14, 21, and 16 replies respectively, with the same byte counts in both builds and modes. The Python.org minimum moved by 0.047 M instructions, within run spread. The live Rust-lang.org minimum fell by 0.272 M. Peak RSS medians fell on both target journeys.

## Python.org profile

One matched Callgrind run on each build had 17 replies and 332,686 bytes. Main ran 329 creation checks and 118 program compiles. The branch ran 255 creation checks and 118 program compiles. The full parser fell from 125.13 M to 125.00 M instructions. Compilation still used 46.31 M instructions, against 46.30 M on main. Both runs called `Parser::add_class` 717 times.

A debugger trace on the branch counted 74 pointer hits on creation and 117 on program lookup. It saw zero content hits on this tape. Source and flag keys largely repeat across two separate realms. Each realm builds its own expensive classes. A trial with 128 entries and a 1 MiB payload budget left the Python.org minimum within the base spread. Retain the 32-entry bound.

## Cost rows

All 16 regexp rows in `cost_rows.txt` passed under glibc and jemalloc. These are `builtins_load` scan, replace, split, routes, zero, and gc, plus `arrobj_load replaceall` and `web_apis_load matchall`, at scales 1 and 10. The mean changes were -0.000% and +0.001%. The largest increase was 0.017%.

| Row | glibc minimum | jemalloc minimum |
|---|---:|---:|
| `interp_load loopfn 3000000` | 800,474,698 → 800,474,818 | 803,875,882 → 803,876,011 |
| `interp_load calls 1000000` | 555,513,829 → 555,514,708 | 558,884,081 → 558,883,925 |
| `interp_load fib 27` | 241,521,829 → 241,521,923 | 244,920,386 → 244,921,084 |
| `interp_load closures 1000000` | 1,739,274,036 → 1,739,273,714 | 1,742,653,953 → 1,742,653,882 |
| `interp_load objects 1000000` | 699,863,750 → 699,863,415 | 703,232,281 → 703,232,288 |

All five interpreter rows passed the 0.5% gate under both allocators.

## Compatibility

The `crates/js-diff` runner used test262 at `e4fbf57745c6aae3d1f12f181ecef78b1665d812` and the repository's skip rules. The `built-ins/RegExp` result was 1,125 passes, 95 failures, and 659 skips on both builds. The pass sets are identical. Batched runs with one timeout per directory timed out in the generated Unicode cases. Smaller batches completed every case.

`cargo test --locked -p bare-js2 --lib regexp` and `cargo check --locked -p bare-js2 -p bare` passed. The benchmark comparison uses the frozen commits above. The final merge includes an OLD session getter change from #931 and documentation changes from #932.

The live Rust-lang.org debugger trace saw 36 pointer hits on creation and zero content hits. The DEV tape used 6.05 M instructions in 124 program compiles. Python.org still has 46.31 M instructions in the compile pass. A separate change can reuse the validated parse for the first match.

Not tested: the full local suites, Miri, Windows, and the rest of the public journey cohort.
