# A page list in one window (`Shell`)

## What changed
- `crates/bare/src/shell.rs`: `Shell` is the `bare_window::App` of `bare --window URL`. It owns a list of pages and shows the current one.
  - The window size goes to every page. Input and frame go to the current page. The tick goes to every page, so a page in the background keeps its timers and fetches.
  - A new page (`Shell::open`) uses `WebStorage::new_session` of the first page: the same `localStorage`, cookies, `indexedDB` and permissions, a new `sessionStorage`. It shares the network client.
  - The shell owns the one `Saver` (taken from the first page). Closing a page does not stop the profile saves. The drop of the shell writes the last changes.
  - `window.open(url)` and `target=_blank` links record `Event::Tab` through the `agent` policy. The shell reads the events after each input event and tick, opens a page for each and switches to it. The shell sets the policy `set_policy(false, None)`, so dialogs answer as before (`alert` nothing, `confirm` false, `prompt` null). The shell drops the dialog events. `agent.rs` did not change.
  - API for the key layer: `open`, `switch`, `next`, `prev`, `close` (gives `true` for the last page), `list`, `current`, `title`.
- `Control::set_title` in `crates/window` (Wayland: `xdg_toplevel.set_title`). The shell sets `[i/n] <document.title or URL> — bare`. It reads the title again only when the page, the count or the document key (`Page::doc_key`) changed.
- `crates/bare/src/page.rs`: `open_shared`, `doc_key`, `title`. `Page::url` and `Page::storage` already existed (`control.rs`).
- `crates/bare/src/main.rs`: `show_interactive` builds a `Shell` over the first page.

## Tests (`shell_pages_tests.rs`, 8, all pass)
One page ticks and draws; `localStorage` and cookies are shared and `sessionStorage` is not; a `_blank` click opens page 2 and switches; dialogs keep their old answers and `window.open` opens a page; close goes to a neighbor and the last close asks the window to close; `next` and `prev` wrap; a background timer runs; the title string.
The tests failed before the code in the sense that the module did not exist. With `set_policy` removed from `Shell::new`, the `window.open` test failed.

## Not tested
- A real window on Wayland (no screen): the title reaching the compositor, a real `bare --window` run.
- The profile `Saver` with two pages and a profile on disk (the code path is the saver of the first page, moved to the shell).
- Cost rows: no cost row was run. The page code did not change on the hot path; the shell adds one loop over the pages in `tick`.

## Gaps
- `window.open` still returns `null`; there is no `window.opener`.
- A `_blank` link now opens a page in `--window`. Before it navigated in place.
- `Shell::open(url, false)` replaces the current page with a new one (its history is gone).
- The pages have no keys yet (Part D).
