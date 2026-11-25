# 2026-11-25: persistent profile, step 2 (cookies and localStorage on disk)

Branch `t3/persistent-profile-storage`. Design: `docs/internal/design-persistent-profile.md`, step 2 and section 8.1.

## Change

- `bare_os::data_dir()`: `$XDG_DATA_HOME/bare` or `~/.local/share/bare` (Linux), `~/Library/Application Support/bare` (macOS),
  `%LOCALAPPDATA%\bare` (Windows). An empty `BARE_DATA_DIR` turns profiles off. `bare_os::fs::create_private_dir` (0700) and
  `open_private` (0600).
- `crates/bare/src/profile.rs`: `Profile` (directory and OS lock on `profile.lock`), `load`, and `Saver`. The saver writes
  `cookies.bin` and `local.bin` (magic, payload, CRC-32; temporary file 0600, `fsync`, rename) when a revision moved.
- Revision counters: `Jar::revision` (cookie set, insert, clear) and the `localStorage` areas (set, remove, clear). A save
  compares two numbers. It never serializes to compare.
- `Page::open_in_with(.., profile)`: the saver starts at the import, before the first request. `Page::tick` saves at most once per second and asks for a call when a change waits. The page saves at drop
  and before the next `open` reads the profile.
- Entry points: `bare-mcp open profile=NAME`, `bare-drive --profile NAME`, `bare --window URL --profile NAME`. With no profile they
  use `default`. A held `default` runs in memory and says so (receipt line or stderr). A held named profile is an error.
  A `state=` file of the open wins for that open.
- Owner answer (2026-11-25): the default applies to bare-drive too. `.cargo/config.toml` sets `BARE_DATA_DIR=""` for tests and
  `cargo run`. `tools/bench/subsystem_profile.sh`, `tools/bench/journeys.py` and `tools/journeys/triage.sh` set it too, so a
  benchmark or journey starts from an empty session.

## Tests

- New: `crates/bare/src/profile_tests.rs` (7 tests): round trip with a session cookie, lock held by a clone and refused to a second
  holder, stale lock file taken over, `default` falls back to memory and a named profile fails, one write per second and none
  without a change, a torn temporary file and a damaged file, modes 0700 and 0600.
- New: `crates/drive/tests/profile.rs` (2 tests, real `bare-drive` processes and a loopback server): a login in one process, a new
  process sends the cookie on its first request and reads `localStorage`; a held `default` runs in memory; a held named profile
  exits with code 2.
- New: `bare_os::fs` test `a_private_dir_is_owner_only`.
- `t` (5 files): `profile_tests` 7 passed, drive `profile` 2 passed, `browser_navigation_storage` 2, `control_tests` 7,
  `page_tests` 9 passed.
- By hand: drive `state` 3 and `cookies` 3, `bare-net` `cookie::tests` 23, `bare-os` `fs::` 2, `bare-mcp` `integration` 3 and the
  crate unit tests passed. `bare-mcp --test server`: 102 passed, 2 failed.
  - `open_shares_the_http_cache_between_opens_and_fresh_does_not` fails the same way on `origin/main` d4ec8a17 (`left: 2`,
    `right: 1`). It does not come from this change.
  - `a_click_under_a_cookie_banner_fails_fast_and_names_the_banner` failed once and passed on the next run (timing).
- The drive end-to-end test first failed: a loopback `Set-Cookie` reached the jar on a network thread before the page took its
  baseline revision, so the cookie looked saved. The saver now starts at the import, before the first request.

## Cost

`tools/js/cost_matrix.sh -j 1 -J -a -g 'navigation_load storage'`. BASE: `origin/main` d4ec8a17, NEW: this branch. Both release
builds of the `navigation_load` example, archMachine, minimum of 3 runs each (`instr.sh`), instructions in user mode.

| Row | glibc base | glibc new | change | jemalloc base | jemalloc new | change |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| `navigation_load storage 1` | 31,346,780 | 31,349,360 | +0.008% | 39,969,979 | 39,972,965 | +0.007% |
| `navigation_load storage 10` | 275,987,741 | 276,012,390 | +0.009% | 300,114,561 | 300,140,343 | +0.009% |

No row over the rule. The cost is one counter add per change of a cookie or an item. A page with no profile does one
`Option` check per tick.

## Not tested

- Windows and macOS: not built and not run. `tools/portable_check.sh --scan` finds no new OS code outside the OS layer.
- The GUI `bare --window URL --profile NAME` (no Wayland session in this run). It uses the same `Page::open_in_with` path as the driver.
- `bare-mcp open profile=` end to end. The code path is the drive `Session::use_profile` that the drive test runs.
- A power loss: the rename is not followed by an `fsync` of the directory.
- Two processes that start at the same moment on one profile (the OS lock decides; not raced in a test).
