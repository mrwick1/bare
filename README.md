# bare

`bare` is a web browser written from scratch in Rust. It has its own networking, TLS 1.3 and 1.2 with its own crypto, URL, HTML, CSS, layout, paint and image code, a Wayland window, and its own JavaScript engine, Vex. It is built for speed and low resource use: CPU instructions, peak memory and time to ready. It is the only browser of two projects, `deck` and `offstage`, which give it to AI agents.

## What works today

- **Network.** DNS with its own client. HTTP/1.1 and HTTP/2 with HPACK. TLS 1.3 and 1.2. Redirects, a cookie jar, `gzip`, `deflate` and Brotli bodies, an RFC 9111 reply cache and a disk cache. WebSocket with `permessage-deflate`. Downloads with resume, retry, parallel ranges and a queue. Session import and export (cookies, `localStorage`, `sessionStorage`).
- **HTML and CSS.** The HTML tokenizer and tree builder, with encoding detection. A CSS parser and cascade with selectors, `calc()`, custom properties, `@media`, `@supports`, `@font-face`, `@keyframes`, transitions, transforms and modern color spaces.
- **Layout and paint.** Block, inline, float, flex, grid and table layout, positioning, scrolling and form controls. Text from built-in Noto fonts. Inline SVG, `<canvas>`, and PNG, JPEG, WebP and GIF images.
- **Window.** A Wayland window with pointer and keyboard input. A page can also paint to a PNG with no window.
- **Vex.** A JavaScript engine with a bytecode interpreter, shapes and inline caches, a non-moving garbage collector, promises, generators, `async` functions, modules, regular expressions and `Intl`. It passes 29,217 test262 tests.
- **Agents.** `bare-drive` runs user journeys through the real input path. `bare-mcp` is an MCP server on top of it. 48 public-site journeys run on `bare`.

`bare` is not hardened like a mainstream browser, and its TLS code has had no outside audit. Do not use it as your everyday browser. `deck` and `offstage` run it for agents.

## Vex

Vex is the JavaScript engine in `crates/vex`. A value is 64 bits (NaN-boxing). Objects share shapes, and property reads hit inline caches. The compiler goes from the parse to register bytecode, and a function compiles at its first call. Built-ins are built when a script first touches them, so startup does not grow with the number of built-ins. An empty script costs 0.70 M instructions and 5.95 MiB of peak memory. See [docs/vex.md](docs/vex.md).

## Agents and MCP

`deck` (a self-hosted AI agent in a web app) and `offstage` (a caged desktop for AI agents) use `bare-mcp` and `bare-drive` as their only browser. Each release ships both programs for Linux amd64, Linux arm64 and Windows amd64.

For agent interaction, use `open`, `snapshot` or `find`, then `act` with an element ref.
`act` keeps its text receipt and also returns `action_status`, `page_status` and `verification` in `structuredContent`.
A completed action does not prove that its business result succeeded.
Add an `expect` string to check the result, for example:

```json
{"op":"click","loc":"role=button name=\"Save\"","expect":"expect text=\"Saved\" visible"}
```

Assertions use the driver's `expect` syntax. JavaScript assertions are not accepted here.
If verification fails after a completed action, inspect the page before retrying.
`page_status: ready` means a layout exists and document loading ended. Application data can still be pending.
See [Agent action outcomes](docs/design-agent-outcomes.md) for the contract, [docs/agent-browser-guide.md](docs/agent-browser-guide.md) for the open, inspect, act, verify and recover loop, and [docs/agents.md](docs/agents.md) for the tools.

Agents that use `bare-mcp` must report each problem with it. The host gives the route: `offstage` agents use `report_issue`, area `bare`. Others file a GitHub issue on `mrwick1/bare` with the label `bare-mcp`. Give the tool call, the result and the expected result.

## Build and run

You need Rust (edition 2024).

```sh
cargo run -p bare -- http://example.com            # print the DOM tree
cargo run -p bare -- http://example.com --source   # print the page source
cargo run -p bare -- http://example.com --styles   # print the computed style of each element
cargo run -p bare -- http://example.com --layout   # print the place and size of each box (--width=N sets the window width)
cargo run -p bare -- http://example.com --resources # download and list the scripts and style sheets of the page
cargo run -p bare -- http://example.com --run      # load the page, run its scripts, timers and fetch calls, print a short status
cargo run -p bare -- --paint=out.png page.html     # paint a local file into a PNG, with no network
cargo run -p bare -- --window page.html            # open a Wayland window that shows the painted page (a local file or an http(s) URL)
cargo run -p bare-drive -- STEPS                   # run a step file (a user journey); --window shows it
cargo run -p bare-mcp                              # an MCP server over stdio: agents run journeys (headless, or a window on WAYLAND_DISPLAY)
cargo test --workspace
```

## Platforms

Linux is the first target, and only Linux has a window today. The core also builds and runs on Windows and macOS. `tools/portable_check.sh` checks that the core does not depend on one OS.

## Layout

| Folder              | Contents                                                     |
| ------------------- | ------------------------------------------------------------ |
| `crates/bare/`      | The app: DOM and engine bridge, page loader, sessions, CLI   |
| `crates/net/`       | DNS, TCP, HTTP/1.1, HTTP/2, WebSocket, caches, downloads     |
| `crates/tls/`       | TLS 1.3 and 1.2 and their crypto                             |
| `crates/url/`       | URL parser and IDNA                                          |
| `crates/html/`      | HTML: encoding, tokenizer, DOM tree, tree builder            |
| `crates/css/`       | CSS: tokenizer, parser, selectors, style cascade             |
| `crates/layout/`    | Layout: box tree, text, block, inline, flex, grid, table     |
| `crates/vex/`       | Vex, the JavaScript engine: parser, compiler, interpreter, heap |
| `crates/intl/`      | `Intl` data and formatting                                   |
| `crates/image/`     | PNG, JPEG, WebP and GIF decoders                             |
| `crates/paint/`     | Display list, pixel buffer, PNG output                       |
| `crates/window/`    | Wayland window and input                                     |
| `crates/unicode/`   | NFC and Unicode data tables                                  |
| `crates/tzdata/`    | IANA time zone data in the binary                            |
| `crates/os/`        | OS services: DNS, files, random numbers, trust roots, memory |
| `crates/observe/`   | Opt-in JSONL observations of a process                       |
| `crates/drive/`     | `bare-drive`, the driver for user journeys                   |
| `crates/mcp/`       | `bare-mcp`, an MCP server for agents                         |
| `crates/bench/`     | Time and memory benchmarks for each module                   |
| `crates/stress/`    | `bare-stress`, load tests in separate processes              |
| `data/`             | Unicode, time zone, HTML entity, CSS color, font and crypto test data, with their licenses |
| `tools/`            | Code generators, test fixtures and check scripts             |
| `docs/`             | Documentation: see [docs/README.md](docs/README.md)          |

## License

Licensed under either of these, at your option:

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT License ([LICENSE-MIT](LICENSE-MIT))

The files in `data/unicode/` and `data/tz/windowsZones.xml` are under the Unicode License v3 ([data/unicode/LICENSE.txt](data/unicode/LICENSE.txt)). `data/tz/tzdata.zi` is public domain. The file in `data/html/` is under CC BY 4.0 ([data/html/LICENSE.txt](data/html/LICENSE.txt)). The file in `data/css/` is under the MIT License ([data/css/LICENSE.txt](data/css/LICENSE.txt)). The fonts in `data/fonts/` are under the SIL Open Font License ([OFL.txt](data/fonts/OFL.txt), [OFL-Arabic.txt](data/fonts/OFL-Arabic.txt), [OFL-Symbols2.txt](data/fonts/OFL-Symbols2.txt)). The Wycheproof vectors in `data/crypto/ecdsa/` are under the Apache License 2.0 ([LICENSE-wycheproof-APACHE-2.0](data/crypto/ecdsa/LICENSE-wycheproof-APACHE-2.0)).

Third-party components and their notices: [docs/third-party.md](docs/third-party.md). Engine monitoring: [status and controls](docs/monitoring.md).
