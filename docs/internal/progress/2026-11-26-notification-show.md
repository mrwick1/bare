# Show, close and tag of a notification (design-notifications.md, step 3)

## Change

- `new Notification` reads all of `NotificationOptions` that Chrome keeps: `badge`, `body`, `data` (structured clone), `dir`,
  `icon`, `lang`, `renotify`, `requireInteraction`, `silent`, `tag`, `timestamp`. `renotify` with no tag and `silent` with
  `vibrate` throw a `TypeError`.
- With the grant, a task shows it: the page keeps a list of live notifications and an event log (`shown`, `replaced`,
  `closed`, `permission-requested`). No cap on the log (owner decision 2). Then `show` fires. Without the grant, `error`
  fires as before.
- `close()` takes it out of the list, and a task fires `close`. A new notification with the same tag replaces the old one with no
  `close` event.
- A shown notification is a GC root. `onshow`, `onclick`, `onclose` and `onerror` are handler attributes.
- `Browser::notifications()` and `Browser::take_notification_events()` give the list and drain the log.

## Tests

New file `crates/bare/src/browser_notification_show_tests.rs`, 8 tests. Before the change they did not compile (no
`take_notification_events`). After: 8 passed. `browser_notification_tests.rs` (5) and `browser_notification_permission_tests.rs` (7)
pass.

## Cost rows

`tools/js/cost_matrix.sh -g 'platform_load|handler_load'`, base `244ff1cd` against this branch, archMachine, 3 runs per row (minimum of
`instructions:u`), glibc. 8 rows, none over +0.5%. Largest: `platform_load media 1` +0.102%. Mean +0.032%.

## Not tested

- `close()` before the `show` task: bare fires `close` and never shows it. Chrome behaviour is to verify.
- `actions`, `image` and `vibrate` are not kept, so their getters do not exist.
