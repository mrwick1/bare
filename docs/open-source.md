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
- Unicode-3.0 (ICU4X): permissive. Keep the notice.
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
| [ICU4X](https://github.com/unicode-org/icu4x) | Unicode-3.0 | 1,878 | Locale data and formatting: dates, numbers, collation, plural rules, segmentation. It covers `Intl`, `toLocaleString` and `localeCompare`. |

## HTML, CSS and layout

`bare` already has its own parser, cascade and layout. Use these as references, or replace ours where they measure better.

| Project | License | Stars | What it gives |
| --- | --- | ---: | --- |
| [html5ever](https://github.com/servo/html5ever) | MIT or Apache-2.0 | 2,656 | Spec HTML5 tokenizer and tree builder. |
| [cssparser](https://github.com/servo/rust-cssparser) | MPL-2.0 | 875 | CSS Syntax Level 3 tokenizer and parser. |
| [Stylo](https://github.com/servo/stylo) | MPL-2.0 | 347 | Firefox and Servo CSS engine: parsing, cascade, computed values. |
| [lightningcss](https://github.com/parcel-bundler/lightningcss) | MPL-2.0 | 7,726 | Fast CSS parser, transformer and minifier. |
| [Taffy](https://github.com/DioxusLabs/taffy) | MIT | 3,735 | Block, flexbox and grid layout. |

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

## Network

`bare` already has TLS 1.3, HTTP/1.1 and URL parsing. These are references.

| Project | License | Stars | What it gives |
| --- | --- | ---: | --- |
| [rustls](https://github.com/rustls/rustls) | Apache-2.0, MIT or ISC | 7,736 | TLS. |
| [hyper](https://github.com/hyperium/hyper) | MIT | 16,396 | HTTP/1 and HTTP/2. |
| [rust-url](https://github.com/servo/rust-url) | Apache-2.0 | 1,583 | WHATWG URL parser. |

## First candidates

This order is a proposal. It is not measured yet.

1. ICU4X for `Intl`, `toLocaleString` and `localeCompare`. These gates block the live login now.
2. The S4 stack: tiny-skia, skrifa, harfrust, Parley and smithay-client-toolkit. S4 has not started, so nothing of ours is replaced.
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
