# Third-party components

This file lists the third-party components that `bare` uses, adapts or ships as data. Each entry gives the component, where `bare` uses it, its license and a link. Exact versions are in `Cargo.lock`.

`bare` is under the Apache License 2.0 or the MIT License (see the [README](../README.md#license)). A component keeps its own license and notice.

## Rust crates

| Component | Where it is used | License | Link |
| --- | --- | --- | --- |
| ICU4X: `icu_collator`, `icu_collections`, `icu_decimal`, `icu_locale_core`, `icu_locale_fallback`, `icu_normalizer`, `icu_plurals`, `icu_provider`, `icu_segmenter`, `fixed_decimal` | `crates/intl`: `Intl.NumberFormat`, `Intl.Collator`, `Intl.PluralRules`, `Intl.Segmenter`, `toLocaleString`, `localeCompare`. Baked data of 5 English locales is in `crates/intl/src/icu_data`. | Unicode-3.0 | [ICU4X](https://github.com/unicode-org/icu4x) |
| ICU4X `icu_properties` (compiled data) | `crates/vex` (the Vex engine) | Unicode-3.0 | [ICU4X](https://github.com/unicode-org/icu4x) |
| ICU4X `icu_segmenter` (compiled data) | `crates/layout`: grapheme boundaries. Layout copies no ICU code or data file. | Unicode-3.0 | [ICU4X](https://github.com/unicode-org/icu4x) |
| `color` 0.3.3 (feature `libm`) | `crates/css`: color-space conversion and modern color parsing | Apache-2.0 OR MIT | [crates.io](https://crates.io/crates/color/0.3.3) |
| `libm` 0.2.16 | Math backend of `color` | MIT | [crates.io](https://crates.io/crates/libm/0.2.16) |
| `ab_glyph` 0.2, with `ab_glyph_rasterizer`, `owned_ttf_parser` and `ttf-parser` | `crates/paint`: glyph outlines to coverage masks | Apache-2.0 (`owned_ttf_parser` and `ttf-parser`: MIT OR Apache-2.0) | [ab_glyph](https://github.com/alexheretic/ab-glyph) |
| `png` 0.18, with `fdeflate`, `flate2`, `miniz_oxide`, `crc32fast`, `bitflags`, `simd-adler32`, `adler2`, `cfg-if`, `zlib-rs` | `crates/paint`, `crates/image` and `crates/drive`: PNG encode and decode | MIT OR Apache-2.0 (`miniz_oxide`, `zlib-rs` and `adler2` also Zlib or 0BSD) | [image-png](https://github.com/image-rs/image-png) |
| `flate2` 1.1 (backend `miniz_oxide`) | `crates/net`: gzip and deflate `Content-Encoding` | MIT OR Apache-2.0 | [flate2](https://github.com/rust-lang/flate2-rs) |
| `brotli-decompressor` 5, with `alloc-stdlib` and `alloc-no-stdlib` | `crates/net`: Brotli `Content-Encoding` (decoder only) | BSD-3-Clause OR MIT (`alloc-stdlib` and `alloc-no-stdlib`: BSD-3-Clause) | [rust-brotli-decompressor](https://github.com/dropbox/rust-brotli-decompressor) |
| `psl` 2.1.253 and `psl-types` 2.0.11 | `crates/bare`: registrable-domain classification for fetch metadata. The list is pinned. There is no run-time download. | MIT OR Apache-2.0 | [psl](https://crates.io/crates/psl) |
| `smithay-client-toolkit` 0.21, with `wayland-client`, `wayland-backend`, `wayland-protocols` (and its `-wlr`, `-misc`, `-experimental` siblings), `wayland-csd-frame`, `wayland-cursor`, `wayland-scanner`, `wayland-sys`, `xcursor`, `cursor-icon`, `quick-xml`, `downcast-rs`, `smallvec`, `thiserror`, `log`, `memchr`, `memmap2`, `xkeysym` | `crates/window`: Wayland window and `wl_shm` pools | MIT (`downcast-rs`, `smallvec`, `thiserror`, `log`, `memmap2`, `xkeysym`, `cursor-icon`: MIT OR Apache-2.0 OR Zlib; `memchr`: Unlicense OR MIT) | [client-toolkit](https://github.com/Smithay/client-toolkit) |
| `rustix` 1.1 (with `errno`, `libc`, `linux-raw-sys`) | `crates/window`: `poll` on the Wayland socket and the wake pipe | Apache-2.0 WITH LLVM-exception, Apache-2.0 OR MIT | [rustix](https://github.com/bytecodealliance/rustix) |
| `zune-jpeg` 0.5 | `crates/image`: JPEG decode | MIT OR Apache-2.0 OR Zlib | [zune-image](https://github.com/etemesi254/zune-image) |
| `image-webp` 0.2 | `crates/image`: WebP decode | MIT OR Apache-2.0 | [image-webp](https://github.com/image-rs/image-webp) |
| `gif` 0.14 | `crates/image`: GIF decode | MIT OR Apache-2.0 | [image-gif](https://github.com/image-rs/image-gif) |
| `miniz_oxide` 0.9 and `crc32fast` 1 | `crates/net` and `crates/bare`: inflate, gzip CRC and the disk cache | MIT OR Zlib OR Apache-2.0 (`miniz_oxide`), MIT OR Apache-2.0 (`crc32fast`) | [miniz_oxide](https://github.com/Frommi/miniz_oxide) |
| `memmap2` 0.9 | `crates/window`: shared-memory buffers | MIT OR Apache-2.0 | [memmap2-rs](https://github.com/RazrFalcon/memmap2-rs) |
| `serde_json` 1 | `crates/bare`, `crates/drive`, `crates/mcp`, `crates/net` and `crates/observe`: JSON for the agent interface and traces | MIT OR Apache-2.0 | [serde-rs/json](https://github.com/serde-rs/json) |
| `stacker` 0.1 | `crates/vex` (Vex): stack growth in the parser and in JSON | MIT OR Apache-2.0 | [stacker](https://github.com/rust-lang/stacker) |
| `zerovec` 0.11 and `writeable` 0.6 | `crates/intl` | Unicode-3.0 | [ICU4X](https://github.com/unicode-org/icu4x) |
| `wayland-server` 0.31 (dev-dependency) | `crates/window/tests`: the fake compositor | MIT | [wayland-rs](https://github.com/Smithay/wayland-rs) |

## Copied or adapted code

| Component | Where it is used | License | Link |
| --- | --- | --- | --- |
| HPACK Huffman table (RFC 7541, appendix B, 257 entries) from the `h2` crate | `crates/net/src/hpack.rs`. The rest of HTTP/2 is our own code. | MIT | [hyperium/h2](https://github.com/hyperium/h2) |
| Ladybird `Libraries/LibWeb/Editing/` (commit 2e2af354), in the shape of its code: `execCommand`, `queryCommand*` and the editing actions | `crates/bare/src/browser_editing.rs` | BSD-2-Clause (`crates/bare/LICENSE-LADYBIRD`) | [LadybirdBrowser/ladybird](https://github.com/LadybirdBrowser/ladybird) |
| Ladybird `Libraries/LibWeb/Selection/Selection.cpp`: the steps of `setBaseAndExtent`, `extend` and `containsNode` | `crates/bare/src/browser_range.rs` | BSD-2-Clause (`crates/bare/LICENSE-LADYBIRD`) | [LadybirdBrowser/ladybird](https://github.com/LadybirdBrowser/ladybird) |
| html5ever `tree_builder/rules.rs` (InSelect) and Ladybird `HTMLParser::handle_in_select`: the rules of HTML 13.2.6.4.16, in our own code | `crates/html/src/tree.rs` (`in_select`) | MIT OR Apache-2.0 (html5ever), BSD-2-Clause (Ladybird) | [servo/html5ever](https://github.com/servo/html5ever) |
| Boa `core/runtime/src/abort`: the shape of `AbortController` and `AbortSignal` | `crates/bare/src/browser_abort.rs` | MIT OR Unlicense | [boa-dev/boa](https://github.com/boa-dev/boa) |
| Boa `core/wintertc/src/base64`: the shape of `btoa` and `atob` | `crates/bare/src/browser_encoding.rs` | MIT OR Unlicense | [boa-dev/boa](https://github.com/boa-dev/boa) |

The files of the Ladybird and Boa rows are our own code in the shape of the source. No source file is copied whole. Each file header names the source path.

The CSS Color 4 gamut mapping follows the [local-MINDE algorithm](https://www.w3.org/TR/css-color-4/#binsearch). Its numeric tests use the results of the [WPT sRGB gamut-mapping test](https://github.com/web-platform-tests/wpt/blob/master/css/css-color/canvas-srgb-gamut-mapping.html). They do not copy the WPT harness.

## Data files

| Data | Where it is used | License | Link |
| --- | --- | --- | --- |
| Unicode data files (`data/unicode/`) and `windowsZones.xml` (`data/tz/`) | `crates/unicode`, `crates/tzdata` | Unicode License v3 (`data/unicode/LICENSE.txt`) | [Unicode](https://www.unicode.org/license.txt) |
| IANA `tzdata.zi` (`data/tz/`) | `crates/tzdata` | Public domain | [IANA tz](https://www.iana.org/time-zones) |
| CLDR and ICU data (`crates/intl/src/icu_data`) | `Intl` formatting | Unicode-3.0 (`crates/intl/LICENSE-UNICODE`) | [CLDR](https://cldr.unicode.org) |
| WHATWG entity list (`data/html/`) | `crates/html` | CC BY 4.0 (`data/html/LICENSE.txt`) | [WHATWG](https://html.spec.whatwg.org/entities.json) |
| CSS color names (`data/css/`) | `crates/css` | MIT (`data/css/LICENSE.txt`) | [W3C CSS Color](https://www.w3.org/TR/css-color-4/) |
| Noto Sans Mono Regular and Bold, Noto Serif Regular and Bold, Noto Sans Symbols2: subsets cut with `pyftsubset` (`data/fonts/`) | `crates/paint`, built into the binary | SIL OFL 1.1 (`data/fonts/OFL.txt`, `data/fonts/OFL-Symbols2.txt`, `data/fonts/OFL-Arabic.txt`) | [Noto fonts](https://github.com/notofonts) |
| Wycheproof ECDSA vectors (`data/crypto/ecdsa/`) | Tests of `crates/tls` | Apache-2.0 (`LICENSE-wycheproof-APACHE-2.0`) | [C2SP/wycheproof](https://github.com/C2SP/wycheproof) |
