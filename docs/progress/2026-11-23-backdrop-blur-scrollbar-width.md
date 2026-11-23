# backdrop-filter blur and scrollbar-width none (OFS-51)

## Change

- `backdrop-filter` and `-webkit-backdrop-filter` parse a list of filter functions. The value keeps the blur radius of all `blur()` functions together (the square root of the sum of the squares). The other functions are valid and do nothing.
- A box with a backdrop blur makes a stacking context. `Item::Backdrop` blurs the pixels under its border box before the box paints. `Canvas::blur` runs three box blurs across and three down with running sums: the area times six, for any radius.
- `scrollbar-width` takes `auto`, `thin` and `none`. With `none`, a scroll container scrolls with no bar and gives no room to one. `thin` draws as `auto`.
- Both are picture properties: `ComputedStyle` stays 880 bytes and `Property::LISTED` does not change. `Styles::differ` reports a change of `scrollbar-width` as a layout change.
- Layout reads `scrollbar-width` only for a box with `overflow` `auto` or `scroll` on an axis. The first version read it for every block and cost +0.72% on the block rows.

## Measurements

Machine: archMachine (16 threads), with other work running. Base: origin/main at `8e958c22`. New: this branch. `tools/js/cost_matrix.sh -j 4 -g 'style_load|bare-stress'`, 184 rows, 3 runs per row (minimum), glibc. No row is over +0.5%. Mean change +0.116%.

| Row | Change |
|---|---|
| style_load churn 1 | +0.124% |
| style_load setget 1 | +0.031% |
| style_load attrs 1 | +0.002% |
| style_load churn 10 | +0.171% |
| style_load setget 10 | +0.015% |
| style_load attrs 10 | +0.026% |
| style_load shorthand 1 | +0.117% |
| style_load shorthand 10 | +0.116% |
| bare-stress run overflow-auto-fits 20000 | +0.412% |
| bare-stress run overflow-auto-fits 2000 | +0.394% |
| bare-stress run overflow-many-auto 20000 | +0.370% |

## Tests

- New: `browser_style_effects_tests` (`bare`) and `tests/backdrop.rs` (`bare-paint`). They failed before the fix.
- `cargo test --locked -p bare-css -p bare-layout -p bare-paint -p bare`: pass. The paint hash tests did not change.
- Release `bare-drive`, headless, 390x300: a sticky header with `backdrop-filter: blur(12px)` over stripes, and a row with `scrollbar-width: none`. Base: `CSS.supports` false, sharp stripes, row `clientHeight` 45 with a bar. New: true, blurred stripes, 60 with no bar.

## Not tested

- `::-webkit-scrollbar { display: none }` alone still shows the bar. Pseudo-element rules never match.
- Round corners do not cut the blur. A blur radius in `em` or `rem` draws no blur.
- `el.style.backdropFilter` and `el.style.scrollbarWidth` have no camel-case accessors, like the other picture properties.
- Paint rows: the cost matrix has no paint row. js2 rows: not touched.
- Windows: the change is platform-neutral Rust. Not run on Windows.
