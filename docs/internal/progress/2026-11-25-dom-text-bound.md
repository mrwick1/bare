# Bound the DOM text list at twice the cap

Date: 2026-11-25. Base: main 3590f1e5.

## Problem

- #1029 counts replaced text as dead. The `dom-text` cap counts live text only.
- Dead text stays in the append-only text list until a rebuild. A rebuild runs only between two tasks, and only after a collection (`due`).
- So one task that rewrites text in a loop grew the list with no bound. Two tests broke on main: `browser_platform_tests::class_list_caps` and `browser_style_tests::caps_are_uncatchable_and_checked_before_growth`.
- A page that rewrites text across many tasks with no collection also grew the list with no bound.

## Change

- `Dom::room`: one test of the list length is the fast path. The list holds the live text, so a list under the cap is under both caps. The checks over the cap are in a cold function (`text_room`).
- The list may hold the cap in live text plus the cap in dead text. Over twice the cap, the write stops with `dom-text`.
- Over the cap, `room` asks for a compaction (`Reclaim::compact`). The next task boundary walks and rebuilds, also when no wrapper died.
- A compaction walk waits until a fourth of the cap was written since the last one. So a list that stays over the cap with little dead text does not walk after each task.
- `Document::reclaim`: under `Free::Now` (a page near a cap), rebuild when a fourth of the lists is dead, also below `REBUILD_BYTES`. Each rebuild frees at least a fourth, so its cost stays amortized.
- `stats().text_bytes` can now be up to twice the cap. The style test checks that bound.

## Cost

- `tools/js/cost_matrix.sh -a -g 'attr_load|browser_load mutation|style_load (attrs|churn)|platform_load class|dataset_load write|fragment_load|scheduler_load'`, base 3590f1e5, on arjun-e16, glibc, minimum of 3 runs of `instructions:u`. 29 rows, mean +0.045%, none over the rule.
- The first version (two tests on the fast path) had `dataset_load write 10000 1` at +0.538% glibc and +0.531% jemalloc. The cold function brought it to +0.414%.

| Row | base | new | change |
|---|---|---|---|
| `dataset_load write 10000 1` | 72367964 | 72667224 | +0.414% |
| `dataset_load write 10000 50` | 80077256 | 80374469 | +0.371% |
| `dataset_load write 1000 1` | 11961886 | 11991436 | +0.247% |
| `style_load churn 1` | 33780892 | 33833599 | +0.156% |
| `style_load churn 10` | 250343418 | 250687418 | +0.137% |
| `attr_load churn 10` | 719595282 | 719040069 | -0.077% |
| `browser_load mutation 10` | 442924132 | 442574522 | -0.079% |
| `scheduler_load timers 10` | 85657728 | 85679201 | +0.025% |

- The other 21 rows are within ±0.06%.
- Hypothesis: the `dataset_load` and `style_load churn` growth is code layout. The fast path has one test, as on main. Not verified.

## Tests

- New: `crates/bare/src/browser_dom_text_bound_tests.rs`. Both tests fail on main and pass with the change.
- The two broken tests pass again. `t`: all pass. `bare-html` reclaim tests: all pass.

## Not tested

- Peak memory of a live page: not measured.
- A fragment parse (`browser_fragment.rs`) gets its text budget from the live bytes, not from the list length. So one parse can take the list past twice the cap, up to three times the cap. This is unchanged.
- test262 counts: not run. The change has no JS engine code.
