# 2026-11-22 — Skip decimal atoms on indexed property misses

The host-aware lookup formatted and interned an index after a dense-element miss, even on shapes with no named indices. Ordinary descriptor lookup already avoided interning. It still formatted indices when an immutable shape could rule out a match.

Use the shape's index-name flag to skip that work. Use the existing lookup-only atom probe for dictionary shapes and shapes that can contain a named index. Keep dense elements, host lookup order, and lazy callbacks on their existing paths.

The new test failed because a numeric miss created an atom. The fix kept the atom absent and still called the host. Another test checks a dense hit, a name added after a miss, an inherited getter with the original receiver, and a dictionary shape. All 23 host tests passed. The `bare-js2` and `bare` typecheck passed.

## Journey measurements

Use the archMachine release build with symbols and the method in the [design](../design-js2-real-pages-2.md). Compare the checked-span parser build in PR #945 with the same build plus this change. Each cell gives three passing native samples. Instructions are the minimum and range in M. Peak RSS is the median and range in MiB. All native instruction counters have 100% coverage.

| W3C home links | OLD instructions, branch binary | js2 before | js2 after | js2 RSS before | js2 RSS after |
|---|---:|---:|---:|---:|---:|
| Live | 699.67 [699.67–722.83] | 876.50 [876.50–882.88] | 743.31 [743.31–748.36] | 29.87 [29.63–30.45] | 29.14 [28.98–29.26] |
| Replay | 690.56 [690.56–690.62] | 848.50 [848.50–849.14] | 715.43 [715.43–715.48] | 29.39 [29.18–29.50] | 28.86 [28.68–28.92] |

Every live W3C js2 sample has the same seven replies and 83,582 bytes. All three OLD branch samples match. One baseline OLD sample received only four replies. Both builds' replays report the same 11 replies and 134,910 bytes and still miss a background POST. Use the live rows for the gate.

The live minimum falls by 133.18 M. The live RSS median falls by 0.73 MiB. In the branch Callgrind profile, about 270,000 `hs_plain` calls cost 25.44 M inclusive. The original live profile had about the same call count at 112.96 M. The profiles have different background replies. The native instruction comparison uses matching replies.

| Control | js2 before, M | js2 after, M | Change, M |
|---|---:|---:|---:|
| python-docs-search live | 1,129.37 [1,129.37–1,132.36] | 1,127.92 [1,127.92–1,132.92] | -1.46 |
| python-docs-search replay | 1,115.42 [1,115.42–1,115.62] | 1,113.86 [1,113.86–1,113.94] | -1.56 |
| python-org-docs-link live | 1,201.50 [1,201.50–1,331.24] | 1,201.30 [1,201.30–1,244.87] | -0.19 |
| python-org-docs-link replay | 1,168.82 [1,168.82–1,168.87] | 1,167.68 [1,167.68–1,167.85] | -1.14 |
| rust-book-search live | 1,842.52 [1,842.52–1,853.66] | 1,806.30 [1,806.30–1,817.94] | -36.21 |
| rust-book-search replay | 5,604.39 [5,604.39–5,605.24] | 5,395.76 [5,395.76–5,396.06] | -208.62 |
| devto-home live | 5,093.63 [5,093.63–6,881.17] | 5,253.15 [5,253.15–5,466.33] | +159.52 |
| devto-home replay | 3,281.16 [3,281.16–3,281.39] | 3,274.46 [3,274.46–3,275.16] | -6.70 |
| python-org-search live | 1,246.30 [1,246.30–1,252.57] | 1,243.41 [1,243.41–1,245.56] | -2.89 |
| python-org-search replay | 1,188.61 [1,188.61–1,188.73] | 1,187.58 [1,187.58–1,187.63] | -1.03 |
| typing live | 5,479.71 [5,479.71–5,481.28] | 5,458.07 [5,458.07–5,459.83] | -21.64 |

All control instruction changes are within the baseline spread or are reductions. The live DEV minimum rose 159.52 M within a 1,787.54 M base spread. Its matched tape improved by 6.70 M. A live Python.org documentation RSS trial reached 52.02 MiB. Its replay RSS median fell 0.09 MiB. React typing peak RSS rose from 30.45 MiB [30.41–30.63] to 30.86 MiB [30.66–30.87]. Recheck this at the end of the round.

## Compatibility and cost

Targeted test262 covers property accessors, assignment, `in`, and affected Array, Object, and Reflect methods. js2 has 2,615 passes, 40 failures, and four skips before and after. No passing test was lost.

| Interpreter row | glibc instructions before → after | jemalloc instructions before → after |
|---|---:|---:|
| `loopfn 3000000` | 800262650 → 800262786 | 803663803 → 803664433 |
| `calls 1000000` | 555302063 → 555301985 | 558671548 → 558672543 |
| `fib 27` | 241309459 → 241307410 | 244708787 → 244708222 |
| `closures 1000000` | 1739061810 → 1739061527 | 1742414268 → 1742414827 |
| `objects 1000000` | 699651102 → 699651443 | 703021387 → 703020794 |

The 12 Array, Object and adapter rows in `cost_matrix.sh` pass under both allocators. Those rows use the same OLD example binaries on both sides because the OLD source is unchanged. The live and replay rows test the js2 paths. No cost debt.

## Rerun after typed-array lookup changed on main

PR #944 merged while this PR was in CI. It gives typed arrays a direct integer-indexed read in host prototype walks. The historical W3C comparison above includes work that #944 also removes. Use the new comparison below for the current effect of this PR.

The baseline source is `cad56767`. In the matched live Callgrind pair it reports five replies and 64,947 bytes. On baseline js2, `hs_plain` calls `idx_atom` 205 times for 0.08 M inclusive instructions. The earlier js2 live profile had 176,341 calls and 76.70 M. The ordinary indexed setter's prototype lookup still costs 33.75 M at 31,633 calls. It can still avoid probing a name on an immutable shape that has no named indices.

The patched Callgrind run reports the same five replies and 64,947 bytes. The setter's prototype lookup falls to 7.41 M at 31,679 calls. Total js2 instructions fall from 744.75 M to 707.83 M in this pair.

| W3C input | OLD on branch, M | js2 before, M | js2 after, M | js2 RSS before, MiB | js2 RSS after, MiB |
|---|---:|---:|---:|---:|---:|
| Live | 702.57 [702.57–716.33] | 687.86 [687.86–702.49] | 665.58 [665.58–673.75] | 28.37 [28.35–28.41] | 27.77 [27.55–28.06] |
| Replay | 690.43 [690.43–690.47] | 657.47 [657.47–657.51] | 631.18 [631.18–631.21] | 27.81 [27.63–27.93] | 27.79 [27.68–28.07] |

All live W3C samples match at seven replies and 83,582 bytes. The live instruction minimum falls 22.27 M and median RSS falls 0.60 MiB. Replay still has the same POST miss. All control instruction minima fall or stay within the baseline spread. The first three DEV tape RSS trials have medians of 100.18 MiB and 103.66 MiB. Three more trials per build give medians of 100.34 MiB [99.88–106.54] and 105.68 MiB [103.49–108.17] across all six. The increase stays within the measured baseline spread.

The expanded host test262 selection adds typed-array internal Get and HasProperty. It gives 2,634 passes, 57 failures and 28 skips on both trees. No passing test was lost. All 24 host unit tests pass. The typecheck, the 12 cost rows and the five interpreter cost rows pass. No cost debt.

Not tested locally: Windows, windowed paint, idle-machine latency, the full workspace suite, and the full test262 suite.
