# Canvas 2D, step 1: pixels for a photo upload

Design: `docs/internal/design-canvas-2d.md`, section 6 (option B with the lazy record).

## Problem

WhatsApp Web draws a chosen photo on a canvas and encodes it again. In bare, `getImageData` gave transparent black, `toDataURL` gave
a fixed 1x1 PNG, `drawImage` and `putImageData` did nothing, and `toBlob`, `createImageBitmap` and `ImageBitmap` did not exist.

## Change

- New `crates/paint/src/raster2d.rs`: our own raster for canvas 2D.
  - `Record`: the draws of one canvas and a premultiplied base (`Option<Arc<Vec<u32>>>`). A read plays the draws. A canvas that
    nobody reads allocates no pixels.
  - A scanline rasterizer with an active edge list, 16 sub-scanlines per row, exact horizontal coverage in a difference row. Nonzero
    and evenodd. Coverage is truncated to 8 bits, as Skia does (half coverage is 127).
  - Image sources sampled through the inverse transform: bilinear, or nearest when smoothing is off. A canvas source is shared by
    `Arc`. A later draw on it copies first (`Arc::make_mut`).
  - A clear or an opaque fill of the whole canvas drops the draws before it. The draws never hold more bytes than 64 KiB or the
    bitmap (`4 * w * h`), whichever is larger. Past that, the next draw plays them.
  - Chrome's size limits: 32,767 px per side, 268,435,456 px in total. A larger canvas stays blank.
- `crates/paint/src/png.rs`: `encode_rgba` (RGBA, `Compression::Fast`, `Sub` filter).
- `crates/bare/src/browser_canvas.rs`:
  - `fillRect`, `strokeRect` and `clearRect` record anti-aliased fills.
  - `drawImage` with 3, 5 and 9 arguments, from an `<img>`, a canvas or an `ImageBitmap`. The source rectangle is clipped to the
    picture, and the destination in the same proportion.
  - `putImageData` with a dirty rectangle. `getImageData` reads the pixels.
  - `toDataURL` and `toBlob`: PNG. `image/jpeg` gives PNG until the JPEG PR. `toBlob` calls back in a new task
    (`browser_scheduler::queue_call`).
  - `createImageBitmap` from an `<img>`, a canvas, an `ImageBitmap`, an `ImageData` or a `Blob`, with a crop rectangle,
    `resizeWidth`, `resizeHeight`, `resizeQuality: "pixelated"` and `imageOrientation: "flipY"`. `ImageBitmap` with `width`,
    `height` and `close`. The collector drops its pixels (`Owner::Bitmap`).
- `crates/bare/examples/browser_load.rs`: the cases `canvas` and `canvasidle`. `tools/js/cost_rows.txt`: their 4 rows.

## Tests

- New: `crates/paint/src/raster2d_tests.rs` (8 tests) and `crates/bare/src/browser_canvas_images_tests.rs` (11 tests). The
  expected values come from Chrome headless shell 1254 for the same scripts: anti-aliased edges, premultiply loss, bilinear and
  nearest scaling, crop, resize, the error names and messages.
- One test is the WhatsApp case: a picture Blob becomes an `ImageBitmap`, is drawn at half size, is encoded with
  `toBlob(cb, 'image/jpeg', 0.8)`, and decodes back to the same size and pixels.
- Changed: `browser_canvas_raster_tests.rs`. The edges of a fractional rectangle are now anti-aliased, as in Chrome.
- `t` on the touched files: all pass.

## Cost

Machine: archMachine. Instructions are `tools/js/instr.sh` (the minimum of 3 runs, glibc). Wall time and peak RSS are one run.

The `browser_load` rows that exist on main (`tools/js/cost_matrix.sh -g browser_load`, base = the merge base `0bc6fb36`): 12 rows,
mean +0.048%, the largest `lookup 10` at +0.272%. No row is over +0.5%.

New rows (no base):

| Row | Instructions | Wall | Peak RSS |
| --- | ---: | ---: | ---: |
| `browser_load canvas 1` | 71,131,536 | 8.9 ms | 16.0 MiB |
| `browser_load canvas 10` | 394,631,423 | 28.3 ms | 18.6 MiB |
| `browser_load canvasidle 1` | 20,357,480 | 1.9 ms | 13.6 MiB |
| `browser_load canvasidle 10` | 294,686,278 | 22.7 ms | 16.7 MiB |

- `canvasidle 1` does no raster. Its cost is the script and the record.
- `canvasidle 10` records 10,000 draws. They pass the memory bound (the 1 MiB of the bitmap), so one raster runs. This is the
  design: the draws never hold more memory than the pixels.
- A first build sampled 16 sub-scanlines in every row and made new row buffers for each draw: `canvas 10` was 755,710,840 and
  `canvasidle 10` 529,862,301. A row with only vertical edges now takes one sample line for each band (exact coverage), and the
  buffers are kept for the whole raster.
- Hypothesis: most of the record cost (about 20,000 instructions for each `fillRect`) is the JS state on the context object: each
  call reads `fillStyle`, `globalAlpha` and the canvas size back from JS and parses the color again. Step 3 (accessors with the state
  in Rust) removes it.

## Not done

- Paths, clip, transforms, composite modes, text, gradients and patterns (step 2 of the plan).
- JPEG (the next PR).
- `createImageBitmap(blob)` decodes on the page thread. `toBlob` encodes on the page thread.
- `premultiplyAlpha` and `colorSpaceConversion` of `createImageBitmap` are ignored.
- One `ImageData` still holds at most 64 MiB. Not changed here.
- No live check in a browser window in this PR. The JPEG PR runs it.
