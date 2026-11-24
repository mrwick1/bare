# Architecture

`bare` is one Rust workspace. Each stage of a page load is its own crate. The crates call each other in one direction, from the URL down to the pixels. The core crates do not call the OS. The `os` crate and the `window` crate hold the OS code.

## From URL to pixels

```text
URL --> net (DNS, TCP, TLS, HTTP) --> bytes
bytes --> html (decode, tokenize, build the DOM) --> DOM tree
DOM tree + style sheets --> css (select, cascade) --> computed style
DOM tree + computed style --> layout --> boxes and text fragments
fragments --> paint (display list, pixel buffer) --> pixels
pixels --> window (Wayland) or PNG
scripts --> vex (compile, run) <--> DOM and events through the host interface
```

1. **Load.** `net` resolves the host name, opens a TCP connection, runs the TLS handshake, and sends an HTTP/1.1 or HTTP/2 request. It decodes `gzip`, `deflate` and Brotli bodies. A page asks for many files at once. `net` runs them on worker threads.
2. **Parse.** `html` finds the encoding of the bytes, cuts them into tokens, and builds the DOM tree with the tree builder of the HTML standard. The scripts and style sheets of the page load while the parser reads.
3. **Style.** `css` parses the style sheets into rules and matches selectors from right to left. The cascade picks the winning declaration for each property and computes the values.
4. **Layout.** `layout` walks the DOM once and writes one rectangle for each box and each piece of text on a line. It does block, inline, float, flex, grid and table layout.
5. **Paint.** `paint` turns the fragments into a flat display list, then draws the list into a plain `u32` pixel buffer. `window` shows the buffer. A test can write it as a PNG.
6. **Script.** Vex runs the scripts. A script reads and changes the DOM, runs timers and promises, and calls `fetch`. A change marks the frame stale. The next frame cascades and lays out again only when the tree, the sheets, the window size or the user action state changed.

## Crates

| Crate | Role |
| --- | --- |
| `bare` | The app. It joins the DOM and the JavaScript engine (`window`, `document`, nodes, events), the page loader, the session and the command line. |
| `net`, `tls`, `url`, `os` | Network, TLS with its own crypto, URL parsing, and the OS services (DNS, files, random numbers, trust roots, time zone, pages of memory). |
| `html`, `css`, `layout`, `paint`, `image`, `unicode` | The rendering path. |
| `vex`, `intl`, `tzdata` | The JavaScript engine, `Intl` data and formatting, and the time zone data in the binary. |
| `window` | The Wayland window and its input. |
| `drive`, `mcp` | `bare-drive`, the journey driver, and `bare-mcp`, the MCP server on top of it. See [agents.md](agents.md). |
| `observe` | Opt-in JSONL observations of a process. It captures no page data on its own. |
| `bench`, `stress` | Time and memory benchmarks, and load tests that run in separate processes. |

## Process model

All tabs of a process share one engine. A session (`bare-drive`, `bare-mcp` or the window) owns one page, with its own cookie jar and storage. `bare-mcp` runs each live session on its own thread. A script runs under a step budget, so a runaway script ends. Peak memory and instruction counts are first-class goals: see [performance.md](performance.md).

## Platforms

The core builds and runs on Linux, Windows and macOS. Only Linux has a window today. Trust roots come from the OS certificate store. `tools/portable_check.sh` checks that the core does not depend on one OS.
