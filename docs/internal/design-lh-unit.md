# The `lh` and `rlh` units

Status: proposal. The owner reviews it before the build.

## Problem

- bare does not know `lh` and `rlh` (CSS Values 4, "font-relative lengths"). `values.rs` `length()` drops a declaration with them.
- `1lh` is the computed `line-height` of the element, in pixels. `rlh` is the one of the root element. In `line-height` itself, and
  in `font-size`, `lh` uses the parent's values.
- T3 Code (`client/assets/main-DYJdcEdq.css`, v0.1.12) uses `lh` 10 times: `min-height: 1lh`, `height: 1lh`, `min-h-[2lh]`, and the
  composer `min-height: calc(2lh + calc(var(--spacing) * 2))`. In bare the composer is 31 px high. In Chrome it is 54 px (UI
  compare, 2026-11-26). Tailwind 4 makes these classes, so more pages will have them.

## Current design

- `Length` (`crates/css/src/values.rs`): `Px`, `Em`, `Rem`, `Percent`, `Vw`, `Vh`, `Vmin`, `Vmax`, `Calc`.
- `Linear` (`crates/css/src/calc.rs`): `[f32; 8]`, one slot per unit (PX EM REM VW VH VMIN VMAX PCT). 32 bytes. `to_px(font_size,
root, viewport)` sums the slots.
- `cascade.rs` `compute()`: the font size is first (`font_size()`), because `em` needs it. The other properties go in property
  order. `resolve()` turns each length into pixels with `length_to_px` or `Linear::to_px`. `line-height` stays a `Number` (`1.5`),
  `normal`, or pixels.
- Layout (`crates/layout/src/font.rs`) turns `line-height` into pixels: `Number(n)` is `n * size`, `normal` is `ascent + descent`.
  All bare faces (Noto Sans, Sans Mono, Serif) have the same metrics: ascent `round(1.069 * size)`, descent `round(0.293 * size)`.
  So the cascade can compute `normal` exactly, with no font data.

## Options

### 1. The cascade resolves `lh` (proposed)

- Data: `Length::Lh(f32)` and `Length::Rlh(f32)`. `Linear` gets 2 slots, `LH` and `RLH`: `[f32; 10]`, 40 bytes.
- Algorithm:
  - The parser sets a flag on the rule set when a value has `lh` or `rlh` (`RuleSet::uses_lh`).
  - When the flag is off, nothing changes. `compute()` costs the same.
  - When it is on, `compute()` gets the line height in pixels right after the font size: the winner of `line-height` (or the
    parent's), with `lh` in it taken from the parent. `normal` is `round(1.069 * fs) + round(0.293 * fs)`.
  - `Cx` gets `root_line_height`, set where `root_font_size` is set.
  - `length_to_px` and `Linear::to_px` get one more input, `(lh, rlh)`.
  - `line-height` keeps its computed form (`Number`, `normal` or pixels). Only the other properties use the pixel value.
- Complexity: O(1) more per element, only on pages that use the unit. The `Linear` list grows by 8 bytes per `calc()` value.
- Expected gain: the T3 composer gets 54 px, as in Chrome. No cost on pages without `lh` (one flag check per element).

### 2. Keep `lh` until layout

- `Length::Lh` survives the cascade. Layout resolves it with `Metrics::line_height`.
- Every reader of `style.px()` in layout (block, flex, grid, table, inline, paint) must handle a unit. `calc()` keeps a third part.
- Wider change, more work in the hot layout path, and `getComputedStyle` must also resolve it. Rejected.

### 3. Rewrite `lh` to `em` when parsing

- `1lh` becomes `1.2em`. Wrong for any other `line-height`: T3's `text-sm` has `line-height: calc(1.25 / 0.875)`, so the error is
  about 19%. Rejected.

## Other engines (to verify in their source before the build)

- Chrome: `CSSToLengthConversionData` carries the line height. `normal` uses the font metrics. To verify:
  `css_to_length_conversion_data.cc`, `LineHeightSize`.
- Firefox: Stylo computes `lh` in `font_relative_length` with the font metrics provider. To verify.
- Ladybird: `Length::to_px` with `FontMetrics::line_height`. To verify.

## Build plan

1. `values.rs`: parse `lh` and `rlh`. `calc.rs`: 2 slots, and `to_px` with `(lh, rlh)`.
2. `cascade.rs`: the `uses_lh` flag, the line height after the font size, `root_line_height` in `Cx`.
3. `getComputedStyle` gives pixels (it reads the computed value, so this comes free; to verify).

## Check

- New test file `crates/css/src/values_lh_tests.rs`: parse `1lh`, `2rlh`, `calc(2lh + 8px)`.
- New test file `crates/bare/src/browser_lh_tests.rs`:
  - `line-height: 20px; height: 2lh` gives 40 px.
  - `line-height: 1.5; font-size: 10px; height: 1lh` gives 15 px.
  - `line-height: normal; font-size: 16px; height: 1lh` gives 22 px (`17 + 5`).
  - `line-height: 2lh` uses the parent's line height.
  - `rlh` uses the root's.
- T3 composer: min-height 54 px in the window (offstage), as in Chrome.
- Cost rows: the `css` rows of `cost_rows.txt` (`cost_matrix.sh -g`).

## Not known yet

- Whether `lh` in `font-size` is used on real pages. The build gives it the parent's line height, as the spec says.
- Chrome rounds the ascent and the descent per font. A page that sets a web font with other metrics gets bare's metrics.
