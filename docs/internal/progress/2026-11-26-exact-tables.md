# Exact tables in a compiled code (2026-11-26)

Slice C3 of `docs/internal/design-code-memory.md`, first part.

## What changed

- `Code::shrink` (`crates/vex/src/bytecode.rs`) drops the spare capacity of the tables of a code. The compiler calls it when a code is complete (`compile.rs`).
- The tables stay `Vec`s. The second part of C3 (boxed slices, a smaller record) is not built.
- New test file: `crates/vex/src/code_shrink_tests.rs`. It fails without the call in the compiler.

## Memory on Teams

- Machine: archMachine, shared with other agents.
- Journey: `mem-journey60.steps` with the saved Teams session. Snapshot 30 s after the chat editor shows.
- Builds: base `413675f5` (origin/main, with C1 and R1) and this branch, `bare-drive` release.
- Metric: glibc `malloc_stats` "in use" bytes, from gdb at the snapshot. In earlier runs it varied by 0.1% between runs. PSS varied by about 60 MiB.

| Build | Run 1 in use (bytes) | Run 2       | PSS run 1 / 2 (MiB) |
| ----- | -------------------- | ----------- | ------------------- |
| base  | 656,956,624          | 657,064,896 | 1166 / 1210         |
| C3    | 645,139,696          | 644,591,328 | 1161 / 1158         |

- Malloc in use: -11.6 MiB. The probe on main with C1 gave about 12 MiB of slack in `words` and `atoms` alone.
- A PSS A/B with 3 runs each (`mem-journey.steps`) gave base 1050 / 1066 / 1057 MiB and C3 1063 / 1039 / 1179 MiB. The gain is below the noise of PSS.

## Cost rows

`interp_load CASE N` minus `interp_load CASE 0`, `tools/js/instr.sh` (minimum of 3), both allocators. All 5 rows (loopfn, calls, fib, closures, objects) are within ±0.001% under both allocators. No row is over the rule. The rows do not measure compile time.

## Tests

- `t`: 118 tests of the changed files pass.

## Not tested

- The compile-time cost of the copies that `shrink_to_fit` makes.
- The Windows VM.
- test262 counts.
- The full cost matrix.
