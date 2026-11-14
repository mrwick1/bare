# Open-source parts

This file lists open-source projects that `bare` can use as crates, copy code from, or study. The policy is in [decisions.md](decisions.md#dependencies).

Survey date: 2026-11-05. Stars, licenses and last-push dates come from the GitHub API on that date. Other facts come from the project pages and news sources listed at the end. Facts marked "to verify" have one source only.

## How to use this list

1. Before a topic starts, check this list for a part that already does the work.
2. Measure the part on our load tests before we adopt it. A well-known name is not proof of speed or low memory use.
3. Use the part as a crate when it fits our speed and memory goals.
4. Copy the code into `bare` when we must change it. Then optimize it with our normal topic loop.
5. Record each adoption in [progress.md](progress.md) and its measurements in [resources.md](resources.md).

## License rules

`bare` is MIT or Apache-2.0.

- MIT, Apache-2.0, BSD and ISC code: we can use or copy it. Keep the copyright notice and the license text. `crates/js/LICENSE-V8` shows the pattern.
- Unicode-3.0 (ICU4X): permissive. Keep the notice. `crates/js/LICENSE-UNICODE` has the text, and the `license` field of `crates/js/Cargo.toml` names it. It covers the ICU4X crates and the data in `crates/js/src/icu_data`, which comes from CLDR and ICU. `crates/js/src/intl_num_data.rs` holds currency symbols that come from the same CLDR data.
- MPL-2.0 (Servo, Stylo, cssparser, lightningcss, Nova): file-level copyleft. We can use it as a crate. A file that we copy and change stays MPL-2.0, and its source must stay available. Keep copied MPL files in their own module with the license header.

## Whole engines

Study these for architecture. Take parts from them.

| Project | Language | License | Stars | Notes |
| --- | --- | --- | ---: | --- |
| [Servo](https://github.com/servo/servo) | Rust | MPL-2.0 | 38,331 | Full engine. The `servo` crate is on crates.io since 0.1.0 (2026-04-13), with an embedding API and LTS releases. JavaScript runs on SpiderMonkey (C++). |
| [Blitz](https://github.com/DioxusLabs/blitz) | Rust | Apache-2.0 | 4,346 | Beta. Modular HTML/CSS renderer: html5ever, Stylo, Taffy and Parley. No JavaScript upstream. Its shape is close to `bare`. |
| [Ladybird](https://github.com/LadybirdBrowser/ladybird) | C++, some Rust | BSD-2-Clause | 66,855 | Independent browser. Its JS front end (lexer, parser, scope collector, bytecode generator) moved to Rust in 2026-02. 97.8% of imported test262 subtests pass (2026-04 newsletter). |
| [Gosub](https://github.com/gosub-io/gosub-engine) | Rust | MIT | 3,691 | Browser engine. Active. Not evaluated yet. |

## JavaScript

| Project | License | Stars | What it gives |
| --- | --- | ---: | --- |
| [Boa](https://github.com/boa-dev/boa) | MIT | 7,659 | Full JS engine. 94.12% of test262 at v0.21 (2025-10). Register VM, NaN boxing. To verify: main is near 96%. Measured on 2026-11-05: slower than bare-js, so we copy its built-ins, not the engine. See [resources.md](resources.md#boa-compared-with-bare-js). |
| [Nova](https://github.com/trynova/nova) | MPL-2.0 | 1,958 | Data-oriented engine: one vector per heap type, 32-bit handles. 1.0 in 2026-03, about 80% of test262. Its docs say it is not fast yet. |
| [regress](https://github.com/ridiculousfish/regress) | Apache-2.0 | 225 | ECMAScript RegExp engine. Boa uses it. |
| [temporal_rs](https://github.com/boa-dev/temporal) | Apache-2.0 | 240 | Temporal API. Passes the test262 Temporal tests. |
| [ICU4X](https://github.com/unicode-org/icu4x) | Unicode-3.0 | 1,878 | Locale data and formatting: dates, numbers, collation, plural rules, segmentation. Adopted on 2026-11-06 for `Intl.Segmenter`: `icu_segmenter` 2.4 with compiled data, and no dictionary or LSTM data (+87,776 B in the stripped `bare`, +1.1%). Adopted on 2026-11-05 for `Intl.NumberFormat`, `Intl.Collator`, `Intl.PluralRules`, `toLocaleString` and `localeCompare`: `icu_decimal`, `icu_plurals`, `icu_collator`, `fixed_decimal`, `icu_locale_core`, with baked data of 5 English locales. Cost, measured: +520 KB in the stripped `eval_check` (+29%, against +1.45 MB with compiled data), 22 k instructions and no measurable memory for a first call. See [resources.md](resources.md#intlnumberformat-intlcollator-and-intlpluralrules). |

### Ported from Boa for the browser JavaScript APIs (2026-11-06)

Boa is MIT or Unlicense. We read its source and wrote our own code in the shape of its classes. We copied no file whole. Each file below says in its header which Boa path it follows.

| Our file | Boa path (Unlicense OR MIT) | What we took |
| --- | --- | --- |
| `crates/bare/src/browser_abort.rs` | `core/runtime/src/abort` | The shape of `AbortController` and `AbortSignal`. The DOM Standard adds what Boa leaves out: the `abort` event, `any`, `timeout`, `{signal}` for listeners and `fetch`, and `onabort`. |
| `crates/bare/src/browser_editing.rs` | Ladybird `Libraries/LibWeb/Editing/` (BSD-2-Clause, commit 2e2af354): `ExecCommand.cpp`, `CommandNames.cpp`, `Commands.cpp`, `Internal/Algorithms.cpp` | The entry points `execCommand` and `queryCommand*`, and the actions of `insertText`, `delete`, `forwardDelete`, `insertParagraph`, `insertLineBreak` and `selectAll`, simplified. The notice is `crates/bare/LICENSE-LADYBIRD`. |
| `crates/bare/src/browser_encoding.rs` | `core/wintertc/src/base64` | The shape of `btoa` and `atob`. The HTML forgiving-base64 rules decide the errors. |
| `crates/js/src/eval_intl_segmenter.rs` | `core/engine/src/builtins/intl/segmenter` | The shape of `Intl.Segmenter`, `%Segments%` and `%SegmentIterator%`. The break rules are ICU4X (`icu_segmenter`, Unicode-3.0), not Boa's. |

### Ported for the Selection methods (2026-11-12)

| Our file | Source (license) | What we took |
| --- | --- | --- |
| `crates/bare/src/browser_range.rs` | Ladybird `Libraries/LibWeb/Selection/Selection.cpp` (BSD-2-Clause) | The steps of `setBaseAndExtent`, `extend` and `containsNode`, with the direction. We wrote our own code in the shape of our selection. We copied no file. The notice is `crates/bare/LICENSE-LADYBIRD`. |

### Ported for the "in select" tree builder mode (2026-11-12)

| Our file | Source (license) | What we took |
| --- | --- | --- |
| `crates/html/src/tree.rs` (`in_select`) | html5ever `tree_builder/rules.rs`, InSelect (MIT OR Apache-2.0); Ladybird `HTMLParser::handle_in_select` (BSD-2-Clause) | The rules of HTML 13.2.6.4.16, classic. We wrote our own code in the shape of our builder. We copied no file. |

`String.prototype.matchAll` and `structuredClone` copy nothing from Boa. `matchAll` follows ECMA-262. `structuredClone` extends the clone that `history.pushState` already used.

## HTML, CSS and layout

`bare` already has its own parser, cascade and layout. Use these as references, or replace ours where they measure better.

| Project | License | Stars | What it gives |
| --- | --- | ---: | --- |
| [html5ever](https://github.com/servo/html5ever) | MIT or Apache-2.0 | 2,656 | Spec HTML5 tokenizer and tree builder. |
| [cssparser](https://github.com/servo/rust-cssparser) | MPL-2.0 | 875 | CSS Syntax Level 3 tokenizer and parser. |
| [Stylo](https://github.com/servo/stylo) | MPL-2.0 | 347 | Firefox and Servo CSS engine: parsing, cascade, computed values. |
| [lightningcss](https://github.com/parcel-bundler/lightningcss) | MPL-2.0 | 7,726 | Fast CSS parser, transformer and minifier. |
| [Taffy](https://github.com/DioxusLabs/taffy) | MIT | 3,735 | Block, flexbox and grid layout. |

### Idea taken for the layout map (2026-11-06)

Taffy (MIT), Servo and Chrome (`NGLayoutResult`) keep the result of a box for the space it was laid out in. `crates/layout/src/memo.rs` does the same for one layout. We took the idea only. We copied no code and no file, so no notice is needed. The key, the replay of side effects and the start rule are our own. See `docs/resources.md`, "Layout work of geometry reads".

## Paint, fonts and text (S4)

| Project | License | Stars | What it gives |
| --- | --- | ---: | --- |
| [tiny-skia](https://github.com/linebender/tiny-skia) | BSD-3-Clause | 1,684 | CPU 2D renderer, a Skia subset. Fits the OS-independent pixel buffer of S4. |
| [vello](https://github.com/linebender/vello) | Apache-2.0 | 4,469 | GPU compute 2D renderer. |
| [kurbo](https://github.com/linebender/kurbo) | Apache-2.0 | 1,025 | Curves and shapes. |
| [fontations](https://github.com/googlefonts/fontations) | Apache-2.0 | 854 | Font reading (`read-fonts`, `skrifa`). |
| [fontdue](https://github.com/mooman219/fontdue) | Apache-2.0 | 1,716 | Small, fast glyph rasterizer. |
| [swash](https://github.com/dfrg/swash) | Apache-2.0 | 881 | Font introspection, shaping and glyph rendering. |
| [harfrust](https://github.com/harfbuzz/harfrust) | MIT | 324 | Official HarfBuzz port. Replaces rustybuzz, which is archived. |
| [Parley](https://github.com/linebender/parley) | Apache-2.0 | 775 | Rich text layout. Blitz uses it. |
| [cosmic-text](https://github.com/pop-os/cosmic-text) | Apache-2.0 | 2,186 | Multi-line text: shaping, layout and editing. |

## Window and input (S4)

| Project | License | Stars | What it gives |
| --- | --- | ---: | --- |
| [smithay-client-toolkit](https://github.com/Smithay/client-toolkit) | MIT | 443 | Wayland client helpers. Small. Wayland only. |
| [winit](https://github.com/rust-windowing/winit) | Apache-2.0 | 6,249 | Cross-platform windows and input. |
| [wgpu](https://github.com/gfx-rs/wgpu) | Apache-2.0 | 18,466 | GPU API. Needed only for a GPU renderer. |

### Adopted for S4 topic 1 (2026-11-05)

| Part | License | Used as | Measured |
| --- | --- | --- | --- |
| [ab_glyph](https://github.com/alexheretic/ab-glyph) 0.2.32, with `ab_glyph_rasterizer` 0.1.10, `owned_ttf_parser` 0.25.1 and `ttf-parser` 0.25.1 | Apache-2.0 (the last two MIT OR Apache-2.0) | crate: glyph outlines to coverage masks | 1.1 to 2.3 µs for a glyph, +165 KB of binary |
| [png](https://github.com/image-rs/image-png) 0.18.1 and its dependencies (`fdeflate`, `flate2`, `miniz_oxide`, `crc32fast`, `bitflags`, `simd-adler32`, `adler2`, `cfg-if`, `zlib-rs`) | MIT OR Apache-2.0 (`miniz_oxide`, `zlib-rs` and `adler2` also Zlib or 0BSD) | crate: PNG capture | 1.5 ms for 1280 by 800, +161 KB of binary |
| [Noto Sans Mono](https://github.com/notofonts/latin-greek-cyrillic) 2.014, Regular and Bold, cut down by `pyftsubset` | SIL OFL 1.1 | copied data: `data/fonts/*.ttf`, built into `bare-paint` | 22 KB each in topic 1, 32 KB each since S4 topic 3 (rupee, arrows, punctuation, a few math signs). License text: `data/fonts/OFL.txt` |
| [Noto Sans Symbols2](https://github.com/notofonts/symbols) (the Regular file of the system package), cut down to 14 characters by `pyftsubset` | SIL OFL 1.1 | copied data: `data/fonts/NotoSansSymbols2-subset.ttf`, built into `bare-paint` as the fallback face for `✓ ✔ ✕ ✖ ✗ ✘ ★ ☆ ☰ ⚠ ✉ ✎ ✚` | 4 KB (S4 topic 3). License text: `data/fonts/OFL-Symbols2.txt` (the same OFL 1.1, with the copyright line of this font) |

### Adopted for S4 topic 2 (2026-11-05)

| Part | License | Used as | Measured |
| --- | --- | --- | --- |
| [smithay-client-toolkit](https://github.com/Smithay/client-toolkit) 0.21.1 with no default features (no calloop, no xkbcommon), and `wayland-client` 0.31.15, `wayland-backend` 0.3.17, `wayland-protocols` 0.32.13 (and its `-wlr`, `-misc`, `-experimental` siblings), `wayland-csd-frame`, `wayland-cursor`, `wayland-scanner`, `wayland-sys`, `xcursor`, `cursor-icon`, `quick-xml`, `downcast-rs`, `smallvec`, `thiserror`, `log`, `memchr`, `memmap2`, `xkeysym` | MIT (`downcast-rs`, `smallvec`, `thiserror`, `log`, `memmap2`, `xkeysym`, `cursor-icon`: MIT or Apache-2.0 or Zlib; `memchr`: Unlicense or MIT) | crate: Wayland window, `wl_shm` pools | window spike: +513 KB of binary, 0 CPU idle |
| [rustix](https://github.com/bytecodealliance/rustix) 1.1.6 (with `errno`, `libc`, `linux-raw-sys`), already a dependency of the Wayland crates | Apache-2.0 WITH LLVM-exception, Apache-2.0 or MIT | crate: `poll` on the Wayland socket and the wake pipe | - |

Dev-dependencies, only for `cargo test`: `wayland-server` 0.31.14 (the fake compositor of `crates/window/tests/lifecycle.rs`), and `wayland-protocols` with its `server` feature.

Measured and not adopted for the window: winit 0.30.13 with softbuffer 0.4 (+1,507 KB of binary, the same idle cost, 1.8 MB more RSS in the spike). See [resources.md](resources.md#wayland-window).

Measured and not adopted (the numbers are in [resources.md](resources.md#paint-and-pixel-capture)): tiny-skia 0.12 (6.5 times slower for glyphs, 3.4 times slower for plain rectangles, +1.1 MB), skrifa 0.44 with ab_glyph_rasterizer (+0.76 MB over ab_glyph), fontdue 0.9.5 (faster, but no fractional glyph offset). The first candidate list below still names tiny-skia and skrifa: it is the plan before the measurement.

The OFL asks that the license text travels with the font. It is in `data/fonts/OFL.txt`, and `crates/paint/Cargo.toml` lists `OFL-1.1` in its license. We do not sell the font by itself, and the subsets keep the glyph shapes unchanged.

## Network

`bare` already has TLS 1.3, HTTP/1.1 and URL parsing. These are references.

Adopted on 2026-11-07 for `Content-Encoding` in `crates/net`: flate2 1.1 with the miniz_oxide backend (MIT or Apache-2.0; `png` builds it already) and brotli-decompressor 5 (BSD-3-Clause or MIT; the decoder of the `brotli` crate, without the encoder). They add `alloc-stdlib` and `alloc-no-stdlib` (BSD-3-Clause). zlib-rs and `brotli` were measured and left out. See [resources.md](resources.md#compressed-bodies-and-the-connect-backup).

Copied on 2026-11-07 for HTTP/2 in `crates/net/src/hpack.rs`: the table of the Huffman code of HPACK (RFC 7541 appendix B, 257 entries) from the `h2` crate (hyperium/h2, MIT). The tests check the table against the examples of the RFC. The rest of HTTP/2 is our code. The `h2` crate itself was measured and not used: see [resources.md](resources.md#http2).

| Project | License | Stars | What it gives |
| --- | --- | ---: | --- |
| [rustls](https://github.com/rustls/rustls) | Apache-2.0, MIT or ISC | 7,736 | TLS. |
| [hyper](https://github.com/hyperium/hyper) | MIT | 16,396 | HTTP/1 and HTTP/2. |
| [rust-url](https://github.com/servo/rust-url) | Apache-2.0 | 1,583 | WHATWG URL parser. |

## First candidates

This order is a proposal. It is not measured yet.

1. ICU4X for `Intl`, `toLocaleString` and `localeCompare`. Done for numbers, collation and plural rules on 2026-11-05. `Intl.DateTimeFormat` stays hand-written. ICU4X date formatting, relative time and lists are not adopted. Segmentation is adopted for `Intl.Segmenter` (2026-11-06).
2. The S4 stack: tiny-skia, skrifa, harfrust and Parley. S4 topic 1 is built with ab_glyph and hand-written rectangles, and S4 topic 2 with smithay-client-toolkit (see above). Nothing of ours is replaced.
3. regress and temporal_rs, when a page needs a RegExp feature or Temporal that we do not have.
4. Larger changes, each one a separate decision: Blitz as the base of the renderer. Boa as the JS engine was measured and rejected on 2026-11-05.

## Sources

- [Ladybird: adopting Rust (LWN)](https://lwn.net/Articles/1059812/)
- [Ladybird newsletter, 2026-02](https://ladybird.org/newsletter/2026-02-28/)
- [Ladybird (Wikipedia)](https://en.wikipedia.org/wiki/Ladybird_(web_browser))
- [Boa releases](https://github.com/boa-dev/boa/releases)
- [Boa blog](https://boajs.dev/blog)
- [hassidan/boa PR 1 (test262 count of a fork)](https://github.com/hassidan/boa/pull/1)
- [Nova](https://trynova.dev/)
- [NLnet: Nova](https://nlnet.nl/project/Nova/)
- [Blitz](https://github.com/dioxuslabs/blitz)
- [Servo 0.1.0 on crates.io](https://servo.org/blog/2026/04/13/servo-0.1.0-release/)
- [Servo LTS (Phoronix)](https://www.phoronix.com/news/Servo-Embed-Crates-LTS)

## Modern CSS colors

Added on 2026-11-14. The CSS crate uses [color 0.3.3](https://crates.io/crates/color/0.3.3). Its license is Apache-2.0 OR MIT. The dependency uses its default std feature. It provides color-space conversion, modern absolute-color parsing, missing-component handling and premultiplied interpolation. Bare owns mix grammar, deferred currentColor resolution and sRGB gamut mapping.

The mapping follows the [CSS Color 4 local-MINDE algorithm](https://www.w3.org/TR/css-color-4/#binsearch). Numeric regression cases use the local-MINDE results documented in [the WPT sRGB gamut-mapping test](https://github.com/web-platform-tests/wpt/blob/master/css/css-color/canvas-srgb-gamut-mapping.html). The tests allow one byte of rounding error. They do not copy the WPT harness.
