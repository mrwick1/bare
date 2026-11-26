# Shared stub records (2026-11-26)

Slice C1 of `docs/internal/design-code-memory.md`.

## What changed

- `Codes::push_all` (`crates/vex/src/codes.rs`) gives each stub a table entry that points to a canonical stub record. Stubs with an equal word, `nregs`, `nparams`, `flags`, `noctor` and `gen_kind` share one record.
- The own data of a stub (name, length, span) is a 24 B entry in `Codes::stubs`, in index order. `Codes::name_len` and `Codes::span` read it.
- A chunk that holds stubs is copied into an exact buffer without them. A chunk with no stub is adopted without a copy, as before.
- `Codes::set` on a stub does not retire the canonical record.
- `code_info`, `Function.prototype.toString` and the frame name of `Error.stack` read through `Codes`.
- The loader, the code cache and the verifier do not change. The code cache still keeps a full stub record per stub in its portable units.
- New test file: `crates/vex/src/stub_records_tests.rs`.

## Memory on Teams

- Machine: archMachine, shared with other agents.
- Journey: `mem-journey.steps` with the saved Teams session. It opens Teams, opens one chat, waits for the editor and idles 20 s.
- Builds: base `f79d17f9` (origin/main) and this branch, `bare-drive` release. Binaries copied with `cp --reflink=never`.
- Runs: 3 per build, interleaved. PSS of the process tree every 250 ms. "Idle" is the sample 3 s before exit.
- All 6 journeys ended `ok: true`. A first round failed at the Teams loading screen for both builds while cost-row builds ran on the machine. Both builds passed alone; the round was run again.

| Build | Run 1 idle / peak (MiB) | Run 2       | Run 3       |
| ----- | ----------------------- | ----------- | ----------- |
| base  | 1158 / 1188             | 1207 / 1211 | 1303 / 1352 |
| C1    | 1115 / 1185             | 1150 / 1197 | 1070 / 1155 |

- Idle PSS: mean 1223 MiB to 1112 MiB, about -110 MiB (-9%). The design estimated about 125 MiB.
- Peak PSS: mean 1250 MiB to 1179 MiB, about -70 MiB. The spread of base is large (1188 to 1352).

## Cost rows

`interp_load CASE N` minus `interp_load CASE 0`, `tools/js/instr.sh` (minimum of 3), both allocators.

| Row      | N         | base (glibc)  | C1 (glibc)    | glibc   | jemalloc |
| -------- | --------- | ------------- | ------------- | ------- | -------- |
| loopfn   | 3,000,000 | 784,501,250   | 784,500,586   | -0.000% | -0.000%  |
| calls    | 1,000,000 | 550,500,848   | 550,501,998   | +0.000% | +0.000%  |
| fib      | 27        | 240,503,938   | 240,503,958   | +0.000% | +0.000%  |
| closures | 1,000,000 | 1,750,724,231 | 1,750,738,216 | +0.001% | +0.002%  |
| objects  | 1,000,000 | 697,789,731   | 697,789,828   | +0.000% | +0.000%  |

No row is over the rule.

## Tests

- `t`: 14 tests of the changed files pass.
- The retire test fails when the guard in `Codes::set` is removed.
- Miri: `cargo +nightly miri test -p bare-vex --lib -- codes:: stub_records_tests::a_set`: 6 passed, 0 failed.

## Not tested

- The Windows VM.
- test262 counts.
- The full cost matrix.
