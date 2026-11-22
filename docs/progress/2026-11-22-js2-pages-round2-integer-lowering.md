# 2026-11-22 — Emit small integer data values during the digit scan

The Python Docs search index has 341,428 integer tokens with at most nine digits. The checked-span parser already walks these digits during lowering. It used the general decimal-to-float converter on each token before it made an integer value.

Accumulate the digits in the lowering pass. Emit a tagged integer for a decimal with at most nine digits and no fraction or exponent. Keep the existing converter for negative zero and other tokens. The first pass checks syntax without accumulating a value.

The new test counted 559 calls to the general converter before the fix and six after it. It checks signed values at the fast-path bounds, both zeros, fractions, exponents, extended integer rounding and cancellation between a long mantissa and exponent.

## Journey measurements

Use the archMachine release build and the [design method](../design-js2-real-pages-2.md). Compare `71c71e0b`, with the same files as main at `33e5b4b1`, with that source plus this change. The build includes the indexed-probe change. The lazy host name change is absent from both sides. Use three passing samples for each cell. Instruction counts give the minimum and range in M. Peak RSS gives the median and range in MiB.

| Python Docs search | js2 before, M | js2 after, M | js2 RSS before, MiB | js2 RSS after, MiB |
|---|---:|---:|---:|---:|
| Live | 1,127.41 [1,127.41–1,129.59] | 1,056.33 [1,056.33–1,064.02] | 75.20 [74.97–75.39] | 75.23 [75.16–75.65] |
| Replay | 1,113.43 [1,113.43–1,113.69] | 1,042.41 [1,042.41–1,042.77] | 74.84 [74.40–75.13] | 74.90 [74.71–75.19] |

The live samples have the same 40 replies and 42,122 bytes. The live minimum falls 71.08 M. The matched replay has 41 replies and 957,162 bytes, including a zero-byte 304 reply. It falls from 1,113.57 M to 1,042.41 M. One baseline sample with 40 replies gives the lower all-sample baseline minimum in the table. The live and replay RSS medians stay within their baseline spreads.

In an earlier tape profile on the source with the lazy host name change, both sides had 41 replies and 957,162 bytes. Calls from `f64::from_str` to `parse_number` fell from 341,991 to 563. The difference is 341,428, which matches the integer tokens in the asset. Total `Ir` fell from 1,123.03 M to 1,052.45 M.

The Rust Book and DEV replay instruction minima rise by 0.40 M and 0.56 M within their respective baseline spreads. React typing rises 2.69 M within its 3.48 M baseline spread. The first three Python.org search live trials per tree have different reply sets. Three more trials per tree give a shared 16-reply set. The matched minimum falls from 1,245.23 M to 1,244.95 M. The matched tape falls by 0.02 M. The W3C tape continues to miss the background POST. Its live minimum falls 1.15 M.

## Compatibility and cost

All four data literal tests pass. The `bare-js2` and `bare` typecheck passes. Targeted test262 covers number literals, arrays and objects. It has 1,066 passes, six failures and 307 skips on both trees. No passing test was lost.

| Cost row | glibc instructions before → after | jemalloc instructions before → after |
|---|---:|---:|
| `js2_parse_load full stmts 1` | 10929514 → 10926404 | 17549860 → 17551068 |
| `js2_lex_load small 1 1` | 114986091 → 114986472 | 121320590 → 121321298 |
| `loopfn 3000000` | 800263867 → 800264243 | 803665574 → 803665112 |
| `calls 1000000` | 555302805 → 555303308 | 558673050 → 558672839 |
| `fib 27` | 241310432 → 241310529 | 244709926 → 244709224 |
| `closures 1000000` | 1739062859 → 1739062719 | 1742415340 → 1742416010 |
| `objects 1000000` | 699653242 → 699652859 | 703022740 → 703022555 |

Both parser and lexer rows and all five interpreter rows pass. No cost debt.

Not tested locally: Windows, windowed paint, idle latency, the full workspace suite, and the full test262 suite.
