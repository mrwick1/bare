# A gate throws a NotSupportedError, and the page goes on (2026-11-25)

## Change

Owner decision 2026-11-25: "Throw, page goes on". Design: `docs/internal/design-unsupported-throws.md`.

- `Host::call` of `Dom` (`browser.rs`) is the one place where a host function returns to Vex. An `Abort::Unsupported` that is not a
  heap fault (`Ops::take_gate`, `eng_new.rs`) becomes a thrown `NotSupportedError` DOMException (code 9). Its message is the
  feature text. The console gets the error `bare: not supported: <feature>`.
- The script can catch the error. An uncaught one ends its task only. The page is not stopped.
- `drive` diagnose reads the console line as "missing feature" and gives the verdict "bare gap: ...". This also works when the page
  catches the error.
- `Fault::Limit`, `Fault::Heap` and a gate that Rust code reaches with no script (loader, driver, render) do not change.
- Vex does not change.

## Tests

- New: `browser_unsupported_throws_tests.rs`, 3 tests (caught gate, uncaught gate in a timer, console line). All 3 fail before the
  change and pass after it.
- 24 test files expected the old fatal fault. Rule: in an expected string, `unsupported:<what>` becomes
  `throw:NotSupportedError:<what>`. Tests that read the fault with `matches!` use `is_gate` of the new test file. Tests that expected a
  stopped pump or an uncatchable gate now expect that the page goes on and that `catch` sees the error.
- `browser_idle_tests.rs`: 2 tests failed on main before this change. 136106cf made `display: contents` give `false` but did not
  change the tests. They now expect `false` (and `true` for a rule with a selector that does not parse).
- `t` on the 55 test files that name a gate, plus the 13 files that mention a gate, `browser_tests.rs` and `eng_new_tests.rs`: all pass
  except the failures below, which also fail on main without this change:
  - `browser_cors_tests::a_simple_cross_origin_get_sends_origin_and_reads_when_it_is_allowed`
  - `crates/drive/tests/cache.rs`: 3 tests (a script is downloaded 3 times, not once). Hypothesis: the disk of /data was full
    during the run, so the HTTP cache could not write.

## Cost

Machine: archMachine, under load from other builds. Base dd32d3d1 (origin/main), new = this branch. Rows (`cost_matrix.sh -g`):
`browser_load|scheduler_load|attr_load|dataset_load|nodelist_load|console_load|handler_load|fragment_load`, 57 rows, 3 runs each
(minimum of `instructions:u`).

| Row                   | glibc   | jemalloc | codegen-units=1 |
| --------------------- | ------- | -------- | --------------- |
| attr_load react 1     | +0.677% | +0.649%  | within +0.5%    |
| attr_load react 10    | +0.710% | +0.707%  | within +0.5%    |
| console_load flood 10 | +1.251% | +1.107%  | within +0.5%    |
| console_load patch 10 | +0.743% | +0.656%  | within +0.5%    |

- At `codegen-units=1` the 4 rows are within +0.5% (mean glibc +0.102%). So no row breaks the rule. Hypothesis: the extra match in
  `Host::call` changes the code layout of the call path; the real work per call is one compare of the error variant.
- Over under one allocator only: `browser_load svg 1` (+0.504% / +0.475%), `console_load flood 1` (+0.671% / +0.396%).
- `console_load retain 1` and `10` fail to run on both trees (`FAIL: freed: 499 != ?`). This is not from this change.
- Mean of the 55 rows that ran: glibc +0.302%.

## test262

No change to Vex. No run.

## Not tested

- A real page (WhatsApp Web) with a gate. No journey ran.
- `drive` diagnose output end to end. The console line matches the words that `diag.rs` looks for ("not supported").
- Windows.
