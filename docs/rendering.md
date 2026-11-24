# Rendering

## HTML

`html` reads the bytes of a page. It finds the encoding (byte-order mark, `Content-Type`, `meta`, then a default), cuts the text into tokens with the state machine of the HTML standard, resolves character references from the WHATWG entity list, and builds the DOM tree with the tree builder of the standard, including the "in select", table and foreign-content modes. The parser reads the page in pieces, so a script or a style sheet starts to load before the page ends.

## CSS

- A tokenizer and a parser for CSS Syntax, selectors, at-rules (`@media`, `@supports`, `@font-face`, `@import`, `@keyframes` and others), `calc()`, custom properties with `var()`, and generated content.
- Modern colors (`color()`, `lab()`, `oklch()`, `color-mix()`) with sRGB gamut mapping, on the `color` crate.
- Selector matching goes from right to left, so most selectors fail at once.
- The cascade picks the winner of each property and computes values (`em`, `rem`, `vw`, percentages, `inherit`). A computed grid track list is stored once, even when many elements share it.
- Documents with the same sheets and the same window share one built rule set, so a warm process does not build it again.
- Transforms, transitions, and the user-agent style sheet.

## Layout

Layout walks the DOM once and writes a list of fragments: one rectangle for each box and for each piece of text on a line.

- Block and inline layout, with line breaking and the `white-space` rules.
- Floats, absolute and fixed positioning, sticky positioning, and scroll containers.
- Flexbox (CSS Flexible Box Layout 1).
- Grid (CSS Grid Layout 1): explicit and implicit tracks, `repeat()` with `auto-fill` and `auto-fit`, placement, named areas, sparse and `dense` packing.
- Tables (CSS 2.1 and CSS Tables 3), with `rowspan` and `colspan`.
- Form controls, and generated content boxes.
- A memo keeps the result of a box for the space it was laid out in. This stops the cost of stretched flex and grid items from doubling at each nesting level.

## Text

Layout cuts text into words and spaces, applies the collapsing rules, and measures each piece from the advance widths of the font. Grapheme boundaries come from ICU4X. The fonts are subsets of Noto Sans, Noto Sans Mono and Noto Serif (Regular and Bold) and a symbols face, built into the binary.

## Paint

Paint makes a flat display list from the fragments, in painting order. It draws the list into a plain `u32` pixel buffer: rectangles as whole-row fills, glyphs as coverage masks from a glyph cache (`ab_glyph`). Nothing allocates for each pixel, rectangle or glyph. The same paint order drives hit testing, so a click lands on what the user sees.

It draws backgrounds, borders, background and mask images, inline SVG (paths, shapes, strokes with dashes, `viewBox`, transforms), and `<canvas>`.

## Images

`image` decodes PNG, JPEG, WebP and GIF into `0xAARRGGBB` pixels. Each format first reads its header, with no allocation, and checks the limits. A file over a limit fails before any pixel buffer exists, so a bad image cannot use up the memory of the page. An animated image shows its first frame.

## Window

`window` shows the pixel buffer in a Wayland window with shared memory (`wl_shm`), with no copy of the picture. It asks for a new frame once after each change and at most once for each frame that the compositor shows. Pointer and keyboard input become DOM events through the same path that `bare-drive` uses. An idle window uses no CPU. Other platforms plug in behind the same small `App` interface.
