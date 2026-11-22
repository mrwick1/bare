# Check required regexp suffixes before matching

## Baseline after #930

Frozen main was `5540846b`. Both engines used the same release `bare-drive` binary with debug symbols and no strip. archMachine ran Linux on an AMD Ryzen 9 5900HX. Rust was 1.96.0, perf was 7.3.1, and Callgrind was 3.25.1. Each live row uses three passing runs with `BARE_SEED=1`, `LC_ALL=C`, and `glibc.malloc.tcache_count=65535`.

| Rust Book live baseline | Min M instructions | Range M | Median peak RSS KiB | RSS range KiB |
|---|---:|---:|---:|---:|
| OLD | 2,332.9 | 2,332.9–2,339.2 | 106,920 | 106,336–107,224 |
| js2 | 4,194.8 | 4,194.8–4,205.9 | 101,692 | 100,804–101,780 |

Every run passed with 21 replies. One OLD run took 870,647 bytes. The other five runs took 870,650 bytes. The js2 minimum was 66.2 M below the 4,261 M cited in #927. The OLD minimum was 36.1 M below the cited 2,369 M. The build and response variance limit attribution of the change to #930 alone.

## Matcher profile

On frozen main, Callgrind measured 746.94 M self and 766.17 M inclusive instructions in the js2 matcher, versus 326.90 M self and 335.06 M inclusive in OLD. The two profiles passed with 21 replies each. Both engines ran 165,966 builtin tests. Each js2 test called the matcher once, and other regexp operations made 9,075 calls. OLD called the matcher 55,611 times from builtin tests and 9,075 times from other operations. Its result table skips repeated tests of the same program, subject and start, and charges the saved steps on a hit. Both js2 subject widths were already scanned directly.

To compare matching with QuickJS 2026-06-04, replay the builtin tests of the Porter stemmer on the words in the 30 fixture results for `ownership`. Extract the words with the page's HTML escape, lowercasing and sentence and word splitting. Capture the actual patterns, inputs, and answers from elasticlunr. Replay all 165,835 tests ten times on each direct matcher and check every answer. Keep the QuickJS source read-only. Build the C matcher with GCC `-O2 -g` and the Rust matcher with `rustc -O -g`. Use one-byte subjects when all units fit and two-byte subjects for the rest. The replay excludes JS result allocation in both engines.

| Matcher on the fixture trace | Self M per pass | Self instructions per call | Inclusive instructions per call |
|---|---:|---:|---:|
| js2 `Prog::exec_in` | 714.53 | 4,309 | 4,417 |
| QuickJS `lre_exec` with inlined backtracking | 607.41 | 3,663 | 3,772 |

The longest suffix alternation averaged 14,013 self instructions per call on js2 and 11,120 on QuickJS. Bytecode fetch, operand decode and dispatch took 53.7 M instructions in js2 for that pattern alone. The step counter took 29.0 M and the pop site took 5.3 M. QuickJS still backtracked. It fetched a byte opcode and kept the capture array and backtrack stack flat. QuickJS capture initialization for that pattern used about 0.08 M instructions. The js2 browser profile attributed 14.84 M inclusive to out-of-line class matching, 5.6 M to flattening under the test call path, and 0.81 M to refilling the fuel block. The main cost is repeated matcher steps through lazy dot and suffix alternatives on words that cannot end in a matching literal.

## Required final literal

Record a 16-byte ASCII bitmap in the image when a terminal `$` follows a literal or a group or alternation that always ends in a literal. Check the final code unit before the backtracking matcher. Skip the check for multiline, ignore-case and Unicode patterns. Matchers without a proof use the regular path. A rejected suffix costs one matcher step. A viable candidate uses the existing fuel and step charges. The wrapper dispatches custom `exec` and applies `lastIndex` updates.

The same fixture trace fell from 165,835 to 40,282 match-loop calls and from 714.53 M to 179.36 M self instructions. All answers agreed with the trace. The check removes the same work for each caller of the matcher.

## Browser comparison

Run three measurements per build and engine against the frozen base. Reverse the order on the second trial. Use live Rust Book and Rust-lang.org. Copy the DEV and Python.org tapes and use the virtual clock. Count user instructions with `perf stat -e instructions:u`. All runs passed at 100% counter coverage.

| Journey and engine | Base min [range] M | Patch min [range] M | Replies |
|---|---:|---:|---:|
| Rust Book js2 | 4,188.9 [4,188.9–4,199.7] | 3,634.5 [3,634.5–3,644.5] | 21 |
| DEV js2 | 3,328.02 [3,328.02–3,330.12] | 3,328.84 [3,328.84–3,329.51] | 16 |
| Python.org js2 | 1,374.27 [1,374.27–1,374.46] | 1,374.34 [1,374.34–1,374.61] | 17 |
| Rust-lang.org js2 | 98.81 [98.81–99.20] | 98.81 [98.81–99.13] | 14 |
| Rust Book OLD | 2,330.5 [2,330.5–2,337.1] | 2,327.5 [2,327.5–2,339.9] | 21 |
| DEV OLD | 3,199.00 [3,199.00–3,200.08] | 3,198.75 [3,198.75–3,199.69] | 16 |
| Python.org OLD | 1,165.59 [1,165.59–1,165.77] | 1,165.63 [1,165.63–1,165.73] | 17 |
| Rust-lang.org OLD | 87.37 [87.37–87.82] | 87.44 [87.44–87.81] | 14 |

Rust Book js2 saved 554.4 M instructions (13.2%). The three control minima stayed within their baseline spreads in both engines. Rust Book peak RSS from `wait4` was 101,728 KiB median [100,880–101,884] on base js2 and 102,000 KiB [101,756–102,024] with the patch. The median increased by 272 KiB within a 1,004 KiB baseline spread.

The passing Callgrind runs on Rust Book kept 165,966 builtin tests on each build. The patch reduced matcher invocations from 175,041 to 49,477 and matcher self cost from 746.94 M to 207.20 M. OLD used 326.90 M. The inclusive builtin test cost fell from 825.44 M to 269.29 M. The drop covers the matcher and its call overhead.

## Cost rows

The 12 published regexp rows use `cost_matrix.sh -aJ` for `builtins_load` scan, replace, split, routes, zero and gc at scales 1 and 10. The largest increase was +0.001% on glibc and +0.060% on jemalloc. The harness uses OLD for these rows. Count the five `interp_load` rows three times per allocator and report the change in minima.

| js2 micro row | glibc | jemalloc |
|---|---:|---:|
| loopfn 3000000 | +0.0003% | +0.0002% |
| calls 1000000 | +0.0004% | −0.0051% |
| fib 27 | +0.0009% | +0.0009% |
| closures 1000000 | +0.0002% | +0.0001% |
| objects 1000000 | +0.0004% | +0.0005% |

The standalone js2 `regexp_row` used identical checksums on both builds. Staging went from 19.89 M to 19.28 M instructions for 9,400 calls. Porter went from 1,388.20 M to 653.94 M for 380,000 calls.

## Checks

The new fuel regression failed on frozen main with `Err(Fuel)` for an impossible suffix. The patch passed all 62 focused RegExp unit tests. `cargo check --locked -p bare-js2 -p bare` passed.

After fast-forwarding to `a17eab43`, repeat the four js2 journeys. All passed with the same reply counts. Rust Book measured 3,632.8–3,638.0 M instructions and a median peak RSS of 101,636 KiB [101,600–101,992]. The other minima were 3,327.42 M (DEV), 1,374.34 M (Python.org) and 98.65 M (Rust-lang.org). The focused unit tests and typecheck passed again.

RegExp Test262 on js2 gave 1,125 pass, 95 fail and 659 skip on both builds in `built-ins/RegExp`. Annex B gave 8 pass, 23 fail and 31 skip on both builds. Rerun three generated Unicode tests individually after directory timeouts. Two passed on both builds and one failed on both with the same assertion. The combined pass sets are identical.

Raw logs are under `/data/bare-tmp/r22/js2-regexp-test/` on archMachine. Windows, the full workspace suite and the rest of the public journey cohort were outside this check.
