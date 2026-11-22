# 2026-11-22 — Encode straight-line js2 bytecode once

The assembler reads operand letters and jump fields from a static table. When no instruction refers to a symbolic target, it encodes each instruction once and records PCs in the same pass. Bound and unbound labels keep their existing PCs. Code with symbolic jumps uses the existing relaxation pass.

## Measurements

archMachine, Ryzen 9 5900HX. Locked release builds. Base `9c8e9f28`, change `232e8a22`. Inputs and completion runner come from the webpack design measurements. Instructions show the minimum and range of three cold processes with `perf stat -e instructions:u`. RSS shows the median and range of three separate `wait4` samples in KiB.

| Workload | Base instructions, M | New instructions, M | Base peak RSS, KiB | New peak RSS, KiB |
|---|---:|---:|---:|---:|
| Empty | 0.719–0.721 | 0.719–0.720 | 5156 [5004–5176] | 5248 [5148–5252] |
| webpack 1 | 6.817–6.818 | 6.193–6.194 | 6076 [6044–6100] | 6000 [5904–6092] |
| webpack 10 | 52.321–52.337 | 46.980–46.990 | 7460 [7432–7480] | 7452 [7356–7500] |
| React eager compile | 317.274–317.334 | 288.376–288.432 | 7656 [7492–7800] | 7600 [7564–7708] |
| Closures 10 | 194.714–194.716 | 194.686–194.687 | 11136 [11120–11236] | 11196 [11132–11268] |
| Throws 10 | 299.119–299.120 | 299.094–299.095 | 10580 [10572–10756] | 10628 [10548–10644] |

Both webpack checksums matched. Callgrind on webpack 10 measured 6.574 M to 2.481 M inclusive instructions in `Asm::finish`. `encode_in` fell from 5.340 M to 1.777 M inclusive instructions.

| Cost row | Base instructions, M | New instructions, M |
|---|---:|---:|
| interp_load loopfn 3000000 | 866.297–866.298 | 866.273–866.274 |
| interp_load calls 1000000 | 571.348–571.348 | 571.311–571.312 |
| interp_load fib 27 | 248.668–248.669 | 248.628–248.628 |
| interp_load closures 1000000 | 1942.671–1942.672 | 1942.600–1942.602 |
| interp_load objects 1000000 | 744.193–744.194 | 744.157–744.157 |

`cost_matrix.sh -g '^examples/js2_(gen|lex|parse|promise)_load '` passed all four rows. Minimum changes were −0.037% for generators, +0.002% for lexing, −0.053% for parsing and −0.013% for promises. The five interpreter rows passed with glibc. Jemalloc and codegen-units=1 controls were unnecessary.

### Browser replay

Three fresh headless `bare-drive` processes per binary and journey with `BARE_JS_EXEC=new` and `BARE_SEED=1`. The binaries replayed a single local copy of the public replies. All 18 runs passed their journey assertions. DEV had two replay misses in every run. React and Substack had none. Each cell gives a range or the RSS median with its range.

| Journey | Base instructions, M | New instructions, M | Base peak RSS, KiB | New peak RSS, KiB |
|---|---:|---:|---:|---:|
| react-docs | 2493.36–2536.08 | 2475.16–2514.39 | 60948 [57792–61128] | 60232 [59632–60352] |
| substack-home | 1641.96–1643.15 | 1619.32–1629.07 | 60108 [58616–60996] | 60636 [60272–61228] |
| devto-home | 2919.81–2978.15 | 2938.11–3070.54 | 69820 [69812–70680] | 72760 [70672–72824] |

The DEV minimum grew by 0.63%. Its range spans 2.0% on base and 4.5% on the change. The measured peak grew for DEV and Substack. Raw samples and logs are in `/data/bare-tmp/r22/js2-parse-once/`.

## Tests

The encode-count regression test observed six calls for three instructions on base and three calls after the fix. The assembler tests compare words and PCs for all valid opcodes and check operand overflow. Existing tests cover crossing wide jumps and loop metadata. All 14 bytecode tests passed. `cargo check -p bare-js2 -p bare` passed.

The test262 comparison covered 22 directories for conditional expressions, functions, arrows, `for` and `try`. Both js2 builds passed 1,534 tests, failed 116 and skipped 16. Lost passes: zero.

Not tested: full suite, Windows, idle wall time.
