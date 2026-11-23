# Generated content, step 1: `::before` and `::after` styles (OFS-50)

Design: `docs/design-generated-content.md`, step 1. Step 2 makes the boxes.

## Change

- Selectors: `::before`, `::after`, `:before` and `:after` give `Simple::Gen` in place of `Simple::Never`. `querySelector` still matches nothing with them.
- Cascade: a rule whose last compound ends with `::before` or `::after` goes to its own index (`RuleSet::generated`). A rule in a shadow tree, or with a second pseudo-element, is dropped.
- `walk` and `restyle` compute the style of each pseudo-element of a shown element, with the element style as parent. `Styles::generated` keeps it only when `content` makes a box. A restyle that adds or removes a box reports the element as changed.
- `content`: a new picture property. Values: `normal`, `none`, the CSS-wide keywords, and a list of strings, `attr(name)`, `open-quote`, `close-quote`, `no-open-quote`, `no-close-quote`, `counter()`, `counters()` and images. The computed value is the serialized list. `content::text` makes the box text. Counters and images give no text.
- `getComputedStyle(el, '::before')` and `'::after'` read the pseudo style (`RuleSet::style_pseudo`), also for a pseudo-element with no box. `content` has a `CSSStyleDeclaration` accessor.

## Measurements

Machine: archMachine (16 threads), with other work running. Base: origin/main at `8e958c22`. New: this branch. `tools/js/cost_matrix.sh -j 4 -g 'style_load|cssom_load|sheets_load|tagged_load css|bare-stress run (boxes-|block-deep |inline-text|text-)'`: 62 rows, 3 runs per row (minimum).

- Mean change over the 62 rows under glibc: +0.082%.
- Over +0.5% under both allocators: 2 rows.

| Row | glibc | jemalloc |
|---|---|---|
| style_load shorthand 1 | +0.801% | +0.749% |
| style_load shorthand 10 | +0.912% | +0.884% |

The other 60 rows stay under +0.5%. Hypothesis: the growth comes from the `content` accessor and the new property in the shorthand path of `CSSStyleDeclaration`. A `#[inline(never)]` on `content::declaration` did not change the two rows (+0.790% and +0.897%), so it is not kept. Rapid mode: recorded here, not fixed.

## Tests

- New: `cascade::generated_tests` (2 tests), `content::tests` (2 tests) in `bare-css`, and `browser_computed_pseudo_tests` in `bare`. They failed before the change.
- Changed: the selector test expects `Simple::Gen`. `browser_style_tests::prototype_key_order`: 264 to 265, and 266 to 267. `browser_computed_tests` no longer expects `::before` to read as the element.
- `cargo test --locked -p bare-css`: pass. `cargo test --locked -p bare --lib -- browser_computed browser_style`: 115 pass.

## Not tested

- No box is drawn yet. Step 2 adds the nodes and the layout.
- `attr()` text does not update when only the attribute changes and no selector reads it.
- Windows: the change is platform-neutral Rust. Not run on Windows.
