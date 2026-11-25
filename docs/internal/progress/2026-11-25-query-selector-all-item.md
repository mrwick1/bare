# `querySelectorAll` result has `item`

Date: 2026-11-25. Base: main after #1033.

## Problem

- Outlook web (`owa.26033` bundle, Griffel style-element insertion) calls `head.querySelectorAll('[data-make-styles-bucket]').item(i)`.
- In Bare the result of `querySelectorAll` was a plain array without `item`. The call threw `TypeError: not a function` in a timer task (14 times in one run).
- Evidence: a probe at the "not a function" site gave source offset 2673484 of `owa.26033.m.13f4bbde.js`. The code there is `m.item(y)` with `m=e.head.querySelectorAll(...)`.
- Outlook then showed its "Something Went Wrong" page. Other errors in the same run: `WeakRef is not defined` (out of scope, the parent works on it). Which of the two causes the page: not known.

## Change

- `query_selector_all` (`browser_dom.rs`) returns `wrap_coll_s`, as `childNodes` and `children` do. The array inherits from the shared prototype that has `item`.
- `assignedNodes` and `elementsFromPoint` keep plain arrays, as the specification gives arrays for them.

## Cost

- `cost_matrix.sh -a -g 'platform_load query'`, base 2f92860b, glibc, minimum of 3 runs of `instructions:u`.
- `query 1` -0.005%, `query 10` +0.005%. Both within the rule. The jemalloc runs were skipped (glibc under the limit).

## Tests

- New: `crates/bare/src/browser_query_item_tests.rs`, `query_selector_all_has_item`. It fails on main (`not a function`) and passes here.
- `t`: all pass.

## Not tested

- test262 counts: not run. The change has no JS engine code.
- A full Outlook run with this change alone: see the next progress notes.
