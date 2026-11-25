# Notification: a global with permission "denied"

## Problem

Microsoft Teams throws `ReferenceError: Notification is not defined` twice per load. bare has no `Notification` global. bare cannot show a desktop notification. The truthful state is permission "denied" (Notifications API, https://notifications.spec.whatwg.org/).

## Change

- New `crates/bare/src/browser_notification.rs`. `Notification` is a lazy global, as `Worker` is.
- `Notification.permission` is "denied". `Notification.maxActions` is 0. `Notification.requestPermission([cb])` resolves to "denied" and calls a function `cb` with "denied".
- `new Notification(title[, options])` returns the object. It fires a plain `error` event in a later task, as the spec says for a denied permission. `title`, `body`, `tag` and `close()` work. `onerror` works.
- The error task is a new scheduler variant, `Task::Notification`. It follows `Task::Worker`. A `Worker` task reads the worker table, so it cannot carry a notification.
- `browser.rs`: a new `Owner::Notification` keeps the listeners of the object alive. A dead object drops its entry.
- Exemplar: `browser_worker.rs` (`worker_error`, `worker_failed`, the `error` task) and `browser_broadcast.rs` (constructor and prototype).

## Tests

- New: `crates/bare/src/browser_notification_tests.rs`, 5 tests. Each asserts:
  - the constructor, `length` 1, the prototype chain, and the `TypeError` for a call without `new` and for no title.
  - `permission` "denied", `maxActions` 0, `requestPermission.length` 0.
  - `requestPermission` calls the callback with "denied", and the promise resolves to "denied".
  - `title`, `body`, `tag`, the empty defaults, `close()`, and `onerror` null.
  - the `error` event: after the current task, a plain `Event` at the object, with `onerror` first and a listener second.
- Before the change: 5 of 5 failed with `ReferenceError: Notification is not defined`.
- After the change: `t crates/bare/src/browser_notification_tests.rs`: 5 passed.
- `t` (the tests of the changed files, 5 test binaries): all pass. No failures.

## Live result

Not run. No Teams session here.

## Cost

No cost rows ran. The change adds one lazy global and one scheduler variant. Nothing runs on the hot path.

## Not done

- `show()`, `onshow`, `onclick` and the other handlers. `ServiceWorkerRegistration.showNotification`, `actions` and `icon` are absent.
- Workers do not get `Notification`. The table installs on the window only.
- A non-function `requestPermission` argument is ignored. WebIDL throws a `TypeError`.
- A function callback runs at once. The spec runs it later.
- Queued `error` tasks have no own cap. They count against the object cap, `max_wrappers`.
