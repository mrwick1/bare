# The cap unit

## Problem

T3 Code shows its "T3" logo as an SVG with `height: 1cap; width: auto`. bare did not know the `cap` unit, so it dropped the
declaration and the logo was 0x0. Chrome draws it at 17x10.

## Change

- `crates/css/src/values.rs`: `cap` is `0.714em`, the cap height of the bundled Noto Sans and Noto Sans Mono (`OS/2`
  `sCapHeight` 714 of 1000). It is the same approach as `ex`.
- New `crates/css/src/values_cap_tests.rs`.

## Gaps

- The ratio is fixed. A web font with another cap height gets the Noto ratio.
- `rcap` is not added.

## Checks

- The new test gave `None` before the change and passes after. `t` on the changed files passes.
- Cost rows: not run. One more string compare runs only for a unit that is not one of the earlier units.
