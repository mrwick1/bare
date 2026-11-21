# 2026-11-21: Keep js2 register cleanup linear in large scopes

PR #836.

## Cause and fix

Each declaration calls `Fc::free` after it emits the initializer. `Fc::below` visited every word in the live-register and variable-register bitsets. A function with N locals keeps about N/64 words in each bitset. N declarations therefore pay for O(N²) word visits.

Shorten each bitset to the number of words below the cutoff. Mask the last word if the cutoff splits it. Reads already treat missing words as zero. Later writes extend the bitset with zero words as needed. This makes each cleanup constant time and keeps the same bits below the cutoff.

The regression test compares compilation of 6,000 and 60,000 initialized locals. It excludes source generation and requires the large compile to finish in less than 20 times the small compile, with a 25 ms floor. It also checks that the compiler rejects a function that exceeds the bytecode register limit.

## Scope ramp

Run on archMachine, Ryzen 9 5900HX, Rust 1.96.0. Baseline `cd412190` contains the original compiler and main at `745d66e9`. Both variants compile one function with `var vN = c;` declarations and a final addition. Each initializer is the integer literal for N modulo 9. Each of three fresh processes times `compile_all` after it builds the source. `perf stat -e instructions:u` counts the whole process. The release build retains debug symbols. The 120,000-local case reaches the bytecode register limit and exits before assembly.

Callgrind attributes 240,082,684 instructions (33.3%) to `Fc::below` and its callees at 6,000 locals in debug. At 60,000, the debug instruction samples put 81.7% in `Fc::below` and the two iterator loops. The release samples put 65.6% in the inlined `Fc::below` at 60,000 and 81.9% at 120,000.

The parser and compiler index large scopes by name. The compiler appends hoisted declarations and assigns registers in order. The early-use check scans at most 4,096 source bytes per name.

| Build | Locals | Before ms (min–max) | After ms (min–max) | Before instructions (min) | After instructions (min) |
| --- | ---: | ---: | ---: | ---: | ---: |
| debug | 6,000 | 322.48–817.49 | 134.16–323.56 | 718,652,034 | 481,941,916 |
| debug | 30,000 | 2,488.34–2,665.89 | 906.30–1,035.35 | 8,451,698,892 | 2,542,360,877 |
| debug | 60,000 | 8,232.73–13,686.49 | 1,793.34–2,424.12 | 28,749,922,842 | 5,111,481,211 |
| debug | 120,000 | 23,089.60–31,512.48 | 3,057.14–4,816.24 | 103,152,158,714 | 8,650,092,011 |
| release | 6,000 | 22.55–38.50 | 26.09–34.88 | 90,520,896 | 72,190,385 |
| release | 30,000 | 400.23–760.66 | 193.64–218.77 | 811,335,423 | 359,988,935 |
| release | 60,000 | 439.91–2,924.99 | 300.76–414.41 | 2,520,765,975 | 716,729,681 |
| release | 120,000 | 1,764.96–3,346.09 | 556.76–659.32 | 8,518,806,662 | 1,312,973,380 |

The 60,000-to-6,000 instruction ratio falls from 40.00× to 10.61× in debug and from 27.85× to 9.93× in release. The machine ran concurrent builds. Use the instruction counts to compare the work.

The first 20× ratio check, with a 100 ms floor, failed before the fix. It took 309 ms at 6,000 locals and 7,354 ms at 60,000 locals.

## Release cost after merging main

Both builds include main at `74f6611e` and the RegExp kind check repair from `860082e1`. The baseline uses the original `Fc::below`. The fixed build uses the shortened bitsets. Run `tools/js/instr.sh` on archMachine. Each count is the minimum of three runs of `instructions:u`.

The eval rows use `script_run --eval` on the source from `interp_load --source`. The interpreter rows run the hand-assembled programs in `interp_load`. The parse rows use `js2_parse_load`. Startup runs `script_run` on an empty file.

All 34 rows pass the +0.5% rule. The largest increases are +0.115% for glibc startup and +0.084% for jemalloc `parse fns chain 1`. The mean changes are +0.006% for glibc and -0.0003% for jemalloc.

| Row | glibc before → after | Change | jemalloc before → after | Change |
| --- | ---: | ---: | ---: | ---: |
| `eval loopfn 100000` | 24,839,877 → 24,839,913 | +0.000% | 28,178,478 → 28,177,803 | -0.002% |
| `interp loopfn 0` | 807,148 → 807,126 | -0.003% | 4,146,031 → 4,147,084 | +0.025% |
| `interp loopfn 100000` | 30,357,447 → 30,357,819 | +0.001% | 33,726,796 → 33,697,841 | -0.086% |
| `eval calls 100000` | 56,487,708 → 56,487,771 | +0.000% | 59,856,025 → 59,856,066 | +0.000% |
| `interp calls 0` | 864,727 → 864,878 | +0.017% | 4,203,351 → 4,204,664 | +0.031% |
| `interp calls 100000` | 60,715,054 → 60,715,221 | +0.000% | 64,084,123 → 64,084,984 | +0.001% |
| `eval fib 25` | 94,908,444 → 94,907,441 | -0.001% | 98,309,616 → 98,308,764 | -0.001% |
| `interp fib 0` | 867,605 → 867,773 | +0.019% | 4,204,953 → 4,206,180 | +0.029% |
| `interp fib 25` | 100,986,748 → 100,987,126 | +0.000% | 104,386,391 → 104,387,348 | +0.001% |
| `eval closures 100000` | 205,508,179 → 205,507,734 | -0.000% | 208,881,697 → 208,880,982 | -0.000% |
| `interp closures 0` | 1,001,473 → 1,001,672 | +0.020% | 4,343,267 → 4,342,877 | -0.009% |
| `interp closures 100000` | 166,217,509 → 166,217,814 | +0.000% | 169,588,711 → 169,588,510 | -0.000% |
| `eval objects 100000` | 71,555,898 → 71,555,773 | -0.000% | 74,930,854 → 74,930,914 | +0.000% |
| `interp objects 0` | 880,415 → 880,311 | -0.012% | 4,217,107 → 4,217,247 | +0.003% |
| `interp objects 100000` | 71,554,778 → 71,555,482 | +0.001% | 74,924,803 → 74,925,271 | +0.001% |
| `parse full stmts 1` | 10,630,735 → 10,627,470 | -0.031% | 14,223,605 → 14,216,571 | -0.049% |
| `parse full stmts 10` | 101,901,609 → 101,911,797 | +0.010% | 106,148,288 → 106,110,559 | -0.036% |
| `parse full chain 1` | 23,860,945 → 23,861,288 | +0.001% | 27,526,594 → 27,504,032 | -0.082% |
| `parse full chain 10` | 248,132,025 → 248,132,447 | +0.000% | 293,709,837 → 293,747,481 | +0.013% |
| `parse full bodies 1` | 11,246,357 → 11,247,808 | +0.013% | 14,867,730 → 14,868,969 | +0.008% |
| `parse full bodies 10` | 109,123,311 → 109,120,269 | -0.003% | 114,072,903 → 114,010,245 | -0.055% |
| `parse lazy stmts 1` | 10,628,422 → 10,629,096 | +0.006% | 14,218,145 → 14,219,312 | +0.008% |
| `parse lazy stmts 10` | 101,888,315 → 101,892,767 | +0.004% | 106,090,058 → 106,137,914 | +0.045% |
| `parse lazy chain 1` | 23,880,923 → 23,881,652 | +0.003% | 27,523,569 → 27,524,842 | +0.005% |
| `parse lazy chain 10` | 248,332,287 → 248,332,445 | +0.000% | 293,947,185 → 293,941,599 | -0.002% |
| `parse lazy bodies 1` | 12,272,410 → 12,273,492 | +0.009% | 15,925,467 → 15,927,466 | +0.013% |
| `parse lazy bodies 10` | 119,648,104 → 119,642,978 | -0.004% | 124,317,925 → 124,323,811 | +0.005% |
| `parse fns stmts 1` | 10,627,644 → 10,629,594 | +0.018% | 14,219,688 → 14,222,907 | +0.023% |
| `parse fns stmts 10` | 101,889,812 → 101,891,796 | +0.002% | 106,149,340 → 106,131,874 | -0.016% |
| `parse fns chain 1` | 23,880,999 → 23,881,613 | +0.003% | 27,524,150 → 27,547,185 | +0.084% |
| `parse fns chain 10` | 248,332,696 → 248,333,138 | +0.000% | 293,913,595 → 293,941,607 | +0.010% |
| `parse fns bodies 1` | 22,877,254 → 22,877,454 | +0.001% | 26,678,849 → 26,678,582 | -0.001% |
| `parse fns bodies 10` | 225,111,960 → 225,111,350 | -0.000% | 231,885,713 → 231,922,095 | +0.016% |
| `startup` | 715,757 → 716,579 | +0.115% | 4,056,894 → 4,057,265 | +0.009% |

## Tests

On Linux, `cargo test --locked -p bare-js2 --lib compile` passes 101 tests in debug and 101 in release. The large-scope ratio test passes in both builds. The six `bi_string_regexp_tests` cases pass after the merge repair. `cargo check --locked -p bare-js2` passes. Windows and the full suite are left to CI.
