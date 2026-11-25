# Error.stack text uses a line index

## Problem

- A profile of the Outlook login journey in bare (archMachine, one run, build ab6a4ae1 with symbols) gave 31.9% of the main thread CPU (about 2.7 s) to `bare_vex::error_stack::stack_get`.
- Cause: `frame_line` found the line and the column of each frame with a scan of the source from its start. Outlook's bundles are long, and some are one line. Each read of `Error.stack` scanned up to the whole bundle for each frame.

## Fix

- A `Lines` index per script unit: the start offset of each line, and a flag for an ASCII source. It is built once, on the first stack text of that unit (`OnceCell` on `Unit`).
- A lookup is one binary search over the line starts. The column of an ASCII line is the byte distance. For a source that has other characters, the column counts UTF-16 units from the start of the line only.
- `LazyCompile::line_col` gives the line and the column. The default builds the index for one call. `script.rs` keeps it on the unit.
- Memory: 4 bytes per line of a unit whose stack text was read. A unit with no stack read has no index.

## Check

- New test file `error_stack_lines_tests.rs`, 3 tests: the UTF-16 column after non-ASCII characters, the line after non-ASCII lines, and 50 stack reads in one 50 KB line (one distinct frame text). All pass.
- `t`: 169 passed.
- Benchmark: `script_run` with `stack_long_line.js` (one line of 488 KB: a 500,000-character pad, then a 10-frame recursion that reads `new Error().stack` 200 times). `tools/js/instr.sh` (minimum of 3 runs), 3 runs, archMachine, glibc. Base 04c36bdc.

| run | base instructions | new instructions |
| --- | ----------------- | ---------------- |
| 1   | 21,785,397,973    | 37,598,462       |
| 2   | 21,785,398,259    | 37,597,967       |
| 3   | 21,785,398,362    | 37,598,158       |

The result is the same on both trees (`65600`). The new tree uses 579 times fewer instructions.

- Vex micro rows, `interp_load`, `instr.sh` minimum of 3, base 04c36bdc:

| row              | base          | new           | change   |
| ---------------- | ------------- | ------------- | -------- |
| loopfn 2000000   | 521,791,064   | 521,790,894   | -0.0000% |
| calls 1000000    | 549,340,570   | 549,340,451   | -0.0000% |
| fib 25           | 92,832,577    | 92,832,401    | -0.0002% |
| closures 1000000 | 1,740,695,044 | 1,740,695,114 | +0.0000% |
| objects 1000000  | 710,641,039   | 710,642,017   | +0.0001% |

- Not tested: the live Outlook journey on this change (one more Microsoft login). Not tested on Windows. Miri does not apply: no unsafe code changed.
