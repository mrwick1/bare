# Performance timeline keeps its entries and their `detail`

Date: 2026-11-25. Base: main 1ffdbcb9.

## Change

- The timeline now keeps the object that `performance.mark` or `performance.measure` returns (`crates/bare/src/browser_perf.rs`). `getEntries`, `getEntriesByType` and `getEntriesByName` return the kept objects. Before, each read made new objects with `detail: null`.
- `detail` is a structured clone of the `detail` option, as User Timing L3 says. A value that cannot be cloned throws `DataCloneError`, and no entry is kept.
- `Dom::perf_entries` roots the kept objects (`browser_perf::roots`).
- Chrome and Firefox keep the entry objects too: `performance.mark('a') === performance.getEntriesByName('a')[0]` is `true`.
- Why: Teams reads `entry.detail.taskId` from its task marks in `createUserContextWithTelemetry`. bare threw "cannot read properties of null (reading 'taskId')". The user context failed (`WaitForUserContextFailed`), and the Teams main view did not load.

## Cost

- No cost row covers `performance`.
- A read makes no new objects now. Before, each read made one object per entry.
- Each kept entry holds its object (5 own properties) and its `detail`. A live Teams run had about 900 marks at its peak. Hypothesis: a few hundred KB at most. Not measured.
- `MAX_ENTRIES` (50 000) still caps the list.

## Tests

- New: `browser_perf_detail_tests.rs` (2 tests). Both fail on main and pass with the change.
- `t`: all pass (1679 tests of the `bare` lib and the covered files).

## Not tested

- A live Teams run with this change: not yet.
- Peak memory of a Teams run: not measured.
- test262 counts: not run. The change touches no Vex code.
