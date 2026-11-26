# Notifications for an agent: list, wait, click, close (design-notifications.md, step 4)

## Change

- bare: `Browser::click_notification(id)` fires a trusted, cancelable `click` and logs "clicked". The notification stays, as in
  Chrome. `Browser::close_notification(id)` is a close by the user: it leaves the list, logs "closed" and fires `close` at once.
  `Browser::notification_event_count()` reads the log size and does not drain it.
- bare-drive: the step `expect notification [title=|~"x"] [body=|~"x"]` waits for a live notification that matches.
  `expect notification new` waits for an event that no agent read. `Session::notifications`, `live_notifications` and
  `act_on_notification`.
- bare-mcp `notifications`: `op=list` (default) gives the unread events (and reads them), then the live notifications.
  `op=wait timeout_ms=N` (default 30000) runs `wait notification new`, then lists. `op=click id=N`, `op=close id=N`. An `act`
  receipt adds `notification: #N "title" "body"` for each new live notification.

## Tests

New files, all failed before (they did not compile) and pass after:

- `crates/bare/src/browser_notification_click_tests.rs`: 4 tests.
- `crates/drive/src/steps_notification_tests.rs`: 1 test.
- `crates/mcp/tests/notifications_agent_tests.rs`: 2 tests. A page makes a notification from a click and from a timer. `act` names
  it, `list` drains the events, `wait` returns the timer one, `click` runs `onclick` (it sets the title and calls `window.focus()`),
  `close` removes it, a bad id and an idle `wait` are errors. The step `wait notification title~"carol"` passes; a step for
  another title fails.

## Cost rows

This step adds driver calls only: no code runs on a page that does not use them. No row covers bare-drive or bare-mcp. Not run.

## Not tested

- Live WhatsApp. That needs a logged-in profile; the owner runs it.
- A navigation makes a new page: the events that no agent read before it are lost, as the page notifications end in Chrome.
