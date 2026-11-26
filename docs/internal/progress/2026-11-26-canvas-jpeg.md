# Canvas 2D: JPEG for `toDataURL` and `toBlob`

Design: `docs/internal/design-canvas-2d.md`, section 6, "Encoders".

## Problem

`toDataURL('image/jpeg')` and `toBlob(cb, 'image/jpeg', q)` gave PNG (step 1, PR #1129). WhatsApp Web sends a photo as the
`image/jpeg` Blob of a canvas. A page that checks `blob.type` or the file size can fail on PNG, and a PNG photo is about 10 times
larger.

## Change

- Owner decision (2026-11-26): accept the IJG terms of `jpeg-encoder` 0.7.1.
- New `crates/paint/src/jpeg.rs`: `encode(w, h, premul, quality)`. An `ImageBuffer` reads the premultiplied canvas pixels row by
  row into YCbCr. The color is on black, as in Chrome (half-transparent red reads 129,0,0 in Chrome). It makes no RGB copy of the
  bitmap. A blank canvas encodes black without a bitmap.
- `crates/bare/src/browser_canvas.rs`, `encode`: `image/jpeg` (any case) gives JPEG. Quality: a number in 0..1 gives
  `round(q * 100)`, at least 1. Else 0.92, as in Chrome. Every other type gives PNG.
- Notices: `crates/paint/LICENSE-IJG` (the IJG terms and the sentence that clause 2 asks for), a row in `docs/third-party.md`, a
  row in `docs/internal/open-source.md`, and a sentence in the README license section.

## Tests

- New `crates/bare/src/browser_canvas_jpeg_tests.rs` (3 tests). They failed first (the result was PNG).
  - A JPEG data URL decodes to the same size. The pixels are within 8 of Chrome's values on black.
  - Quality: the default, 0.92, a number out of range and a string give the same bytes. 0.1 is smaller than the default, and 1 is
    larger. Chrome gives the same 4 results.
  - The WhatsApp case: a PNG Blob, `createImageBitmap`, a half-size `drawImage`, `toBlob(cb, 'image/jpeg', 0.8)`, then decode
    again. The Blob type is `image/jpeg`, the size is 4x3, and each channel is within 12 of the drawn color. Chrome gives
    237,164,0 and 36,59,100 for 240,160,0 and 32,64,96.
- Changed in `browser_canvas_images_tests.rs`: the `toBlob` type test expects `image/jpeg`. The PNG data URL test uses
  `image/webp` (no encoder in bare, so PNG). The exact PNG round trip now uses PNG.
- `t` on the touched files: all pass.

## Live check

`bare-drive` (release, headless, no profile) on a local page: `fetch` of a 1600 by 1200 JPEG photo (31,524 B, two flat colors),
`createImageBitmap`, `drawImage` at half size, `toBlob(cb, 'image/jpeg', 0.8)`, then `createImageBitmap` of the result. The page
title was `ok image/jpeg 8756 800x600 240,160,1,31,64,95`: a JPEG Blob of 8,756 B, the same size, and the drawn colors 240,160,0 and
32,64,96 within 1. The whole script took 91 ms. The longest tick was 46.9 ms. One run.

## Cost

Machine: archMachine (16 threads, load average about 3). Instructions are `tools/js/instr.sh` (the minimum of 3 runs, glibc).

The `browser_load` canvas rows do not encode, so the encoder is not in them. Against the step 1 numbers (same canvas code):

| Row | Step 1 | This PR | Change |
| --- | ---: | ---: | ---: |
| `browser_load canvas 1` | 71,131,536 | 71,097,255 | -0.05% |
| `browser_load canvas 10` | 394,631,423 | 394,460,525 | -0.04% |
| `browser_load canvasidle 1` | 20,357,480 | 20,358,621 | +0.01% |
| `browser_load canvasidle 10` | 294,686,278 | 293,114,956 | -0.53% |

No row is over +0.5%.

Encoder alone (release, a 1600 by 1200 gradient, the best of 5 runs, one process):

| Encoder | Time | Bytes |
| --- | ---: | ---: |
| JPEG, quality 92 | 27.4 ms | 217,417 |
| JPEG, quality 80 | 16.5 ms | 62,318 |
| PNG (`encode_rgba`) | 9.6 ms | 2,476,884 |

The crate: no dependencies, 3,974 lines, +211,944 B of stripped binary, 1.9 to 2.2 s clean release build (3 runs).

## Not done

- The encoder runs on the page thread. Hypothesis: a large photo blocks the page for tens of milliseconds. A later step can encode
  on the picture worker threads.
- Chrome encodes with libjpeg-turbo, always 4:2:0. The crate uses 4:4:4 at quality 90 and higher. The bytes differ from Chrome.
- WebP encode is not built (PNG).
- The live check ran headless, not in a window, and with a picture of flat colors, not a camera photo.
