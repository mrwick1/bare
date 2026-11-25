# Case-insensitive classes fold with a table

## Problem

- In the Outlook login profile (archMachine, one run, build ab6a4ae1), regexp compile took 161 samples of the main thread. The 2nd-largest regexp symbol was `core::unicode::conversions::lookup`.
- Cause: a class with `i` and without `u` called `canon` (a `to_uppercase`) for each member of each range. It then scanned all 65,536 bits of the result one bit at a time. The bench below costs 6.3 M instructions per pattern on average. The regexp cache keys on the source and the flags, so each distinct pattern pays once.

## Fix

- `CANON_MOVES`: a static sorted table of the code units that `canon` changes (1,169 pairs, 4,676 bytes). File `regexp_canon_table.rs`, generated from `canon`.
- `canon_class` sets each range in the bitmap a word at a time. It moves only the table members inside the range (a binary search for the first), then reads the runs a word at a time. The cost depends on the ranges and the moves in them, not on the range sizes.
- `MAX_CLASS_WORK` is unchanged. It still counts the range sizes. Hypothesis: it can now go for this path; not changed here.

## Check

- New test file `regexp_canon_class_tests.rs`:
  - `canon_class` equals a brute-force `canon` of each member, for 9 sets (the full range, a negated set, Latin, Greek and Cyrillic, the special folds, word edges) and 29 sets that start at different bit offsets;
  - `CANON_MOVES` equals `canon` for all 65,536 code units. This test fails after a Rust update that changes the Unicode data.
- `t`: the regexp test files pass.
- `script_run` with `tools/js/instr.sh` (minimum of 3), 3 runs each, archMachine, glibc, base 7299e011:

| script                                                                         | base                          | new                     |
| ------------------------------------------------------------------------------ | ----------------------------- | ----------------------- |
| 300 x 3 distinct `/i` patterns with classes (`[a-z0-9_.-]`, `[\s\S]`, `[^<>]`) | 5,629,425,911 – 5,629,426,094 | 58,277,797 – 58,278,190 |
| one `/^[a-z]+$/i.test`                                                         | 1,450,063 – 1,450,402         | 807,192 – 807,823       |
| empty script                                                                   | 724,351 – 724,986             | 724,329 – 725,083       |

The results are the same on both trees. A runtime-built table was tried first: it cost about 16 M instructions on the first `/i` class, so a page with one pattern got slower. The static table has no first-use cost.

- `script_run` binary: +5,360 bytes.
- `interp_load` rows (minimum of 3): all within +0.00004%.
- test262 (`test262_js2.sh -g 'RegExp|String/prototype/(match|replace|search|split)'`, pinned e4fbf577): 1,368 passed, 118 failed, 797 skipped on both trees. The per-test results are the same.
- Not tested: Windows; the live Outlook journey on this change.
