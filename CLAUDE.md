# CLAUDE.md — bare

## Main focus

Performance and resource use come first: CPU instructions, peak memory and time to ready. Weigh every change against them.

## Bug fixes and small changes

The design process below is for performance work. A bug fix or a small change skips it:

1. Write a failing test for the bug.
2. Make the shortest fix.
3. Run `t`. It runs the tests that cover the changed files (`t --for <file>` lists them). Without `t`, run `cargo test --locked -p <crate> <module>::`.
4. Commit, write the PR description, then run `tools/ship.sh`. It checks, pushes and merges. There is no CI (owner, 2026-11-23).
## How to improve a part

Do not patch hot spots one by one. A 10% gain on one benchmark row is not the goal. Look for a change in design that removes a whole kind of work.

1. Measure first. Profile the whole browser on the public journeys and the load-test ramps. Use a build with symbols. Split the cost by subsystem: network, parsing, style, layout, paint and JS. Work on the subsystem with the largest share.
2. Study the design, not the symptom. Write down the current data structures and algorithms, and their time and memory complexity. Read how real engines solve the same problem (Chrome, Firefox and Servo, WebKit, Ladybird) and what research gives.
3. Compare options. List at least two other designs. For each, give the data structures, the algorithm, the complexity, the memory layout and the expected gain on our benchmark. Prefer a design that makes the work unneeded over one that makes the work cheaper.
4. Write a design document per subsystem in `docs/`. The owner reviews it before any code is built.
5. Build the chosen design. Run the same benchmark again on the whole browser, not only single rows. Record the numbers in a new file in `docs/progress/` (see its README).

A limit or a budget that protects against a slow algorithm is a sign of a design problem. Fix the algorithm, then remove or lower the limit.

## Benchmark rules

- Measure a frozen commit. Main stays open while we measure.
- Rebase a change on the newest main and measure it again there before merge.
- Trust instruction counts under load. Trust wall time only on an idle machine.
- Run each case at least 3 times. Give the machine, the inputs, the run count and the spread.
- Mark an unverified cause as "hypothesis".

## Cost gate (owner, 2026-11-21)

- A PR runs only the rows of the code it changes: the js2 micro rows (`interp_load`) for a js2 change, and the `cost_rows.txt` rows of the touched subsystem (`cost_matrix.sh -g`). The rule per row stays: +0.5% under both allocators, or within +0.5% at `codegen-units=1`.
- The full matrix runs on main after about every 6 merged PRs. A row that grew there is bisected to its PR, and a fix PR follows at once.
- Rapid mode (owner, 2026-11-21): a row over the rule does not block the merge. The PR records the row in its progress file and in `docs/progress/cost-debt.md`. A fix PR follows the same day.

## Merge flow (owner, 2026-11-21, rapid mode)

- Every PR merges through `tools/ship.sh`, also for unsafe, GC and trait changes. The script is the gate: `cargo check` of the changed crates and `t`. Miri does not run: a change to an unsafe module of js2 runs `cargo +nightly miri test -p bare-js2 --lib -- <module>::` by hand. The owner's session reviews after merge and opens a fix PR when needed.
- `tools/release.sh` tags the next version. The release workflow builds, runs the offstage gate and publishes. `tools/ship.sh --release` merges and releases in one command.
- Stacked PRs are allowed. Retarget a stacked PR to main when its base merges.
- Each PR merges origin/main right before its final push and solves its own conflicts.
- A PR does not change `tools/fixtures/js/test262-js2-baseline.txt`. It gives its test262 counts in its progress file. The owner's session writes the baseline on main together with the full cost matrix.
- A PR puts new tests in a new test file per topic (`<module>_<topic>_tests.rs`). It does not append to a shared test file.
- A PR does not change `docs/progress/cost-debt.md`. It gives its over-rule rows in its progress file. The owner's session copies them into the debt list.
- On a conflict in a generated file (`crates/js2/src/prelude_facts.rs`), take main's version and generate the file again. Do not merge it by hand.
