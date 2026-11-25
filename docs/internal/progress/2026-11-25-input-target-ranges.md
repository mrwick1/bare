# Editing: `InputEvent.getTargetRanges` and `StaticRange`

## Problem

The Teams compose box (CKEditor 5) stayed empty after `fill`. CKEditor reads `getTargetRanges()` on `beforeinput` and makes its insert selection from the result. bare always gave `[]`. CKEditor then threw `CKEditorError: Cannot read properties of null (reading 'parent')`, and no text went in. bare had no `StaticRange` class.

## Change

- New `StaticRange` in `browser_range.rs`: `new StaticRange(init)` and the getters of `AbstractRange`. A static range does not follow the tree. Its points are not checked against the tree (DOM Standard). A DocumentType point throws `InvalidNodeTypeError`. A missing member throws `TypeError`.
- The `AbstractRange` getters accept a `Range` or a `StaticRange`.
- `Ui` has a new field `targets`. `input_ev` sets it to the points of the selection for a `beforeinput` on an editing host. `getTargetRanges()` gives one `StaticRange` for these points.
- A `beforeinput` on a text field and every `input` event give `[]`, as Chrome does.
- The collector drops static ranges through `Owner::Range`, as for live ranges. The reclaim pass holds their nodes.

## Tests

- New: `crates/bare/src/browser_input_target_ranges_tests.rs`, 3 tests. Before the change, 2 failed: `beforeinput:0` and `StaticRange is not defined`. The text field test passed before and after.
- `t` on the touched files: 84 passed, 4 ignored, 0 failed.
- Live Teams check (release `bare-drive`, saved session): the journey closed the call error dialog, filled the Daliya Joseph compose box and clicked Send. The message showed in the chat pane, the server replied 201 and the editor cleared. Before the change, the same journey failed at the editor text check.

## Cost

No cost rows ran. `Ui` grows by one `Option` of two points. The range state has one more map, empty until a page makes a static range.

## Not done

- A `deleteContentBackward` or `deleteContentForward` with a collapsed caret gives the collapsed caret. Chrome gives the range of the character that goes.
- Attr points are not refused: bare has no Attr nodes.
- Found during the live check, not fixed here:
  - Teams shows the dialog "You can't receive or make calls right now". Hypothesis: no WebRTC.
  - bare lays out that dialog at 0x0 (`position: fixed; inset: 0; margin: auto; max-width: 600px`), so it is not visible. It still traps the focus.
