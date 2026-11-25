# Remove the DOM count caps (2026-11-25)

Owner order (2026-11-25): remove every cap that stops a real page. This PR adds the audit of all caps (`../caps-audit.md`) and removes the count caps of `BrowserLimits`. `max_events` went in #1102.

## What changed

- `BrowserLimits` keeps `max_tree_depth`, `max_reports`, `max_task_aborts`, `max_child_pages` and `max_child_depth`.
- Removed: `max_nodes`, `max_text_bytes`, `max_attr_slots`, `max_names`, `max_wrappers`, `max_listeners`, `max_results`, `max_selector_len` and `max_string_bytes`, with their checks at about 80 call sites.
- Removed: the name-bytes cap of `NodeList` (`MAX_NAME_BYTES`).
- Removed: the count fields of `FragmentLimits`. It keeps `max_depth`.
- `max_child_pages` goes from 64 to 1000. It still stops a page that makes frames without end.
- Text bytes: a check no longer fails at 64 MiB of live text. Past `TEXT_COMPACT` (64 MiB), the document asks for a text rebuild between tasks. The next rebuild waits until the list grows by a quarter of `TEXT_COMPACT` or by half the list. So a large live text is not copied at each write. A `dom-text` limit stays only when one task makes more than 4 GiB of text, because a `Span` holds `u32` offsets (follow-up F7).
- The XHR object cap now has its own name, `xhr-objects`. It is follow-up F2.
- A string between JS and the document is no longer measured. The extra UTF-16 decode pass of each string argument is gone.

## Tests

- New `browser_count_caps_tests.rs` passes the old defaults:
  - 2^20 + 1 elements with 2 attributes each;
  - 2^16 + 1 query results;
  - 2^16 + 1 wrappers with a listener each;
  - 2^16 + 1 names;
  - a 16 MiB + 1 string.
- `browser_dom_text_bound_tests.rs` is rewritten. 5 text nodes of 14 MiB each are allowed. 50 tasks that each rewrite 2 MB keep the text list under `TEXT_COMPACT` + `TEXT_COMPACT`/4 + 4 MiB.
- Each test of a removed cap is removed. Where the cap test also checked other behavior, the test now passes the old value.
- `t` passes on these files:
  - attr 22, css_ns 5, dataset 10, dom_text_bound 2, domexception 3, form 17, fragment 35, handler 20;
  - link 9, namespace 16, nodelist 22, platform 22, range 27, reclaim 7, scheduler 37, select 9;
  - shadow 53, browser_tests 58, count_caps 5, survive 5, child 35, xhr 11, events_gc 1;
  - html fragment 13, html5lib 5, context 17, pseudo 2.
- `browser_style_tests::prototype_key_order` fails (left 267, right 265). It fails in the same way on the base `1a5690b6`. This PR does not cause it.

## Cost rows

- Machine: archMachine, 16 threads. The machine was under load (load average 38 to 46), so the rows use instruction counts only.
- Base: `1a5690b6`. New: this branch before the merge of origin/main.
- Command: `tools/js/cost_matrix.sh -a -g 'examples/(attr|browser|dataset|fragment|handler|nodelist|platform|range|style|scheduler|xhr|selection|cssom|observer|web_apis)_load'`.
- Each row ran 3 times, and the script keeps the minimum. Only glibc ran, because no row went over +0.5%.
- Result: 93 rows. The mean change is -0.587%.
  - Largest growth: `scheduler_load timers 10` +0.256%.
  - Largest drop: `dataset_load write 10000 1` -3.804%.
  - Other drops: `platform_load query 1` -2.448%, `browser_load svg 1` -2.117%, `style_load churn 10` -1.252%.
- No row is over the rule.
- `web_apis_load segmenter 1` and `10` fail to run with a script error on both trees. This failure exists before this PR.
- The rows were not measured again after the merge of origin/main.

## Not removed

- `max_tree_depth`, `max_child_depth`, the recursion `MAX_DEPTH` constants and `MAX_CUT_DEPTH`: they guard the native stack.
- `max_reports`, `max_task_aborts`, `EvalLimits`, `HeapLimits`, `PageLimits`, `SessionLimits` and `PromiseLimits`: they are budgets or diagnostics. The owner decides.
- `dom-selector-complexity`: it protects a backtracking matcher (follow-up F1).
- The count caps of the other modules (timers, observers, fetch, streams, WebSocket, workers, geometry, computed style, Vex source size): these are follow-ups F2 to F6 in `../caps-audit.md`.

## Not tested

- A real site (WhatsApp, Teams, Outlook) that went past a removed cap. No journey ran.
- Peak memory of a page with more than 2^20 nodes, apart from the unit test.
- Windows.
- test262: Vex is not changed.
