# Notification actions and maxActions (design-service-workers.md, owner decision 2026-11-26)

## Change

- `Notification.maxActions` is 2, as in Chrome. WhatsApp reads it to choose the service worker path for test and call
  notifications.
- `new Notification` reads `actions` first (a `sequence<NotificationAction>`: `action` and `title` are required, `icon` is
  resolved against the document). A non-empty list throws the `TypeError` of Chrome: only a service worker notification has
  actions. A bad item throws the `TypeError` of the WebIDL conversion first.
- `Notification.prototype.actions` gives a new frozen empty array.
- `NotificationAction` and `actions_member` are ready for `ServiceWorkerRegistration.showNotification` (step 5).
- The module header of `browser_worker.rs` now says that a dedicated worker runs. The header of `browser_sw_tests.rs` names
  the code that it tests.

## Tests

New file `crates/bare/src/browser_notification_actions_tests.rs`, 3 tests. Before the change: 3 failed (`maxActions` was 0;
`actions` was ignored). After: 3 passed. `browser_notification_tests.rs`: 5 passed (its `maxActions` value is now 2).

## Cost rows

`tools/js/cost_matrix.sh -g 'platform_load|handler_load|browser_load'`, base `d727b069` (origin/main) against this branch,
archMachine, 3 runs per row (minimum of `instructions:u`), glibc. 24 rows, none over +0.5%. Largest:
`browser_load canvasidle 10` +0.434% in one run, -0.03% mean over all rows. The changed code runs only in `new Notification`,
which no row calls.

## Not tested

- A live page with actions. WhatsApp gives actions only to `showNotification` (step 5).
