# A fixed box inside an absolute box of the window (2026-11-25)

## Change

The window is the containing block of `fixed` boxes and of absolute boxes with no positioned block above them. `out_of_flow`
in `crates/layout/src/block.rs` walks the fragments of the root for these boxes. Then it lays them out. Their fragments go at
the end of the list, after the walk. A `fixed` box inside such an absolute box was never found. It had no fragment, so
`getBoundingClientRect()` gave 0x0 and the paint did not draw it.

Now the walk runs again over the new fragments for the `fixed` boxes inside them. It runs only on the window and only when the
page has a `fixed` box (`Cx::fixed`).

WhatsApp Web puts its picture viewer (`position: fixed; width: 100%; height: 100%`) inside `#app`, which is
`position: absolute`. Before the fix, the viewer and its Download button had no box.

The hypotheses of the brief were wrong. A fixed box in a parent of height 0 and a fixed box in a block-in-inline `span` both
had the correct box. The cause was the absolute ancestor whose containing block is the window.

## Tests

- New: `crates/bare/src/browser_rects_fixed_tests.rs`, 7 tests. 5 fail before the fix: the live ancestor chain, a fixed box in
  one absolute box, in two nested absolute boxes, in a fixed box, and the paint test. The 2 hypothesis tests pass before and after.
- `t crates/layout/src/block.rs`: 42 pass.

## Cost

`tools/js/cost_matrix.sh -g '^bare-stress'`: the 180 layout rows. Base: origin/main 9b541e89. New: this branch. archMachine,
3 runs per row (minimum), glibc. Mean change +0.001%. Over +0.5%: 0 rows. `position-fixed-deep 300`: 3,562,490 to 3,569,696
(+0.20%).

## test262

No change to Vex. No run.

## Not tested

- The live WhatsApp viewer and its Download button.
- Windows.
- A fixed box in an absolute box inside a positioned inline box. The fixed box then takes the inline box as its containing
  block (an older bug, not changed here).
