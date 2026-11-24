# A live page on Vex stopped on the dom-events cap

Date: 2026-11-24. Base: main e796e970.

## Problem

`live_budget::a_live_page_runs_more_tasks_than_one_page_load_may` failed once during a ship run on a busy machine. A `MessageChannel` loop stopped after 20,740 tasks with `Limit(Host("dom-events"))`. On an idle machine the test passed (4 debug runs, 1 release run).

## Cause

A live turn of `Page::tick` ends after its slice (8 ms). The loader collects in `rounds` when `res.rounds` is a multiple of `collect_every` (32), or when `Browser::collect_due` asks. `res.rounds` starts at 0 in each turn. On a busy machine a turn runs fewer than 32 rounds, so the count never reaches 32. `collect_due` asked only on heap growth and on DOM arena growth. Events are small in the Vex heap, so the 16,384 live events of `max_events` filled before the heap asked.

## Fix

`Browser::collect_due` also asks when the live events pass half of `max_events`. `ui_call` (`browser_ui.rs`) had the same test inline; it now calls `collect_due` only.

## Tests

- New: `page_live_events_tests::a_live_page_with_one_round_per_turn_frees_its_events`. It sets the slice to 50 us, so each turn runs about one round. Before the fix: stopped on `dom-events` after 18,573 tasks. After the fix: passes (40,000 tasks).
- `live_budget::a_live_page_runs_more_tasks_than_one_page_load_may`: the `#[ignore]` is removed.
- `t` on the change: 1646 passed, 102 ignored, 1 failed: `browser_storage_tests::chrome_cases`. It fails the same way on a build without this change (6 of 45 cases differ: Storage named properties, `Object.freeze` and `Object.seal`). It is a Vex gap that no `t` run had covered before. It is now ignored with that reason.

## Cost

The check is one `RefCell` borrow and one compare per round. No cost rows were run.

## Not tested

- Peak memory of a live page with the change.
- Windows.
