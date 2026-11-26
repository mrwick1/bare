# Compact pending entry for a lazy function (2026-11-26)

Slice C4a of `docs/internal/design-code-memory.md` (section C4, first part).

## What changed

- `Wait` in `crates/vex/src/script.rs` is 24 B. It keeps `ci`, `unit`, the function id, the scope chain, `self_name` and a `done` flag.
- It keeps no copy of the function name. The lazy compile takes the name from the stub's atom in `Codes` (`Codes::name_len`, from C1) and makes the `Child` again.
- Before: `Wait { ci: usize, unit: usize, child: Option<Child> }`, about 64 B, plus a `Box<str>` name on the heap for most stubs.
- A failed compile still marks the entry as done, so the next call fails again, as before.
- New test file: `crates/vex/src/lazy_entry_tests.rs` (the entry size; names with one-byte and two-byte text, an inferred name and a named function expression).

## Memory on Teams

- Machine: archMachine, shared with other agents.
- Journey: `mem-journey60.steps` with the saved Teams session. Snapshot 30 s after the chat editor shows.
- Builds: base `a050a709` (origin/main, with C1, R1 and C3) and this branch, `bare-drive` release.
- Metric: glibc `malloc_stats` "in use" bytes, from gdb at the snapshot.

| Build | Run 1 in use (bytes) | Run 2       | PSS run 1 / 2 (MiB) |
| ----- | -------------------- | ----------- | ------------------- |
| base  | 646,649,472          | 643,524,320 | 1168 / 1144         |
| C4a   | 620,261,936          | 624,914,896 | 1171 / 1131         |

- Malloc in use: -21.5 MiB. The design estimated about 16 MiB for this part (the entry and the names).

## Cost rows

`interp_load CASE N` minus `interp_load CASE 0`, `tools/js/instr.sh` (minimum of 3), both allocators. All 5 rows (loopfn, calls, fib, closures, objects) are within ±0.001% under both allocators. No row is over the rule.

## Tests

- `t`: 175 tests of the changed files pass.

## Not tested

- The Windows VM.
- test262 counts.
- The full cost matrix.
