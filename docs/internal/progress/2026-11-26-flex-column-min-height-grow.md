# Flex items grow into a percent min-height

## Problem

T3 Code shows its sidebar groups (Working, Settled) at the bottom of the sidebar. bare showed them at the top. The chain is a
column flex box with `min-height: 100%` inside a scroll area of a known height, a `flex: 1` item, and `margin-top: auto`.

`flex::layout` read the container's `min-height` and `max-height` with no percent base, so `100%` gave nothing. The box itself
was 656 px tall (block layout resolves the percent), but its items were laid out as if the box had no minimum (73 px in T3).

## Change

- `crates/layout/src/block.rs` passes the `min-height` and `max-height` that it resolved against the containing block to
  `crates/layout/src/flex.rs`. Flex uses them in place of its own reading.
- New `crates/bare/src/browser_flex_min_height_tests.rs`.

## Checks

- `a_percent_min_height_of_a_sized_parent_counts_too` gave `300:0,30:0,20:10` before the change and `300:0,300:0,20:280` after.
  `t` on the changed files: all pass.
- Cost rows: not run. The change removes two `resolve` calls for each flex container.
