# scrollbar-width: none on overflow: scroll

## Problem

T3 Code showed classic scrollbars with arrows on its sidebar and its side panel. Chrome shows none. The scroll areas of Base UI use
`overflow: scroll` with `scrollbar-width: none` and draw their own bars.

Layout gave such a box no bars, but `bare_layout::scroll` gave every `overflow: scroll` box both bars. So paint drew them, and
`clientWidth` and `clientHeight` lost 15 px.

## Change

- `crates/layout/src/scroll.rs`: an `overflow: scroll` axis has a bar only when the box does not have `scrollbar-width: none`.
- New `crates/bare/src/browser_scrollbar_none_tests.rs`.

## Checks

- The new test gave `85,35,85,35` before the change and gives `85,35,100,50` after. `t` on the changed files: 16 passed.
- T3 Code in Chrome: the sidebar scroll area has `clientWidth` 255; bare had 240 before the change.
- Cost rows: not run. The change adds one style read for each clipping box in the scroll pass.
