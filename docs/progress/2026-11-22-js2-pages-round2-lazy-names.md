# 2026-11-22 — Convert a lazy host name only on the first ask

The host-aware property path converted a name to text before it looked in the existing asked set. Move conversion after the cache miss and after the host check. Keep the object and key identity, callback order, exception handling, and reset at garbage collection.

The new test failed on four conversions for two distinct names. It now records two. It also checks index, Unicode name, symbol and enumeration calls across a collection. All 24 host tests and the `bare-js2` and `bare` typecheck pass.

## Journey measurements

Compare the checked-span and indexed-probe build with the same build plus this change. Use the archMachine release build with symbols and the [design method](../design-js2-real-pages-2.md). Values give the minimum and range of instruction counts in M, or median and range of peak RSS in MiB. Three trials per cell, with six js2 live instruction trials for each Python.org journey due to a timing spread in the initial set.

| Python.org docs link | js2 before, M | js2 after, M | js2 RSS before, MiB | js2 RSS after, MiB |
|---|---:|---:|---:|---:|
| Live | 1,206.67 [1,206.67–1,224.35] | 1,198.78 [1,198.78–1,235.02] | 37.74 [37.67–38.31] | 37.89 [37.65–37.94] |
| Replay | 1,167.64 [1,167.64–1,167.84] | 1,162.93 [1,162.93–1,163.04] | 38.25 [37.91–38.29] | 38.17 [37.91–38.50] |

The target samples match by reply URL, status, and byte count. Live has 16 replies and 331,870 bytes. Replay has 20 replies and 541,271 bytes. The live minimum fell 7.89 M. The replay minimum fell 4.72 M. The live RSS median rose 0.15 MiB within its baseline spread. The tape median fell 0.08 MiB.

In Callgrind, both tape profiles have the same 20 replies. The `hs_lazy` edge to `Realm::text` fell from 14,987 calls and 5.29 M inclusive instructions to 3,088 calls and 1.13 M. Total `Ir` fell from 1,216.30 M to 1,211.08 M.

| Control | js2 before, M | js2 after, M | Change, M |
|---|---:|---:|---:|
| python-docs-search live | 1,127.87 [1,127.87–1,130.16] | 1,121.30 [1,121.30–1,126.26] | -6.57 |
| python-docs-search replay | 1,113.91 [1,113.91–1,114.37] | 1,106.33 [1,106.33–1,106.40] | -7.58 |
| w3c-home-links live | 741.26 [741.26–744.60] | 724.11 [724.11–743.80] | -17.15 |
| w3c-home-links replay | 715.30 [715.30–715.35] | 710.96 [710.96–711.02] | -4.34 |
| rust-book-search live | 1,809.30 [1,809.30–1,814.54] | 1,797.15 [1,797.15–1,805.81] | -12.15 |
| rust-book-search replay | 5,395.83 [5,395.83–5,396.26] | 5,394.81 [5,394.81–5,395.06] | -1.02 |
| devto-home live | 5,811.40 [5,811.40–12,424.83] | 7,761.10 [7,761.10–8,706.33] | +1,949.70 |
| devto-home replay | 3,275.06 [3,275.06–3,275.40] | 3,270.77 [3,270.77–3,270.92] | -4.28 |
| python-org-search live | 1,241.70 [1,241.70–1,258.51] | 1,239.71 [1,239.71–1,362.50] | -2.00 |
| python-org-search replay | 1,187.64 [1,187.64–1,187.81] | 1,182.73 [1,182.73–1,182.90] | -4.91 |
| typing live | 5,459.29 [5,459.29–5,460.57] | 5,226.04 [5,226.04–5,230.09] | -233.25 |

DEV live varies in background work. The change in its live minimum stays within the baseline spread. The matched DEV tape improves by 4.28 M. The W3C tape still misses a background POST. Its live minimum improves by 17.15 M. No control instruction minimum rises by more than its baseline spread.

## Compatibility and cost

Targeted test262 covers affected Array, Object, and Reflect methods and property access, assignment and `in`. js2 retains all 2,615 passes, with 40 failures and four skips before and after.

All ten adapter cost rows pass under both allocators. These rows use the same OLD example binaries because the OLD source is unchanged. The browser measurements exercise the js2 host path. No cost debt.

| Interpreter row | glibc instructions before → after | jemalloc instructions before → after |
|---|---:|---:|
| `loopfn 3000000` | 800262405 → 800262278 | 803663624 → 803664376 |
| `calls 1000000` | 555302524 → 555302220 | 558671401 → 558671416 |
| `fib 27` | 241309358 → 241309544 | 244708675 → 244708873 |
| `closures 1000000` | 1739061211 → 1739061380 | 1742414373 → 1742414201 |
| `objects 1000000` | 699651294 → 699650823 | 703020871 → 703020602 |

Not tested locally: Windows, windowed paint, idle latency, the full workspace suite, and the full test262 suite.
