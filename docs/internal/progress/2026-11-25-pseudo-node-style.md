# The style of a ::before or ::after node after a change of the tree (2026-11-25)

## Problem

A live bare-mcp session on WhatsApp Web panicked in `act wait`:
`crates/layout/src/boxes.rs:132: layout only asks for the style of a box`.

## Cause

A `::before` or `::after` box is a node (`dom/pseudo.rs`). `generated::sync` makes the node before a layout. The node gets an
index at the end of the arena. `Styles::style` read the style of the node from `pseudo_nodes` only when the index was past the end
of `of_node`.

After a change of the tree, `RuleSet::restyle` grows `of_node` to `doc.len()`. A new full cascade also sizes `of_node` to `doc.len()`.
Both now cover the old pseudo nodes with `NONE`. So `Styles::style` gave `None` for a pseudo node that a style shows.

Two results:

- Layout gave the pseudo node `Kind::None`: the `::before` or `::after` box was not drawn after the first change of the tree.
- `Pass::culled` (`browser_rects.rs`) reads the rectangles of an inline box that layout did not keep. It calls `Tree::style` for
  each element child that is not atomic, float or out of flow. A pseudo node with `Kind::None` went there, and `Tree::style` panicked.
  `getBoundingClientRect`, `getClientRects` and the agent's element boxes use `Pass::culled`.

Two more paths kept wrong pseudo styles:

- `Browser::refresh` with no box to lay out again (a color change on hover) did not run `generated::sync`. The pseudo nodes kept
  the style numbers of the old styles.
- `Browser::restyle_frame` gives the frame new styles with the same layout. The new styles had no `pseudo_nodes` at all.

## Change

- `Styles::style`: an entry of `NONE` reads `pseudo_nodes` too, when the styles have generated boxes. A page with no `::before`
  or `::after` box pays one more `is_empty` test for a node with no style.
- `Browser::restyle_frame` and the no-layout return of `Browser::refresh` run `generated::sync` with the new styles.

## Tests

- New: `browser_rects_pseudo_tests.rs`. A culled inline with `::before`, a read of its rectangle, a new element, a read again.
  Before the fix: the panic of `boxes.rs`. After the fix: the same width twice.
- New: `page_pseudo_style_tests.rs`. The window frame of a page with `::before` and `::after`, after four tree changes and a
  hover that changes a color, against a frame made from nothing. Before the fix: 167 pixels differ in the first frame. After the
  fix: no difference.
- A random search (not kept): 199 seeds of 60 random tree changes each, a frame after each change, against a frame made from
  nothing. No panic in the frame path. It found frame differences. Their cause is not known (see "Not fixed").
- `t`: 5 groups, 146 tests, all pass (`browser_rects_pseudo_tests`, `page_pseudo_style_tests`, `browser_ui_tests`, `bare-css cascade::`,
  `browser_editing_tests`).

## Cost

`tools/js/cost_matrix.sh -g 'style_load|bare-stress'`: the `style_load` rows and the 176 layout rows of `bare-stress`, 188 rows,
3 runs each, on archMachine. Base: origin/main `0478d8c3`. Mean change under glibc +0.010%. No row over +0.5% under one
allocator, so jemalloc did not run. No row over the rule.

## test262

No change to Vex. No run.

## Not fixed

- The random search found refreshed frames that differ from a frame made from nothing: seed 70 step 10
  and seed 143 from step 47 (the height of `#root`). The page has `display: contents`, tables, `:empty`, `:first-child` and
  `.x > .y` rules. Not studied.
- A hover that makes a new `::before` box with the same style number of its element does not lay out again: the box shows at the
  next layout. Hypothesis, not tested.
- The live WhatsApp page was not run again (rule of the task: no logged-in WhatsApp session).
