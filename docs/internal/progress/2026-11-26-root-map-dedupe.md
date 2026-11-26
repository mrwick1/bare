# Root maps: each distinct map once (2026-11-26)

Slice R1 of `docs/internal/design-code-memory.md`.

## What changed

- `RootMaps::seal` (`crates/vex/src/roots.rs`) keeps each distinct bitmap of a code once and gives each safepoint a 2 B index. It also drops the spare capacity of the maps.
- The seal keeps the old form when the index form is not smaller, or when a code has more than 65,536 distinct maps.
- The compiler seals the maps of each code (`compile.rs`). `find` reads the index, then the bitmap. The binary search on `pc` does not change.
- New test file: `crates/vex/src/roots_dedup_tests.rs`.

## Probe before this change (main with C1)

One jemalloc run of the Teams journey with the scratch probe: root maps 48.3 MiB by length, 55.3 MiB by capacity, about 26.0 MiB deduplicated (estimate). Chunk room after C1: 23,024 spare slots (8.4 MiB), so C2 is now small.

## Memory on Teams

- Machine: archMachine, shared with other agents.
- Journey: `mem-journey.steps` with the saved Teams session.
- Builds: base `d998c64f` (origin/main, with C1) and this branch, `bare-drive` release. Binaries copied with `cp --reflink=never`.
- Runs: 3 per build, interleaved. All 6 journeys ended `ok: true`.

| Build | Run 1 idle / peak (MiB) | Run 2       | Run 3       |
| ----- | ----------------------- | ----------- | ----------- |
| base  | 1113 / 1117             | 1099 / 1104 | 1084 / 1088 |
| R1    | 1063 / 1068             | 1108 / 1111 | 1070 / 1075 |

- Idle PSS: mean 1099 MiB to 1080 MiB, about -19 MiB. The design estimated about 22 MiB. The spread of the runs is of the same size, so one run pair is not a proof.

## Cost rows

`interp_load CASE N` minus `interp_load CASE 0`, `tools/js/instr.sh` (minimum of 3), both allocators.

| Row      | glibc base    | glibc R1      | glibc   | jemalloc |
| -------- | ------------- | ------------- | ------- | -------- |
| loopfn   | 784,500,902   | 784,500,323   | -0.000% | +0.000%  |
| calls    | 550,501,609   | 550,501,275   | -0.000% | +0.000%  |
| fib      | 240,504,153   | 240,504,118   | -0.000% | +0.001%  |
| closures | 1,750,734,550 | 1,750,734,485 | -0.000% | -0.000%  |
| objects  | 697,789,632   | 697,789,476   | -0.000% | +0.000%  |

No row is over the rule. The rows do not measure compile time; the seal adds one hash per safepoint at compile (not measured).

## Tests

- `t`: 113 tests of the changed files pass.

## Not tested

- The Windows VM.
- test262 counts.
- The compile-time cost of the seal.
- The full cost matrix.
