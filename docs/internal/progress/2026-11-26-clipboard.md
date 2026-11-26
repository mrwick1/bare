# Clipboard: copy, cut and paste of text and images

## What changed
- `crates/window`: the Wayland window owns the selection (`Control::set_clipboard`) and reads it (`Control::read_clipboard`, answered by one `Event::Clipboard { items }`). It uses the SCTK 0.21.1 data device; no new feature was needed. The MIME rules and the read state are in `clipboard.rs`. The pipe fds join the `poll` set only while a read or a write is open. A write sends 4096 bytes at most after `poll` says writable. A paste over 64 MiB is dropped. A compositor with no data device manager still opens a window; copy and paste do nothing.
- `shims/clipboard.js`: `DataTransfer`, `DataTransferItem`, `DataTransferItemList`, `ClipboardEvent`, made at the first read of a name. Its helper makes the events for the host.
- `browser_ui/clipboard.rs`: Ctrl+C, Ctrl+X, Ctrl+V, Ctrl+Insert, Shift+Delete and Shift+Insert as the default action of a `keydown` that was not canceled. `copy`, `cut` and `paste` are trusted. Cut deletes as `deleteByCut`. Paste inserts as `insertFromPaste`. `InputEvent.dataTransfer` is the paste store for a host.
- `page.rs`: `Event::Clipboard` arm, `take_clipboard`, `take_paste_request`; `App::event` passes both to the window.
- `browser_editing.rs` (outside the brief's file list, two small additive functions): `insert_text_as` and `delete_selected`.

## Tests
- `crates/window/src/clipboard_tests.rs` (6), `browser_datatransfer_tests.rs` (9), `browser_clipboard_keys_tests.rs` (18): pass. The old `browser_ui`, `browser_uievent`, `browser_editing`, `page`, `browser_drive`, `browser_handler` and shim tests pass.
- `crates/window/tests/clipboard_live.rs`: ran on a headless `cage` with `wtype` for the key and the keyboard. `wl-paste` read 300000 bytes of text and an `image/png` that the window offered. The window read `wl-copy` text. Run once.

## Not tested
- Hyprland itself, and the real bare window with a person typing.
- Image paste from a real screenshot tool (only bytes offered by the test).
- `navigator.clipboard` is not built (over the size limit of the brief).
- Cost rows, test262: not run (no engine change).
