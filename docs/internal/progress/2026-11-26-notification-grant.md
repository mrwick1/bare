# Notification grant in the profile and the bare-mcp tool (design-notifications.md, step 2)

## Change

- A profile keeps the permission decisions in `permissions.bin` (magic `barepm01`, `u32` count, `(str origin, str name, u8
  decision)*`, CRC-32). The `Saver` writes it when the decisions change, as it does for `cookies.bin`.
- `StorageState` has a field `permissions`. `export` and `import` carry it. The session file of `save_state` has a top-level
  `permissions` list (`origin`, `name`, `state`).
- bare-drive `Session::set_permission` and `Browser::document_origin`.
- bare-mcp has a tool `notifications` with `op=grant|deny|reset` and an optional `origin` (default: the origin of the page).

## Tests

New files, all failed before the change (they did not compile) and pass after:

- `crates/bare/src/profile_permissions_tests.rs`: 4 tests (round trip, reset, damaged file, sorted export).
- `crates/drive/src/state_permissions_tests.rs`: 2 tests (JSON round trip, bad state).
- `crates/mcp/tests/notifications_tests.rs`: 2 tests (grant, deny, reset, another origin; the grant stays in the profile after
  `close`).
- `crates/mcp/tests/server.rs::tools_list_names_the_tools` now lists `notifications`.

`open_shares_the_http_cache_between_opens_and_fresh_does_not` in `crates/mcp/tests/server.rs` fails on main `031fde4c` too (the second
open fetches `/app.js` again: 2, not 1). This change does not touch the HTTP cache. Because `t` runs all of `server.rs`, this PR
merged with the steps of `tools/ship.sh` run by hand.

## Cost rows

No row covers the profile, bare-drive or bare-mcp (design section 7). Not run.

## Not tested

- Live: the WhatsApp banner after a grant. That needs a logged-in profile; the owner runs it.
