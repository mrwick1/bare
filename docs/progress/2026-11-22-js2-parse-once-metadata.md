# 2026-11-22 — Record js2 function metadata while emitting bytecode

The compiler records argument and `super` flags from emitted ops. The loader propagates the `super` flag when it links child code. It derives stub flags from the function facts. Public VM construction still scans externally supplied bytecode.

The compiler copies live register bits at each safepoint. A small frame uses one word per safepoint and transfers that buffer to the code. Larger frames pad each snapshot to the final width.

## Measurements

ArchMachine, Ryzen 9 5900HX. Locked release builds with the assembler step in both trees. Base `e371b1cb`. Each instruction range is three cold `perf stat -e instructions:u` runs. Peak RSS shows the median and range of three separate `wait4` runs in KiB.

| Workload | Base instructions, M | New instructions, M | Base peak RSS, KiB | New peak RSS, KiB |
|---|---:|---:|---:|---:|
| Empty | 0.713–0.714 | 0.719–0.719 | 5172 [5148–5208] | 5148 [5000–5160] |
| webpack 1 | 6.187–6.188 | 5.838–5.839 | 6024 [5940–6112] | 5960 [5884–5968] |
| webpack 10 | 46.980–46.988 | 43.969–43.975 | 7364 [7360–7424] | 7364 [7260–7500] |
| React eager compile | 288.415–288.489 | 275.824–275.864 | 7628 [7584–7708] | 7584 [7476–7596] |
| Closures 10 | 194.681–194.681 | 194.671–194.672 | 11108 [11100–11184] | 11052 [10996–11072] |
| Throws 10 | 299.089–299.090 | 299.082–299.083 | 10692 [10512–10728] | 10572 [10552–10676] |

Both webpack checksums matched. Callgrind attributed 4.662 M → 3.313 M inclusive instructions to `Fc::finish` on webpack 10. The first implementation copied all maps into a second buffer. Its webpack 10 median was 7460 KiB. Transferring the one-word buffer kept the median at 7364 KiB and saved more compiler work.

| Cost row | Base instructions, M | New instructions, M |
|---|---:|---:|
| interp_load loopfn 3000000 | 866.274–866.274 | 866.274–866.275 |
| interp_load calls 1000000 | 571.311–571.312 | 571.312–571.312 |
| interp_load fib 27 | 248.628–248.628 | 248.628–248.629 |
| interp_load closures 1000000 | 1942.608–1942.608 | 1942.611–1942.612 |
| interp_load objects 1000000 | 744.159–744.160 | 744.160–744.161 |

`cost_matrix.sh -g '^examples/js2_(gen|lex|parse|promise)_load '` passed all four rows. Minimum changes were +0.000% for generators, −0.000% for lexing, +0.022% for parsing and −0.004% for promises. All five interpreter rows passed with glibc. No allocator or codegen-unit control was needed.

### Browser replay

Three headless runs per binary and journey with `BARE_JS_EXEC=new` and `BARE_SEED=1`. The [assembler step](2026-11-22-js2-parse-once-assembler.md) used the same local replay. All 18 runs passed. DEV had two replay misses per run. React and Substack had none.

| Journey | Base instructions, M | New instructions, M | Base peak RSS, KiB | New peak RSS, KiB |
|---|---:|---:|---:|---:|
| react-docs | 2515.40–2588.73 | 2573.96–2578.37 | 58316 [57880–58812] | 60588 [60472–60996] |
| substack-home | 1619.09–1624.80 | 1605.85–1609.29 | 59412 [57396–59740] | 59088 [58732–61336] |
| devto-home | 3007.02–3130.34 | 2934.77–2970.35 | 70856 [69784–71568] | 74080 [68440–74988] |

React's minimum instructions increased by 2.33%. Its median decreased by 0.39%. The measured peak grew for React and DEV. Raw samples and logs are in `/data/bare-tmp/r22/js2-parse-once/`.

## Tests

The flag regression test failed before the change and passed afterward. It compares emitted flags to the decoder on arguments, rest and spread, generators, async functions and `super`. The map test checks exact bits across register words and a branch join. The accessor test verifies collection within a call and reads a caller local after the catch handler reenters.

Narrow compiler and script tests passed. `cargo check -p bare-js2 -p bare` passed. Test262 covered 131 directories for the argument object, rest parameters, classes, `super`, generators, async functions, `try`, `apply` and `call`. Both js2 builds had 6,593 passes, 308 failures and 2,367 skips. Lost passes: zero.

Not tested: full suite, Windows, idle wall time.
