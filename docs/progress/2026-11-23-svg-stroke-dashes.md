# SVG stroke dashes and the dash properties in `style` (OFS-49)

## Change

- The SVG painter draws `stroke-dasharray` and `stroke-dashoffset`. It cuts each polyline of a stroke into open dashes, and each dash gets its caps. A negative length, a zero sum, a bad value or `none` gives a solid stroke (SVG 2).
- The painter reads all stroke properties from the `style` attribute first, then from the attributes, on the shape or the nearest element above it. Before, it read only the attributes.
- `CSSStyleDeclaration` has accessors for `stroke-dasharray` and `stroke-dashoffset` (`strokeDasharray`, `strokeDashoffset`). `el.style.strokeDasharray = "..."` writes the `style` attribute. Before, the write made a plain JS property and the attribute did not change. React sets SVG styles this way.
- The two names are `Kind::Whole` declarations with no longhand mask. `bare_css` does not compute them. A small check accepts `none` or lengths of 0 or more (`stroke-dasharray`), or one length (`stroke-dashoffset`).

## Measurements

Machine: archMachine (16 threads), with other work running. Base: origin/main at `dd4171d7`. New: this branch. `tools/js/cost_matrix.sh -j 2 -g 'browser_load svg|style_load'`, 3 runs per row (minimum), glibc.

| Row | Change |
|---|---|
| browser_load svg 1 / 10 | +0.000% / +0.001% |
| browser_load svgparse 1 / 10 | -0.002% / -0.004% |
| style_load attrs 1 / 10 | +0.196% / +0.130% |
| style_load churn 1 / 10 | +0.136% / +0.195% |
| style_load setget 1 / 10 | +0.177% / +0.144% |
| style_load shorthand 1 / 10 | +0.123% / +0.113% |

No row is over +0.5%. The `style_load` growth comes from the four new prototype accessors (hypothesis: the accessor setup in `browser_style::ensure`).

## Tests

- New: `svg::tests::a_dash_array_cuts_the_stroke`, `svg::tests::a_progress_ring_draws_only_its_arc` (`bare-paint`), `browser_style_svg_tests` (`bare`). They failed before the fix.
- `browser_style_tests::prototype_key_order`: the prototype counts grow by 4 (260 to 264, and 262 to 266 for the computed prototype).
- `cargo test --locked -p bare --lib` and `cargo test --locked -p bare-paint`: pass.
- Release `bare-mcp`: a page with a script-set `style.strokeDasharray` and a page with the attributes both draw a quarter ring. Before, both drew a full ring and the `style` attribute stayed empty.

## Not tested

- `getComputedStyle(el).strokeDasharray` stays `""` (Chrome gives the value in `px`). The cascade does not compute these properties.
- Dash lengths in `%` or `em`: they read as no dash.
- Dashes from a style sheet rule: not read, like `stroke-width`.
- Windows: the change is platform-neutral Rust. Not run on Windows.
