# Lazy feedback slots per machine (2026-11-26)

Slice 1 (design A0) of `docs/internal/design-feedback-table.md`.

## What changed

- Each machine keeps `Vec<Option<Box<Feedback>>>` in place of `Vec<Feedback>`. A code that never ran on a machine costs 8 B on it, not 128 B.
- `feedback()` in `crates/vex/src/interp.rs` gives the feedback of a frame. The fast path checks the slot, `ready` and the site count. The cold path `make_feedback()` grows the vector and makes the box.
- `Realm::collect` walks only the boxes that exist.
- New test file: `crates/vex/src/feedback_slots_tests.rs`.

## Memory on Teams

- Machine: archMachine, shared with other agents.
- Journey: `mem-journey.steps` with the saved Teams session. It opens Teams, opens one chat, waits for the editor and idles 20 s.
- Builds: base `e2bff65e` (origin/main) and this branch, `bare-drive` release. Binaries copied with `cp --reflink=never`.
- Runs: 3 per build, interleaved. PSS of the process tree every 250 ms. "Idle" is the sample 3 s before exit.
- All 6 journeys ended `ok: true`.

| Build | Run 1 idle / peak (MiB) | Run 2       | Run 3       |
| ----- | ----------------------- | ----------- | ----------- |
| base  | 1503 / 1531             | 1518 / 1592 | 1571 / 1621 |
| A0    | 1242 / 1288             | 1190 / 1281 | 1233 / 1311 |

- Idle PSS: 1503 to 1571 MiB to 1190 to 1242 MiB, about -290 MiB (-19%).
- Peak PSS: 1531 to 1621 MiB to 1281 to 1311 MiB, about -290 MiB.
- Chrome on the same journey (2026-11-26, design document): idle 1198 to 1271 MiB. bare idle is now in the same range.
- The design estimated -420 to -520 MiB from the heap profile. The profile counts jemalloc size classes; PSS does not. Hypothesis: that explains part of the gap.

## Cost rows

`interp_load CASE N` minus `interp_load CASE 0`, `tools/js/instr.sh` (minimum of 3), both allocators. The values are equal for both allocators.

| Row      | N         | base          | A0            | Change  |
| -------- | --------- | ------------- | ------------- | ------- |
| loopfn   | 3,000,000 | 781,500,763   | 784,500,612   | +0.384% |
| calls    | 1,000,000 | 548,500,982   | 550,500,331   | +0.365% |
| fib      | 27        | 240,821,506   | 240,504,182   | -0.132% |
| closures | 1,000,000 | 1,739,736,920 | 1,750,723,535 | +0.632% |
| objects  | 1,000,000 | 709,790,642   | 697,789,802   | -1.691% |

A first form without a cold path cost +11.1% on `loopfn`. The cold path `make_feedback()` removed that cost.

Over the rule (+0.5%): `closures` +0.632% under both allocators. Not measured at `codegen-units=1`. Debt for the owner's list.

## Tests

- `t`: all tests of the changed files pass.
- Miri: `cargo +nightly miri test -p bare-vex --lib -- interp:: feedback_slots_tests`: 41 passed, 1 ignored, 0 failed. Miri printed a warning with a stack in `lazy_nested_globals_refill_after_redefinition`; only the last 15 lines were kept, so its kind is not recorded (to verify).

## Not tested

- The Windows VM.
- test262 counts.
- The full cost matrix and `codegen-units=1` for `closures`.
