# Notification permission per origin (design-notifications.md, step 1)

## Change

- `WebStorage` keeps a permission decision per origin and name (`Permission::Granted | Denied`). A new tab shares it.
- `Browser::set_permission(origin, name, decision)` sets or removes a decision. It fires a trusted `change` at each live
  `PermissionStatus` whose state moved.
- `Notification.permission` and `requestPermission()` give "granted" only for a grant in a secure context. Else they give
  "denied", with no decision too (owner decision 1). A page never sees "default".
- `navigator.permissions.query({name})` resolves a new `PermissionStatus` (`name`, `state`, `onchange`). "notifications" follows
  `Notification.permission`. Other known names are "prompt". An unknown name rejects with a `TypeError`, as in Chrome. A status
  with a listener is a GC root.
- The design document records the six owner decisions.

## Tests

New file `crates/bare/src/browser_notification_permission_tests.rs`, 7 tests, Chrome as the reference. Before the change they did not
compile (`Browser::set_permission` did not exist). After: 7 passed. `t` on the changed files: 16 passed, 1 ignored (old).

## Cost rows

`tools/js/cost_matrix.sh -g 'platform_load|handler_load'`, base `c9d46272` against this branch, archMachine, 3 runs per row
(minimum of `instructions:u`), glibc. 8 rows, none over +0.5%. Largest: `platform_load query 1` +0.372%, `query 10` +0.187%
(one more getter on `Navigator.prototype`). Mean +0.074%.

## Not tested

- The Teams journey (the default stays "denied", so no change is expected).
- No `Permissions` or `PermissionStatus` global exists. A page that reads `window.PermissionStatus` gets `undefined`.
- Another tab that shares the storage gets no `change` event on a grant.
