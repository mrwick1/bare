# `display: contents`

Design: `docs/internal/design-display-contents.md`, option 1 (approved by the owner on 2026-11-25). Teams gap census item 1 (`docs/internal/teams/gap-census.md`).

## Change

- CSS: `display: contents` parses to `Keyword::Contents`. `Styles::contents()` tells if some style has it. `getComputedStyle` gives `contents`, and `block` on the root element.
- Layout (`crates/layout/src/boxes.rs`): `Kind::Contents`. The elements of Appendix B of CSS Display 3 (`img`, `input`, `video` and the others) and foreign elements with `contents` give `Kind::None`. 3 walk functions replace `flat_*` in the walks of layout:
  - `box_first`: the first child box. A contents child is replaced by its children. An empty one is skipped.
  - `box_next`: the next sibling box. At the end of the children of a contents element it climbs to the element and goes on.
  - `raw_next` and `enter`, for the 2 hot walks (`Children`, `inline::Events`): the walk enters a contents element when it meets it. See "Cost".
  - `box_parent`: the parent box. It skips contents elements.
  - With no `display: contents` on the page, each function is the `flat_*` function behind one branch.
- Users: `Children`, `inline::Events`, the runs of `grid.rs`, `flex.rs` and `table.rs`, the flex and grid parent tests (`kind`, `block.rs`, `intrinsic.rs`), the parent metrics of `vertical-align`, and the relative offsets of lines.
- Line height (`inline.rs`, `reach_text`): text in a contents element can have a font that its parent box does not have. When the `line-height` of the parent box is `normal`, the font of the text makes the line taller. A fixed `line-height` of the parent holds. WPT `display-contents-line-height.html` and `display-contents-text-line-height.html` check both.
- `checkVisibility` (`browser_element.rs`): an element with `display: contents` gives `false` (CSSOM View: it has no box). The gate for `display: contents` rules is removed. The gate for `content-visibility` stays.
- `getBoundingClientRect` and `getClientRects` give an empty rectangle and an empty list for a contents element. Hit tests find the child boxes. Both needed no change.
- New WPT runner: `tools/wpt/run.py`. It runs script tests (its own `testharnessreport.js` keeps the results), reftests (screenshots of the test and of the reference in bare, compared byte for byte) and crash tests.

## Check

- New test files:
  - `crates/layout/src/boxes_contents_tests.rs`: 8 tests. Block, inline, flex, grid, nested and empty, `::before` and `::after`, the unusual elements, and the 2 line-height rules.
  - `crates/bare/src/browser_element_contents_tests.rs`: 3 tests. Computed `display` (and the root), no rectangles and `checkVisibility`, hit test. The root test failed before its fix (`contents`).
- WPT, `css/css-display/display-contents-*.html` at f4d3f2e, without the SVG, multicol, `::first-letter` and `::first-line` tests: 70 tests. archMachine, 1 run each, `tools/wpt/run.py`.

  | Build           | Pass | Fail | Error |
  | --------------- | ---- | ---- | ----- |
  | main (06dedc49) | 12   | 56   | 2     |
  | this PR         | 58   | 10   | 2     |
  - No test that passed on main fails now.
  - Fail: `blockify-dynamic` (0/3), `button`, `dynamic-fieldset-legend-001`, `fieldset-nested-legend`, `replaced-content`, `root-background`, `shadow-dom-1`, `shadow-host-whitespace`, `slot-attach-whitespace`, `table-003`.
  - Error, also on main: `pseudo-click-target` (the harness does not finish) and `state-change-001` (bare panics).
  - The reftests compare bare with bare, not with Chrome.

- Teams offline replay (tape3, archMachine, 1 run, 1280x800): the `data-tid` boxes against Chrome (`chrome1/at-main-area.boxes.json`, 76 boxes, matched by order of each tid).
  - All 16 `app-layout-area--*` boxes and `experience-layout` are the same as in Chrome. The `display: contents` wrapper `app-layout-area--unified-areas` is 0x0, as in Chrome. Before: the wrapper was a grid item in an extra column, and the chat pane was at x=1097.
  - 44 of 76 boxes within 2 px, 7 off, 25 missing. The missing boxes are the app bar entries, the chat header and the pre-core title bar. Their content does not show yet. That is a separate gap.
  - The screenshot is not in the repository: it comes from a logged-in account.
- `t`, and the tests of the other changed layout modules (`inline`, `boxes`, `grid`, `table`, `flex`, `block`, `generated_tests`): all pass.
- Cost: see below.

## Cost

`tools/js/cost_matrix.sh -J -g '^bare-stress'` (archMachine, 1 count per row as the script does). Base: origin/main 7a37bf38 with only the 2 new `bare-stress` cases added (`/data/bare-tmp/dc-base`). New: this branch after the merge of 7a37bf38.

- The 176 rows that were there before: mean +0.170% (glibc) and +0.149% (jemalloc). No row is over +0.5% under both allocators.
- The 4 new rows (`boxes-contents-wide`, `boxes-contents-deep`): +11% to +16% against the base. The base does other work there: it does not know `contents`, and makes a box for each element. So these rows are not a change in cost. They are the reference for later work. `boxes-contents-deep` grows linearly: 8.26M instructions at 2,000 levels, 67.8M at 20,000.

The first build was over the rule. The causes, found with `callgrind` on `inline-breaks` and `table-many-rows` (`profiling` builds):

| Step | Cause | Fix | Mean of the 176 rows (glibc) |
| --- | --- | --- | --- |
| 1 | `kind` called `box_parent`, and `box_parent` called `kind`. The compiler stopped inlining `kind` into `block::sized_with`. | The contents paths are `#[cold]` functions out of line. | +2.09% -> +1.44% |
| 2 | `box_children` stepped with `flat_next`. The `has_blocks` scan of `Tree::children` used the `FlatChildren` iterator before, which steps with less work. | `box_children` gives `FlatChildren` when the page has no contents element (`BoxChildren`). | +1.44% -> +1.08% (incl. the 4 new rows) |
| 3 | The test of the flag in `box_next` and `box_parent` at each step of `Children`, `inline::Events` and `kind`. | `kind` finds a contents parent from the `display` of the parent, which it reads anyway (`in_flex_or_grid`). `Children` and `Events` enter a contents element when they meet it (`raw_next`, `enter`), as `Events` enters an inline box. They test for contents only at the end of a list of siblings. `is_flex_or_grid` stays a leaf function. | -> +0.22%, 4 rows over |
| 4 | Step 3 found the next node before the kind. For a contents element that is a last child, `raw_next` climbed to the top, and the result was thrown away: O(n^2) for nested contents elements (`boxes-contents-deep 20000`: 35.3G instructions). | The next node is found after the kind. | +0.170%, no row over |

## Not tested

- Windows.
- The live Teams journey (only the offline replay).
- `display: contents` on SVG elements, in multicol, with `::first-letter` and `::first-line`.
- A change of `display` to or from `contents` after layout, other than the WPT `dynamic-*` tests.
