# Keys for the window: modes, a command line, hints and find

Follows [2026-11-26-shell-pages.md](2026-11-26-shell-pages.md). The key reference is at the top of `crates/bare/src/shell.rs`.

## What changed
- `shell_keys.rs`: the key logic as a pure state machine, (mode, key) -> (new mode, actions). Modes: NORMAL, INSERT, COMMAND, HINT, FIND. It also holds the command parser (`parse`), the URL rule (`url_of`) and the hint labels (`hint_labels`: home row letters, all different, none the start of another).
- `shell_page.rs`: what a key does to a page: scroll the box under the middle of the window, list the clickable elements that show (`a[href]`, buttons, inputs, selects, textareas, `summary`, roles, `onclick`, `tabindex` >= 0, `contenteditable`), click through the pointer path, find text, read the focus.
- `shell_draw.rs`: the status line, the hint labels, the box of the current match, `-- INSERT --`, and the page list, drawn over the frame. The viewport does not shrink.
- `shell.rs`: `Shell` runs the key logic before the page. An eaten key and its release never reach the page. NORMAL and INSERT follow the focus of the page. `:open`, `reload` and `p` use `Page::navigate`, so the session, its history and its storage stay.
- No change in `crates/window`, `agent.rs` or `page.rs`.

## Tests
- `shell_keys_tests.rs` (16): modes, two-key commands and the 1 s timeout, the Shift of `gT`, release of eaten keys, command line editing, history, Tab, the URL rule, commands, hint labels and filtering.
- `shell_modes_tests.rs` (14, live pages from a local server): `f` + label clicks a button; a hint on a field focuses it, enters INSERT, and typed text (also `hjk`) reaches the field; Escape blurs; `i`; `F` on a link opens page 2; hints end on Escape and on the wheel; `/` selects the match (`getSelection().toString()`), `n` scrolls to the far one, `N` and wrap, "not found"; `:open`, `:t`, `:b`, `:bd`, `:q`; `x`; `y` and `p` (clipboard); Ctrl+V in the line; `j k gg G d` scroll and the page sees neither them nor their keyups; the status line pixels and the 3 s message; the hint box pixels; `o` and `O`.
- Two deliberate breaks (find does not select; hint click does nothing) made 4 tests fail. All 30 pass now.

## Manual run
`target/debug/bare --profile shell-keys-manual --window http://localhost:3773` on an offstage screen (cage, 1280x800, Wayland), once, debug build. T3 showed its pairing page (no session in the new profile), so the chat UI itself was not driven.
- `f`: labels `a` (field), `s` and `d` (the two buttons) drew at the top left of each element, and `-- HINT --` showed in the bottom line.
- `a`, `x`, `y`: the field got the focus (INSERT) and held `xy`. Escape returned to NORMAL.
- `t` then `localhost:3773/x` and Enter: a second page opened and showed the "Page not found" route of T3. The line drew as `:tabopen  localhost:3773/x` with a caret. `gt` went back to page 1, which kept its `xy`.
- The window title: `WAYLAND_DEBUG=1` shows the requests `set_title("[1/1] T3 Code (Beta) — bare")`, then `"[2/2] http://localhost:3773/x — bare"`, then `"[2/2] T3 Code (Beta) — bare"`. The `windows` list of offstage kept showing `[1/1]`; hypothesis: that list does not follow title changes (the requests were sent).
- Not done by hand: find, the clipboard keys (`y`, `p`, Ctrl+V), `:buffers`, scrolling in the real T3 chat.

## Not tested
- The things in "Manual run" that were not done by hand.
- The `Control` calls themselves (`set_clipboard`, `read_clipboard`, `close`): a `Control` cannot be read outside its crate, so the tests read what the shell asked for (`Shell::sent`, tests only).
- Hints inside a scroll container that is scrolled, and find inside one (the box uses the shift of the container; no test).
- IME, keys of other layouts, a page with an `iframe` (a hint does not enter a frame).
- Cost rows: none run. The code is in the shell and runs only for keys, the overlay and the tick loop over the pages.

## Gaps
- Find searches one line piece at a time: a match across a line break or an element edge is not found; `text-transform` is ignored.
- A hint needs a size and an element on top at the middle of its visible part.
- `buffer N` and `bdelete N` count from 1, as the window title does.
- No count prefix (`3gt`), no `:set`, no marks, no history file, no config.
