# Generated content, step 2: `::before` and `::after` boxes (OFS-50)

Design: `docs/design-generated-content.md`, steps 2 and 3. Step 1: `2026-11-23-generated-content-style.md`.

## Change

- DOM (`dom/pseudo.rs`): a pseudo node is an element named `::before` or `::after` with one text node. It is not a child of its element. `pseudo_of` and `pseudo_hosts` link them, like `shadow_of`. `flat_children` gives the `::before` node first and the `::after` node last. `flat_next` and `flat_parent` follow the same order. `children`, selectors, serialization and scripts do not see the nodes.
- A pseudo node that no style shows stays hidden for the next show. Text changes make no change record and no revision bump.
- Reclaim: an element and its pseudo nodes live and die together.
- `bare_css::generated::sync` shows, updates or hides the nodes from `Styles::generated`, and gives each node its style (`Styles::style` falls back to a cold map). The host calls it in `layout_at` and after the cascade of the command-line paths.
- Hit testing: a hit on a pseudo node, or on its text, gives the element (`hit`, `element_of`). `elementFromPoint` and clicks reach the element.
- Layout and paint have no change of their own: the boxes come through the flat tree.

## Measurements

Machine: archMachine (16 threads), with other work running. Base: `feat/generated-content-style` at `2845a534` (step 1). New: this branch. `tools/js/cost_matrix.sh -j 4 -g 'style_load|cssom_load|sheets_load|tagged_load css|bare-stress run (boxes-|block-deep |inline-text|text-)|layout'`, 3 runs per row (minimum).

The first build broke the rule on 16 layout rows (+0.9% to +1.7%). Two fixes:

- `Styles::style` is `#[inline]` again, and the pseudo lookup is in a cold function. `table-fixed-layout` went from +1.70% to +0.17%.
- `flat_parent` calls the pseudo lookup through a cold function.

Rows still over +0.5% under glibc (all `bare-stress` box-tree walks, which count only the boxes):

| Row | glibc |
|---|---|
| boxes-deep-blocks 2000 / 20000 | +0.79% / +1.02% |
| boxes-list-items 20000 / 200000 | +0.81% / +0.59% |
| boxes-wide-blocks 20000 / 200000 | +0.71% / +0.73% |
| boxes-inline-blocks 20000 / 200000 | +0.62% / +0.63% |
| boxes-mixed-runs, boxes-spaces-between | +0.54% to +0.55% |

Hypothesis: the larger `FlatChildren` iterator (the `::after` node and the step after a `::before` node) costs a few instructions per child list. Rapid mode: recorded here, not fixed.

## Tests

- New: `dom::pseudo_tests` (`bare-html`), `generated_tests` (`bare-layout`), `hit_generated_tests` (`bare-paint`) and `browser_generated_tests` (`bare`). The layout and hit tests failed before the change.
- The layout test helper `block::tests::run_in` calls `generated::sync`, as the host does.
- Narrow runs pass: `bare-html` (pseudo, reclaim, flat, slot), `bare-layout` (generated, keep), `bare-paint` (hit), `bare-css` (generated, cascade), `bare` (generated, computed, point, rects, geometry: 99 pass).
- Release `bare --paint` of a page with a red `::before` text, a green `::after` text and a 40 px `inline-block` `::before` box: all three are drawn. Before, none was.

## Not tested

- `bare-mcp` in a live session: the `Browser` path is covered by `browser_generated_tests` only.
- `attr()` text does not update when only the attribute changes and no selector reads it.
- `::before` rules in shadow trees, `::marker`, counters, `url()` content and the accessibility snapshot.
- Windows: the change is platform-neutral Rust. Not run on Windows.
