# Canvas size and pixels (WhatsApp bring-up step 4)

Record: `docs/internal/design-whatsapp-bringup.md`.

## What changed

- `HTMLCanvasElement.width` and `height` reflect their attributes, 300 and 150 by default (`browser_canvas.rs`). Before,
  they were `undefined`, and `c.width = 228` made a plain property.
- A `<canvas>` gets `width:Wpx; aspect-ratio:W/H` as a presentational hint from its attributes (`crates/css/src/hints.rs`).
  Before, its box was 0x0. An author `width` keeps the ratio.
- `fillRect`, `strokeRect` and `clearRect` draw into a bitmap per canvas (`Dom.canvases`, `Raster`). Colors are CSS colors
  with `globalAlpha`, source-over. A pixel is in a rectangle when its center is. A new size clears the bitmap. The context
  now has the default `fillStyle`, `strokeStyle`, `lineWidth` and `globalAlpha`.
- Paint shows the bitmap like the picture of an `<img>` (`crates/paint/src/display.rs`). The document holds a lazy copy
  (`Pixels::lazy`): the pixels are copied once, when paint first reads them. A draw call after that gives a new lazy copy.
  The many calls of one frame (WhatsApp makes about 3 per QR module) cost no copy each.

## Design notes

- The bitmap is not kept in the document, because `Pixels` is shared and read-only. The `Raster` keeps a `Vec<u32>` behind a
  `Mutex`. The lazy copy reads it on the paint side.
- A bitmap larger than 64 MiB (the `ImageData` limit) draws nothing.
- Transforms, paths, text and images still draw nothing (deviation, see the module header).

## Live effect

`docs/internal/whatsapp/whatsapp-qr.steps`, archMachine, 1 run, with PRs 1 to 3 on main. Before: the QR canvas was 0x0,
and blank after the size fix. After: the step passes in 15 s and the screenshot shows the QR code. `zbarimg` decodes it to a
`https://wa.me/settings/li...` link.

## Tests

- `crates/bare/src/browser_canvas_size_tests.rs` (3 tests: reflection, box size, a script-made canvas in a flex box).
- `crates/bare/src/browser_canvas_raster_tests.rs` (4 tests: fill, stroke and clear pixels; alpha and bad styles; a new size
  clears; paint shows the bitmap).
- `t`: all covering tests pass.

## Cost rows

`cost_matrix.sh -g 'bare-stress|style_load|browser_load|web_apis_load|geometry_load|sticky_load'`, base 0d79be9f (main),
archMachine, glibc. 209 rows measured, none over +0.5%, mean +0.065%. `web_apis_load segmenter 1` and `segmenter 10` fail
on the base and on this branch the same way (known, see `2026-11-25-subtle-ctr-hmac.md`).

## Not tested

- Transforms on rectangles (they are ignored).
- `getImageData` and `toDataURL` still give empty pixels.
- Windows.
