# Service worker notifications (design-service-workers.md, step 5, design S2)

## Change

- `ServiceWorkerRegistration.showNotification(title, options)` in a page and in a service worker. It reads the same
  `NotificationOptions` as `new Notification` (one shared reader in `browser_notification.rs`). Then it checks, as Chrome does:
  an active worker, the grant, `silent` with `vibrate`, `renotify` with no tag. `actions` keeps the first 2 (`maxActions`).
- The notification lives in the session (`sw_hub.rs`, `PNote`), not in a realm. A live notification of the origin with the same
  tag leaves with no event ("replaced"). A page notification and a service worker notification share one id counter of the
  session, so the ids of an agent never collide.
- `getNotifications({tag})` gives a new `Notification` object for each live notification of the registration. Its `actions`
  getter gives frozen `{action, title, icon}` objects. Its `close()` closes the session notification with no event.
- The user (an agent) clicks or closes it: `Browser::click_notification_action(id, action)` and `Browser::close_notification`.
  The worker gets a trusted `notificationclick` (with `action`) or `notificationclose`. `NotificationEvent` is a global of the
  worker. The worker starts when it does not run; the page of the agent hosts it.
- `WindowClient.focus()` works while a `notificationclick` is in flight (until its `waitUntil` promises settle). The page comes
  out of the background at its next turn. Outside a click it rejects with `InvalidAccessError`, as before.
- `new Notification` in a service worker throws a `TypeError`, as in Chrome.
- Agent view: `Browser::notifications`, `take_notification_events` and `notification_event_count` include the service worker
  notifications of the document origin. `NotificationInfo` has `actions`.
- bare-mcp `notifications`: the list shows each button as `action="name":"title"`. `op click id=N action=name` clicks a button.
  `bare-drive` `act_on_notification` takes the action.
- An unregistered registration closes its notifications ("closed" in the log).

## Cost when no registration exists

`Browser::notifications` reads the hub notes only when the session has a registration (`has_regs`, one length check). The event
count and the drain read the hub log: one `Rc` clone and an empty check. A page notification takes its id from the hub: one
`Rc` clone and one add.

## Tests

- `crates/bare/src/browser_sw_notification_tests.rs` (new, 7 tests): the two rejections with the messages of Chrome, a shown
  notification read back (`data`, frozen `actions`, 2 of 3 buttons) and seen by the agent, tag replace with
  `getNotifications({tag})` and `close()`, a click on a button starts a stopped worker that focuses the hidden page, `focus()`
  outside a click rejects and a user close fires `notificationclose`, `new Notification` in a worker throws and the worker shows
  one, shared ids.
- `crates/mcp/tests/notifications_sw_tests.rs` (new, 1 test): the list shows the button, a click on an unknown button is an
  error, a click on `reply` reaches the worker and the page.
- Before the change these tests do not build: `showNotification`, `click_notification_action` and `NotificationInfo::actions` do
  not exist. After: 7 + 1 passed. The notification tests (`browser_notification_*_tests`, 27), the service worker tests (16) and
  the bare-mcp notification tests (5) pass.

## Cost rows

`tools/js/cost_matrix.sh -g 'platform_load|handler_load|browser_load'`, base `413675f5` (origin/main) against this branch
(merged with it), archMachine, glibc and jemalloc. 24 rows, none over +0.5% under both allocators. One row over under one
allocator only: `browser_load canvasidle 1` glibc +0.532%, jemalloc -0.923% (noise candidate). Mean change glibc -0.022%.

## Known deviations

- The session does not keep the notifications in the profile. Chrome keeps them across a restart.
- `clients.openWindow()` still rejects with `InvalidAccessError`, also in a `notificationclick`.
- The window interaction of a click has no time limit (Chrome: 10 s).
- `image` and `vibrate` are not kept.

## Not tested

- WhatsApp live (the owner tests it).
- Two tabs of one session that use one registration.
- Windows.
