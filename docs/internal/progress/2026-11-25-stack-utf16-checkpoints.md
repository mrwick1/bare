# Stack frame columns use UTF-16 checkpoints

## Problem

- After #1074, a profile of the Outlook login (archMachine, one run, main 40d66e3d with symbols) still gave 13.2% of the main thread to `error_stack::Lines::get`.
- Cause: for a source that is not all ASCII, `Lines::get` counted the UTF-16 units from the start of the line to the frame. Outlook's bundles have non-ASCII characters and are one long line, so each frame scanned up to the whole bundle.

## Fix

- `Lines` keeps, for a non-ASCII source only, a checkpoint about every 1 KiB: a byte offset on a character start and the UTF-16 units before it.
- A column is `units(off) - units(line start)`. Each `units` is one binary search and a count of at most about 1 KiB.
- Memory: 8 bytes per KiB of a non-ASCII unit whose stack text was read (40 KB for a 5 MB bundle). An ASCII source has no checkpoints.

## Check

- New test in `error_stack_lines_tests.rs`: 40 lines of 1- to 4-byte characters that cross many checkpoints; the line and column at every 97th character equal a direct count. The 4 tests of the file pass.
- `script_run`, `tools/js/instr.sh` (minimum of 3), 3 runs, archMachine, glibc, base 40d66e3d:

| script | base | new |
| --- | --- | --- |
| `stack_long_line` with one `é` (488 KB, one line, 200 stack reads of 10 frames) | 11,046,890,243 – 11,046,893,310 | 61,282,439 – 61,282,831 |
| `stack_long_line`, all ASCII | 37,598,226 – 37,598,806 | 38,122,269 – 38,123,035 |

  The results are the same on both trees. The ASCII script is +1.4% (+0.52 M instructions). Hypothesis: code layout; the ASCII path does the same work as before.
- `interp_load` rows (minimum of 3): all within +0.0005%.
- Not tested: Windows; the live Outlook journey on this change.
