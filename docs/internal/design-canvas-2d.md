# Design: a real canvas 2D context

Status: design for review. No code is built from it yet.

## Problem

The 2D context of `<canvas>` is mostly a stub. Many apps draw on a canvas and read the pixels back: image resize before upload,
thumbnails, charts, QR codes, captcha, fingerprint scripts. WhatsApp Web draws the chosen photo on a canvas and encodes it again. In bare,
a 6,388-byte PNG became a 68-byte blank image, and the photo preview was blank.

Recommendation: **option C. Each canvas records a short command list. A raster backend runs the list only when something reads the
pixels: paint of a visible frame, `getImageData`, `toDataURL`, `toBlob`, or a `drawImage` that uses the canvas as its source. The
backend is the `tiny-skia` crate (option A) behind a small trait.** A canvas that nobody reads costs only its commands.

## 1. Current state

Files (all verified on main at `ebb46712`):

- `crates/bare/src/browser_canvas.rs` (466 lines): the context, `ImageData`, `toDataURL`.
- `crates/bare/src/browser_canvas_tests.rs`, `browser_canvas_raster_tests.rs`, `browser_canvas_size_tests.rs`: the tests.
- `crates/bare/src/browser.rs:813`: `Dom::canvases`, a `HashMap<NodeId, Raster>`. `browser_reclaim.rs:198` drops the entry of a
  removed node.
- `crates/paint/src/canvas.rs`: the page painter (`Canvas`, a `Vec<u32>` of `0xAARRGGBB`).
- `crates/paint/src/display.rs:1054`: paint shows the bitmap of a `<canvas>` like the picture of an `<img>`.
- `crates/html/src/dom.rs:557`: `Bitmap = (u32, u32, Pixels)`. `Pixels` is an `Arc` with a `OnceLock` and an optional maker: the
  pixels are made at the first read (`Pixels::lazy`).
- `crates/bare/src/images.rs`: download and decode of pictures on worker threads. `BUDGET` is 64 MiB of decoded pixels per page.
- `crates/image`: decoders only (PNG `png`, JPEG `zune-jpeg`, WebP `image-webp`, GIF `gif`) into `0xAARRGGBB`, not premultiplied.
- `crates/paint/src/png.rs`: a PNG writer for page captures (RGB, no alpha, `Compression::Fast`).

What works:

- `getContext("2d")` returns one object per element. It is a plain object, not a `CanvasRenderingContext2D` interface object.
- `fillRect`, `strokeRect` and `clearRect` (commit `54a6c5e2`) draw into `Raster`: `0xAARRGGBB`, not premultiplied, in a
  `Arc<Mutex<Vec<u32>>>`. A solid `fillStyle` or `strokeStyle` (a CSS color) with `globalAlpha`. No anti-aliasing: a pixel is in when
  its center is. Source-over blending is in `f32` per channel.
- After each draw, `draw` gives the document a new `Pixels::lazy` copy if paint read the old one. Paint copies the whole bitmap once per
  frame that has new draws.
- `measureText` returns `{ width }` from the layout font metrics (`canvas_font` maps the CSS `font` value to a bare face).
- `ImageData` (constructor, `createImageData`, `getImageData`) is complete as an interface. One `ImageData` holds at most 64 MiB.

What is a stub:

- `getImageData` returns transparent black. It does not read `Raster`.
- `toDataURL` always returns the same 1x1 transparent PNG (114 characters). `toBlob` does not exist.
- Paths, `drawImage`, `putImageData`, text, transforms, `save`/`restore`, `clip`, gradients and patterns are no-ops.
- `isPointInPath` and `isPointInStroke` return `false`.
- `createImageBitmap`, `ImageBitmap` and `OffscreenCanvas` do not exist.
- The drawing state (`fillStyle`, `lineWidth`, `globalAlpha`) is a set of JS data properties. Each `fillRect` reads them back with
  `get_str` and parses the color string again. No setter checks or normalizes a value (Chrome returns `#ff0000` for `"red"`).

Painter limits that matter here (`crates/paint/src/canvas.rs`):

- `blend` assumes an opaque destination. A canvas bitmap is transparent at the start.
- `fill_poly` checks every edge on every row and samples 8 sub-lines per row: `O(H * E)`, with no active edge list.
- `stroke` computes the distance from each pixel in the box to each segment: `O(area * segments)`.
- `draw_image` takes the nearest pixel, with no transform and no smoothing.
- No gradients, patterns, clip masks or composite modes.

## 2. What apps need, in order

1. `drawImage` from `<img>` and from another canvas, with scale (5 and 9 arguments). `ImageBitmap` next. A video frame later (bare has
   no video).
2. `fillRect`, `clearRect`, `strokeRect`, anti-aliased, with transforms.
3. Paths: `beginPath`, `moveTo`, `lineTo`, `arc`, `arcTo`, `ellipse`, `rect`, `roundRect`, `quadraticCurveTo`, `bezierCurveTo`,
   `closePath`, `fill` (nonzero and evenodd), `stroke` (width, cap, join, miter, dash), `clip`, `Path2D`.
4. `save`, `restore`, `translate`, `scale`, `rotate`, `transform`, `setTransform`, `getTransform`, `resetTransform`.
5. `globalAlpha` and `globalCompositeOperation`: `source-over` first, then `copy`, `destination-in`, `destination-out`,
   `source-in`, `source-atop` (the ones that image editors and masks use).
6. `fillText`, `strokeText`, `measureText` (full `TextMetrics`), `font`, `textAlign`, `textBaseline`.
7. `getImageData`, `putImageData`, `createImageData`.
8. `toDataURL` and `toBlob`: PNG, and JPEG with a quality. WebP when it is free.
9. `createImageBitmap` (from `<img>`, a canvas, a `Blob`, `ImageData`), with resize options.
10. `OffscreenCanvas` and `convertToBlob`. In a worker this needs worker scripts first (`browser_worker.rs`: they do not run).
11. The canvas in the page frame: the frame shows the newest bitmap.

Gradients, patterns, shadows and `filter` come after this list. Fingerprint scripts use text, gradients and arcs, and then
`toDataURL`. They must not throw. They do not need a match with Chrome.

## 3. How real engines do it

Facts below come from the engine sources and their public docs as far as we know them. "Hypothesis" marks what we did not check in source.

- **Chrome.** The context records calls into a `cc::PaintRecord` (a Skia display list) and does not raster each call. A read
  (`getImageData`), a `drawImage` of the canvas, or a frame flushes the record into a `CanvasResourceProvider`. The provider is GPU
  (Skia Ganesh, now Graphite) or CPU (a Skia raster surface). The `willReadFrequently: true` option, or many readbacks, selects CPU
  raster, because a GPU readback is slow. Pixels are RGBA8 premultiplied. `getImageData` un-premultiplies, so an alpha below 255 loses
  precision. Hypothesis: Chrome flushes the record when it holds too many ops or too much memory, so an animation does not grow it.
- **Firefox.** `CanvasRenderingContext2D` draws through Moz2D (`gfx::DrawTarget`). The software backend is Skia. Since about
  Firefox 110, "accelerated canvas" draws with `DrawTargetWebgl` in the GPU process. The content process records the commands
  (`DrawTargetRecording`) and the GPU process plays them (`CanvasTranslator`).
- **Servo.** The canvas runs in its own thread (`canvas_paint_thread`). Script sends drawing messages over IPC. The backend was
  `raqote` (a CPU rasterizer in Rust). Hypothesis: in 2025 Servo added `vello` (GPU compute) and `vello_cpu` backends, selected by a
  preference, and `raqote` is on its way out.
- **WebKit.** The context draws through `GraphicsContext`. The backend is CoreGraphics on Apple platforms. GTK and WPE used Cairo, and
  moved to Skia in 2024. With the GPU process, the web process records a display list and the GPU process plays it
  (`RemoteDisplayListRecorder`).
- **Ladybird.** LibWeb draws through `Gfx::Painter`. In 2024 Ladybird moved its 2D drawing to Skia (`PainterSkia`), for the page and
  for canvas.

Common points: every engine uses a mature 2D rasterizer (Skia in four of five), premultiplied RGBA8, and a recorded command list
between script and raster. Two of them (Chrome, Firefox) raster only when the result is needed.

## 4. Options

All options share the JS side: a real `CanvasRenderingContext2D` interface object, with the drawing state on the host (a Rust struct
per context, not JS properties). A setter parses a color once and stores it as four bytes. A call reads the state with no JS
property lookup.

### A. tiny-skia, eager

- **Data:** one `tiny_skia::Pixmap` per canvas, made at the first draw. RGBA8 premultiplied, `4 * w * h` bytes. A state stack
  (`Vec<State>`: transform, paint, line style, clip mask, alpha, composite, font).
- **Algorithm:** each call rasters at once. Paths go to `PathBuilder`, then `fill_path` or `stroke_path` (scanline, anti-aliased,
  nonzero and evenodd). Images go through `draw_pixmap` with a transform and bilinear or bicubic filter. Clip is a `Mask`.
  Gradients and patterns are `Shader`s. tiny-skia has most Skia blend modes (all 12 Porter-Duff modes and the separable ones).
- **Text:** tiny-skia has no text. Each glyph outline comes from `ab_glyph` (already a dep of `bare-paint`) as a path and is filled
  with the current transform. Upright text at a plain scale can use the glyph mask cache (`crates/paint/src/glyphs.rs`) instead.
- **Complexity:** a fill is `O(edges * log edges + covered pixels)`. A `drawImage` is `O(destination pixels)`.
- **Size and build (measured, archMachine, 2026-11-26):** a probe binary with `fill_path`, `fill_rect`, `stroke_path` and
  `draw_pixmap` is 643,744 bytes stripped, against 351,520 for an empty binary: **+292 KB**. A clean release build of the probe takes
  3.1 s wall (tiny-skia and `tiny-skia-path` are about 22,500 lines; their new deps are `arrayref`, `arrayvec`, `bytemuck`, `cfg-if`).
  `png` is shared. The `png-format` feature is off.
- **Speed (measured, same probe, 3 runs, 1.06 to 1.17 ms):** per loop on a 512x512 pixmap: a rotated anti-aliased circle fill, a
  100x100 rect, a half-scale `draw_pixmap` of the whole pixmap and a 3 px circle stroke. The probe is a single process, not the browser.
- **Memory:** `4 * w * h` per canvas from the first draw. A 1,600x1,600 photo is 10 MiB.
- **Cost:** every call pays its raster now, also when nobody reads the canvas.

### B. Extend `bare-paint`

- **Data:** the same `Vec<u32>` (`0xAARRGGBB`) that paint uses, as premultiplied ARGB, plus a state stack.
- **Work needed:** blend over a transparent destination; an active-edge-list scanline for `fill_poly` (today `O(H * E)`); a stroker
  that outputs polygons (the SVG code has one, `svg.rs`), to replace the distance-per-pixel `stroke`; image draw with a transform and a
  bilinear filter; gradients; patterns; clip masks; the composite modes; evenodd.
- **Size:** no new crate. Hypothesis: 2,000 to 3,000 new lines in `bare-paint`, and their tests.
- **Gain:** SVG and page paint get the same fixes (transformed images, a faster fill).
- **Cost:** weeks of work before canvas reaches the quality of A. Each part must be tested against Chrome by hand. The risk of
  wrong pixels at edges is high.

### C. A lazy command list, rasterized with A (recommended)

- **Data:** per canvas a `Record`:
  - `ops: Vec<Op>`, an enum of about 30 small variants (`FillRect`, `FillPath(PathId)`, `DrawImage(ImageRef, src, dst)`, `SetTransform`,
    `SetFill(Paint)`, `Clip(PathId)`, `PutImage(Arc<[u8]>, x, y)`, `Text(..)`). About 24 to 48 bytes per op.
  - `paths: Vec<tiny_skia::Path>`, so a path is built once and shared by `fill` and `stroke`.
  - `base: Option<Arc<Pixmap>>`, the raster of the ops before the list. `None` means transparent black.
  - `state`: the live drawing state of the script (the JS side reads it; the ops hold copies only where the raster needs them).
- **When it rasters:** a read plays the ops on top of `base` and keeps the result as the new `base`. The reads are paint of a
  visible canvas (`Pixels::lazy` does this today for `Raster`), `getImageData`, `toDataURL`, `toBlob`, `createImageBitmap`, and a
  `drawImage` with this canvas as the source.
- **Work it avoids:**
  - A canvas that nobody reads never allocates pixels. Headless journeys paint only on a snapshot or screenshot, so a chart or an ad
    canvas costs only its ops.
  - An op that fills or clears the whole canvas with an opaque color or with `clearRect`, at the identity transform with no clip,
    drops all ops before it. An animation that clears each frame keeps one frame of ops.
  - A size change drops the list and `base` (the standard clears the bitmap).
- **Memory bound:** when `ops` passes 64 KiB, the next draw rasters the list into `base`. This is not a guard against a slow
  algorithm: the raster cost is the same, it moves earlier. It bounds memory for a canvas that draws a lot and never reads.
- **Images in ops:** `ImageRef` holds the source `Bitmap` (an `Arc`, no copy). A canvas source is rastered first, and the op holds an
  `Arc` of its `base`. A later draw on the source makes a new `Pixmap` (copy on write), so the op keeps the old pixels, as the
  standard says.
- **Complexity:** recording is `O(1)` per call, with no per-pixel work. Raster is the same as A, once per read.
- **Memory layout:** `base` is a tiny-skia RGBA8 premultiplied `Pixmap`. Two conversions exist:
  - `getImageData`: un-premultiply into the new `Uint8ClampedArray` (one pass over the rectangle; Chrome loses the same precision).
  - paint: the document keeps `Pixels::lazy`, and its maker converts `base` to `0xAARRGGBB` not premultiplied, once per frame
    that changed. Hypothesis for a later step: paint takes premultiplied RGBA directly and the copy goes away.
- **Expected cost:**
  - CPU: a page that draws and never reads saves all raster work (today's `fill` loop per call, and A's `fill_path`). A page that draws
    and reads once pays A's cost plus about 10 to 20 ns per recorded op (hypothesis: a `Vec` push).
  - Peak memory: zero pixel bytes until the first read. After it, `4 * w * h` for `base`, plus one `0xAARRGGBB` copy for each frame
    that paint read.
  - Time to ready: no change for a page with no canvas. tiny-skia code is not touched until a raster, so startup does not grow.

### Rejected

- **A GPU backend** (Skia Ganesh, `vello`): bare paints on the CPU into a `Vec<u32>` (`decisions.md`, "Platforms"). A GPU adds a
  device, a driver and much memory. No benefit for headless runs.
- **Bind real Skia** (`skia-safe`): a C++ build of many minutes and a binary of several MB. Hypothesis on the exact numbers; we did
  not build it.
- **`raqote`:** Servo is moving away from it. It is slower than tiny-skia on fills (hypothesis, from the tiny-skia README benchmarks).
- **Keep option B for canvas and A for nothing:** see B. The quality gap is too large for the time it saves.

### Comparison

|                            | A eager tiny-skia          | B own painter           | C lazy list + tiny-skia  |
| -------------------------- | -------------------------- | ----------------------- | ------------------------ |
| New code                   | ~1,500 lines of glue       | ~3,000+ lines of raster | A + ~400 lines of record |
| New deps                   | tiny-skia (+292 KB, 3.1 s) | none                    | tiny-skia                |
| Pixels of an unread canvas | `4wh` at first draw        | `4wh` at first draw     | 0                        |
| CPU per draw call          | raster now                 | raster now              | `O(1)` record            |
| Correctness at edges       | Skia-grade                 | to be built             | Skia-grade               |

## 5. Encoding

What the repo has (Cargo.lock, verified):

- PNG: `png 0.18.1` encodes (with `fdeflate`, `flate2`, `miniz_oxide`, `crc32fast`). `crates/paint/src/png.rs` writes RGB. Canvas needs
  RGBA, un-premultiplied. The writer gets a color type argument. No new dep.
- WebP: `image-webp 0.2.4` has a `WebPEncoder` (lossless only). `image/webp` can use it with no new dep. Lossy WebP is not
  available; the quality argument is ignored, which the standard allows.
- JPEG: no encoder. `zune-jpeg` only decodes.

What to add for JPEG (owner decision, see open questions):

- **J1. The `jpeg-encoder` crate.** Pure Rust, no deps (hypothesis: we did not check its current version, size or license). Baseline
  and progressive, 4:2:0, a quality from 1 to 100.
- **J2. Our own baseline encoder** in `crates/image`: forward DCT, the standard quantization tables scaled by quality, the standard
  Huffman tables, 4:2:0. Hypothesis: about 400 lines.
- **J3. No JPEG.** The standard says an unknown type gives PNG. A page that checks `blob.type` or the file size may fail, and an upload
  gets much larger.

Encoding off the page thread: `toBlob` and `convertToBlob` are async. They take the `base` `Arc` (no copy) and encode on the worker
threads that decode pictures (`bare_net::parallel`). The page thread queues the callback when the encode is done. `toDataURL` is
sync and encodes on the page thread. JPEG quality follows the standard: a number in 0..1, else the default (0.92 in Chrome).

PNG settings: `toDataURL` output differs between browsers, so no page can depend on exact bytes. `Compression::Fast` with the
`Sub` filter is the measured choice for captures (`png.rs`). Use it.

## 6. Plan

Each step is one PR. Tests go in new files `crates/bare/src/browser_canvas_<topic>_tests.rs`. The expected values come from Chrome
(same script, `getImageData` or `toDataURL` decoded). An edge pixel may differ by a small tolerance (for example 2 per channel);
interior pixels must match exactly. `tools/fixtures` has no WPT canvas tests (verified); step 1 adds a small set of WPT-style cases,
written by hand.

1. **Context object and host state.** A real `CanvasRenderingContext2D` with prototype accessors (`fillStyle`, `strokeStyle`,
   `lineWidth`, `globalAlpha`, `globalCompositeOperation`, `font`, `textAlign`, `textBaseline`, `lineCap`, `lineJoin`, `miterLimit`,
   `imageSmoothingEnabled`). Setters validate and normalize as Chrome does. `save` and `restore`. Tests: get/set round trips, invalid
   values are ignored, `instanceof`. No pixel change.
2. **Record and tiny-skia raster (option C core).** `Record`, `Op`, the read points, and the move of `fillRect`, `strokeRect`,
   `clearRect` onto it, anti-aliased. `getImageData` reads real pixels. Transforms. Tests: the current raster tests with Chrome edge
   values; `getImageData` after `fillRect`; transform cases; a canvas that is never read allocates no pixels (a counter).
3. **Paths and clip.** All path calls, `fill` and `stroke` with all line styles, `setLineDash`, `clip`, `Path2D`, `isPointInPath`,
   `isPointInStroke`. Tests: shapes from Chrome, evenodd, dashed strokes, clip then fill.
4. **Images.** `drawImage` (3, 5 and 9 arguments) from `<img>` and from a canvas; `putImageData` with a dirty
   rect; `imageSmoothingEnabled`; copy on write of a canvas source. Tests: scale down a decoded PNG and compare with Chrome;
   self-draw (`drawImage(canvas)` on its own canvas).
5. **Encoders.** `toDataURL` and `toBlob` for PNG and WebP, and JPEG if J1 or J2 is chosen. Async encode for `toBlob`. Tests:
   round trip through `bare_image` decode; the type and quality rules; the WhatsApp case as a unit test (draw a PNG, encode, decode,
   same pixels).
6. **Composite modes and text.** The Porter-Duff modes; `fillText`, `strokeText`, full `TextMetrics`. Tests: one case per mode
   from Chrome; text bounds within a tolerance (the fonts differ from Chrome's).
7. **ImageBitmap and createImageBitmap.** From `<img>`, canvas, `Blob` (decode on a worker), `ImageData`; resize options; `close`.
   Tests: sizes, the promise order, `drawImage(bitmap)`.
8. **OffscreenCanvas on the main thread** (`getContext("2d")`, `convertToBlob`, `transferToImageBitmap`). The worker form waits
   for worker scripts (`design-workers.md`).
9. **Gradients, patterns, shadows** (last; only if a journey needs them).

Each step also checks the WhatsApp photo journey by hand (without the live profile: a saved page that draws a picture and
re-encodes it).

Cost rows to add (to `tools/js/cost_rows.txt`, in step 2):

- `examples/browser_load canvas 1` and `10`: a script that draws N rectangles, paths and one `drawImage` on a 512x512 canvas and reads
  it once with `getImageData`. It measures record plus one raster.
- `examples/browser_load canvasidle 1` and `10`: the same draws and no read. It must show near-zero raster instructions. It guards the
  lazy design.
- Peak memory: the `canvasidle` case also prints peak RSS, which must not grow with the canvas size.

## 7. Open questions for the owner

1. tiny-skia as a new dependency (+292 KB, 3.1 s clean build): yes or no? If no, option B and a much longer plan.
2. JPEG: J1 (crate), J2 (own encoder) or J3 (PNG only)?
3. Limits: today one canvas holds at most 64 MiB (16 M pixels). Chrome allows 32,767 per side and 268 M pixels (hypothesis on the
   exact numbers). Keep 64 MiB per canvas? Should canvas bitmaps count against the page picture `BUDGET` (64 MiB)?
4. Text: glyph outlines through tiny-skia for all text (one path, slower), or the mask cache for upright text (faster, two paths)?
5. Paint input: keep the per-frame `0xAARRGGBB` copy, or a later PR that lets paint read premultiplied RGBA and drops the copy?
6. Fingerprint scripts: is any stable output enough, or should `toDataURL` of a known fingerprint pattern look like Chrome's?

## Notes outside this task

- The brief said `fillRect` paints nothing. Commit `54a6c5e2` already draws `fillRect`, `strokeRect` and `clearRect` into the
  bitmap, and paint shows it. `getImageData` and `toDataURL` are still stubs.
- `crates/paint/src/canvas.rs` `stroke` costs `O(area * segments)`. A long polyline (a chart line in a future SVG case) is slow there.
  Not changed here.
