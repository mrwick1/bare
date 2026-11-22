# 2026-11-22 — Parse bulk js2 data literals as checked spans

The 3,806,411-byte Python Docs search-index script builds a large general syntax tree before js2 lowers it to a data recipe. Keep a checked source span for a large JSON-subset literal in a lazy parse of a source of at least 2 MiB. Lower that span directly to the existing recipe. Count the original syntax nodes and list entries and fall back when a limit can apply. Keep ordinary parsing for potential assignment patterns, prototype setters, and syntax outside the subset.

The new test measured 560 expression nodes on the baseline and one after the change. All five focused tests passed. They cover parser limits, Unicode escapes and lone surrogates, negative zero, exponent notation, repeated keys, destructuring, prototype setters, elisions, and fresh objects. The `bare-js2` and `bare` typecheck passed.

## Journey measurements

archMachine, Ryzen 9 5900HX, Rust 1.96.0, perf 7.3.1. Compare release binaries with debug symbols on the engine source at `862ab0c3` and the branch. Main at `0d91226d` has the same baseline code. Use the commands and frozen tapes in the [design](../design-js2-real-pages-2.md). Rotate builds and engines. Instruction samples use `instructions:u` with full counter coverage. RSS samples use `wait4`. Each cell gives a minimum instruction count in M or median peak RSS in MiB, followed by its range.

| Python Docs search | OLD instructions, branch binary | js2 before | js2 after | js2 RSS before | js2 RSS after |
|---|---:|---:|---:|---:|---:|
| Live | 1,072.28 [1,072.28–1,073.51] | 1,454.97 [1,454.97–1,457.16] | 1,129.59 [1,129.59–1,138.34] | 75.29 [75.11–75.93] | 75.43 [75.07–76.30] |
| Replay | 1,058.51 [1,058.51–1,058.58] | 1,440.81 [1,440.81–1,441.40] | 1,115.21 [1,115.21–1,115.58] | 75.48 [74.79–75.87] | 74.95 [74.35–75.61] |

All instruction samples have identical replies by URL, status, and byte count in all builds and modes. Live has 40 replies and 42,122 bytes. Replay has 40 replies and 957,162 bytes. The live RSS samples also match. The replay RSS reports have 40 replies on baseline js2 and 41 on branch js2. The extra reply is a zero-byte 304. Use the matching live samples for the RSS gate. The live median rose 0.14 MiB within the 0.82 MiB baseline spread.

The target saves 325.38 M live and 325.60 M on the tape. The search-results step with its list assertion also passes on both builds on this tape. That extended step asks for result snippets that the count tape lacks.

## Controls

The first build tried the scanner in every lazy or first-call parse. The DEV tape rose from 3,281.06 M to 3,287.43 M. Callgrind observed 23,084 candidate checks and 10.39 M inclusive instructions in the probe. The second build requires a source of at least 2 MiB and excludes first-call parses. All rerun minima are within the baseline spread. There are three samples per cell, except six per build for DEV replay. At three samples the DEV replay increase was 0.89 M against a 0.73 M baseline spread. Three additional paired trials of the same binaries gave a 1.10 M baseline spread.

| Control | js2 before, M | js2 after, M | Change, M |
|---|---:|---:|---:|
| w3c-home-links live | 877.21 [877.21–888.90] | 882.72 [882.72–888.29] | +5.51 |
| w3c-home-links replay | 848.95 [848.95–849.33] | 848.52 [848.52–849.44] | -0.44 |
| python-org-docs-link live | 1,203.00 [1,203.00–1,219.48] | 1,196.07 [1,196.07–1,222.88] | -6.93 |
| python-org-docs-link replay | 1,168.66 [1,168.66–1,168.80] | 1,168.75 [1,168.75–1,168.92] | +0.09 |
| rust-book-search live | 1,833.55 [1,833.55–1,842.93] | 1,833.50 [1,833.50–1,873.30] | -0.05 |
| rust-book-search replay | 5,604.66 [5,604.66–5,605.10] | 5,605.10 [5,605.10–5,605.21] | +0.44 |
| devto-home live | 5,257.51 [5,257.51–5,889.13] | 5,420.52 [5,420.52–18,254.24] | +163.01 |
| devto-home replay | 3,280.16 [3,280.16–3,281.26] | 3,281.05 [3,281.05–3,281.82] | +0.89 |
| python-org-search live | 1,246.19 [1,246.19–2,847.16] | 1,244.08 [1,244.08–1,249.54] | -2.11 |
| python-org-search replay | 1,188.59 [1,188.59–1,188.68] | 1,188.55 [1,188.55–1,188.69] | -0.04 |
| typing live | 5,480.28 [5,480.28–5,481.28] | 5,480.93 [5,480.93–5,491.50] | +0.64 |

The W3C tape still misses the background POST. Use W3C live traffic for its gate. DEV live and Python.org search live vary in background work. The matched tapes provide the tighter control for those journeys.

## Compatibility and cost

Run js2 through `crates/js-diff` on the pinned test262 checkout and the repository skip rules. Across the array, object, assignment, arrow and destructuring directories, js2 has 1,722 passes, 36 failures, and 311 skips on each build. No passing test was lost.

The two `cost_matrix.sh` rows passed under glibc and jemalloc. `js2_lex_load small 1 1` changed +0.000% and −0.004%. `js2_parse_load full stmts 1` changed −0.013% and −0.009%. No cost debt.

| Interpreter row | glibc instructions before → after | jemalloc instructions before → after |
|---|---:|---:|
| `loopfn 3000000` | 800262763 → 800262521 | 803664115 → 803664378 |
| `calls 1000000` | 555302257 → 555301942 | 558672171 → 558672205 |
| `fib 27` | 241309632 → 241309596 | 244708671 → 244708493 |
| `closures 1000000` | 1739061332 → 1739062252 | 1742415070 → 1742414854 |
| `objects 1000000` | 699650961 → 699651474 | 703020981 → 703021193 |

Not tested locally: Windows, windowed paint, idle-machine latency, the full workspace suite, or the full test262 suite.
