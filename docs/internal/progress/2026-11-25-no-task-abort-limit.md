# A budget abort never ends a page

## Change
- `BrowserLimits::max_task_aborts` is removed. Its default was 5. The sixth abort by a script budget stopped the page for good (`Survive::Exhausted`, `Fatal::Aborted`). No timer, promise or input ran after it.
- `Browser::survive` now aborts each task that a script budget ends, with no count. The console error per abort stays. It no longer says "N of M aborts used".
- `Survive::Exhausted`, `Fatal::Aborted` and the abort counter of the DOM are removed. Nothing else made them.
- Reason: a long-lived app (WhatsApp Web, Teams) runs for hours and can reach 6 aborts. Chrome never stops a page for this.
- A chain of runaway timers in a bounded run now ends at the session budgets of time, tasks and rounds. A live page has no such budgets (`page.rs`), so the chain runs on, one aborted task at a time, as in Chrome.

## Tests
- New: `browser_task_aborts_tests.rs::many_aborted_tasks_never_stop_the_page`. Ten timers run out of steps, then a later timer sets a flag. Before the fix the page stopped at the sixth abort. After it the page settles with 10 aborts and the flag set.
- Changed: `browser_survive_tests.rs::an_endless_chain_of_runaway_tasks_ends_at_the_session_task_budget` (was `..._stops_the_page_with_a_reason`). The chain now ends at the session task budget (20), not at a fatal stop.
- `t` (9 tests of the changed files), `browser_survive_tests.rs` (5 pass, 1 ignored as before) and `browser_task_aborts_tests.rs` (1 pass).

## Cost rows
- `tools/js/cost_matrix.sh -g 'browser_load|scheduler_load|console_load'`, BASE origin/main 0478d8c3, NEW this branch, archMachine, 3 runs per row (minimum).
- 23 rows measured. Mean change under glibc: +0.026%. No row is over +0.5%.
- `console_load retain 1` and `retain 10` fail to run on both trees (`FAIL: freed: 499 != ?`). This failure exists on main and is not from this change.

## Not tested
- A live page on a real long-lived app (WhatsApp Web, Teams).
- The Windows VM.
