# A page in the background (design-notifications.md, step 5)

## Change

- `Browser::set_background(on)` puts the page in the background or brings it back, as a switch of tabs in Chrome. Going:
  `document.hasFocus()` is false and a trusted `blur` fires at the window. Then `visibilityState` is "hidden", `hidden` is
  true and a trusted `visibilitychange` fires at the document. Coming back: `visibilitychange`, then `focus`. The order is
  the one of the Page Lifecycle API. A closed page (`hide`) stays hidden.
- While the page is in the background, a timer (`setTimeout`, `setInterval`) is due at the next whole second of the scheduler
  clock. The timers in the queue move when the page goes to the background. Messages and network tasks are not aligned
  (owner decision 4).
- A click on a notification that the page does not cancel brings the page back (the standard: the user agent focuses the window).
- bare-drive: `Session::set_background`. bare-mcp: `notifications {"op":"background","on":true|false}` (default true).

## Tests

New files, Chrome and the Page Lifecycle API as the reference. Before the change they did not compile (no `set_background`, no
op `background`). After: all pass.

- `crates/bare/src/browser_background_tests.rs`: 6 tests (event order and values, a closed page, timer alignment, an interval
  once a second, a message not aligned, a click that brings the page back).
- `crates/mcp/tests/notifications_background_tests.rs`: 1 test (a page that notifies only when hidden).

`t` on the changed files: all passed. Also `browser_scheduler_tests.rs` (37 passed, 4 ignored), `browser_notification_click_tests.rs`,
`notifications_agent_tests.rs` and `notifications_tests.rs`: all passed.

## Cost rows

`tools/js/cost_matrix.sh -a -g 'platform_load|handler_load'`, base `c21ed9e7` (origin/main) against this branch, archMachine,
3 runs per row (minimum of `instructions:u`), glibc. 8 rows, none over +0.5%. Largest: `handler_load 1` +0.085%. Mean +0.023%.

## Not tested

- Live WhatsApp: whether it shows a notification only when hidden is still a hypothesis.
- A new document starts in front: the background state does not carry across a navigation or reload.
- `blur` and `focus` are plain `Event`s, not `FocusEvent`. The focused element gets no `blur`.
- `requestAnimationFrame` still runs on a hidden page. Chrome stops it.
- The Chrome limit of one wake per minute for chained timers after 5 minutes is not built.
- Queued timers keep their aligned time when the page comes back (at most 1 s late).
