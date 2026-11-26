# Textarea wrap and scroll, line keys, word delete, host Up and Down (2026-11-26)

## What changed
- `page.rs`: a `<textarea>` wraps each line at the content width (less the scrollbar that layout reserves). It breaks after the last space that fits, else before the letter that does not fit. `wrap` makes the visual lines (UTF-16 and byte ranges). Drawing, the selection, the caret, `caret_in` and `point_in` all use them.
- `page.rs`: the scroll of a textarea is kept per node in `Page::area_top`, with the caret that was kept in view last. A paint keeps the caret in view only when the caret moved, so the wheel is not undone. The wheel scrolls a focused textarea that can move that way, before the page.
- `page.rs` and `browser_ui.rs`: Up, Down, Home and End in a textarea move by visual line. The window finds the offset (`line_move`) and passes it with the new `ui_key_at`; `move_caret` uses it. Ctrl+Home and Ctrl+End still go to the ends of the value. Shift extends.
- `browser_ui.rs`: Ctrl+Backspace and Ctrl+Delete delete a word (`deleteWordBackward`, `deleteWordForward`) in fields, textareas and hosts, with the `word_move` boundaries.
- `browser_range.rs`: `Selection.modify` takes the granularity `line`. Up and Down in a host use it (same column, cut short on a shorter line). A line start that is also the end of the line before (paragraphs join in the text) now resolves to the start of its own line: Home at the start of a paragraph used to jump to the end of the paragraph before.
- Doc gap lists of `page.rs` and `browser_ui.rs` updated. The stale gap about arrows, Home, End and Ctrl+Left/Right in a host is removed.

## Tests (new files)
- `page_textarea_wrap_tests.rs`: wrap at spaces, a long word, a line feed, a press on visual line 2, a press right of a wrapped line, a press below the text.
- `page_textarea_scroll_tests.rs`: Up, Down, Home, End, Ctrl+Home and Ctrl+End by visual line, Shift extends, the caret stays in view after 30 lines, the wheel.
- `browser_ui_word_delete_tests.rs`: field, textarea across a line feed, host.
- `browser_ui_host_lines_tests.rs`: Up and Down in a host of three paragraphs, Shift, typing at the moved caret, and a Home and End guard.
- The wrap test failed before the change (`selectionStart` 2, wanted 7). The word delete tests failed before (they deleted one letter). The host line tests failed before, as did the host Home guard (it went to the end of the paragraph above).

## Not tested
- No pixel check of the drawn textarea; no Chrome comparison of the wrap points.
- A textarea with padding, a border or a scrollbar that is drawn: only the width math (`pad` less `client`) is used.
- The driver (`browser_drive.rs`) maps a press in a scrolled textarea with no scroll (`top` 0), and has no Up, Down, Home or End by visual line.
- Up and Down keep the x of the caret per move, not across moves (no sticky x).
- The `input` event after a word delete in a host has `inputType` `deleteContentBackward` or `deleteContentForward` (`delete_step` fixes it); `beforeinput` has the word type.
- Performance: `wrap` runs for each drawn textarea in each frame (only lines in the box are drawn). Not measured. No cost rows run.
- test262 counts: not run, no Vex change.
