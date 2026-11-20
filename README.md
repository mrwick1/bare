# bare

`bare` is a web browser written from scratch in Rust. It is the only browser of two projects, `deck` and `offstage`. It is also a project to learn how the core parts of a browser work.

The first parts were written by hand: URL parsing, DNS, HTTP, TLS, HTML, CSS, layout and JavaScript. Since 2026-11-05, `bare` also uses or copies open-source parts that are already fast and tested. See [docs/open-source.md](docs/open-source.md).

## Who uses bare

| User | How it uses `bare` | Version | Where its `bare` problems go |
| --- | --- | --- | --- |
| [`deck`](https://github.com/mrwick1/deck), a self-hosted AI agent in a web app | Starts `bare-mcp` from `PATH`, or from `bare.command` in `deck.toml`, at the first browser tool call. `bare` is its only browser. | The `bare-mcp` that is on `PATH` | As `deck` tells its agent |
| [`offstage`](https://github.com/mrwick1/offstage), a caged desktop for AI agents | Installs `bare-mcp` and `bare-drive` from a release. It starts one `bare-mcp` for each browser session and gives its tools as `browser_*`. `bare` is its only browser. | The newest release (`OFFSTAGE_BARE_VERSION` in `install.sh` overrides it) | `offstage` `report_issue`, area `bare` |
| Learning | Each part is a study of how real engines work. Design documents in `docs/` compare `bare` with Chrome, Firefox, Servo, WebKit and Ladybird. | `main` | — |

A lean browser for a small, focused group of users is a possible later goal. No work on it has started.

The plan: `offstage` becomes the only way an agent reaches `bare`, `deck` included, and `bare` gets WebDriver BiDi as its public interface. See [docs/design-agent-interface.md](docs/design-agent-interface.md).

### Change bare-mcp with both users in mind

- A change to a `bare-mcp` tool, its arguments or its results reaches `deck` at once, and reaches `offstage` at the next `bare` release. Check both before you merge.
- `offstage` installs the newest release. A release reaches every `offstage` user at the next install or update. The `gate` job of `release.yml` runs the offstage browser test (`TestBareBrowser`, offstage `beta`) against the new Linux amd64 binaries. The release is published only when it passes.
- `bare-mcp` has no issue route and no host-specific option. The host (`offstage` or `deck`) gives its agent the rule for where to report problems, and places any window through `WAYLAND_DISPLAY`.

## Status

`bare` loads pages over HTTP/1.1, HTTP/2 and HTTPS (TLS 1.3 and 1.2). It parses HTML and CSS, runs JavaScript in a tree-walking interpreter, lays out and paints the page, and shows it in a Wayland window with pointer and keyboard input. A test driver (`bare-drive`) runs user journeys through the same input path. On 2026-11-07 it passed the 8-step staging login journey. On 2026-11-10 it passed 48 public-site journeys.

What is done, what comes next and what is missing: [docs/progress.md](docs/progress.md).

`bare` is not hardened like a mainstream browser. Do not use it as your everyday browser. `deck` and `offstage` run it for agents.

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
cargo run -p bare-drive -- STEPS           # run a step file (a user journey); --window shows it
cargo run -p bare-mcp                      # an MCP server over stdio: agents run journeys (headless, or a window on WAYLAND_DISPLAY)
cargo test --workspace
```

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
See [Agent action outcomes](docs/design-agent-outcomes.md) for the contract.

Each release carries prebuilt `bare-mcp` and `bare-drive` binaries for Linux amd64, Linux arm64 and Windows amd64.

Agents: read [docs/agent-browser-guide.md](docs/agent-browser-guide.md) for the open, inspect, act, verify and recover loop.

Agents that use `bare-mcp` must report each problem with it. The host gives the route: `offstage` agents use `report_issue`, area `bare`. Others file a GitHub issue on `mrwick1/bare` with the label `bare-mcp`. Give the tool call, the result and the expected result. The owner's agents can file in the private Plane project "bare MCP" (`BMCP`) instead. Its API key is in the owner's vault as `bare/plane`.

Linux is the first target. The core must also build and run on Windows and macOS (see "Cross-platform core" in [docs/decisions.md](docs/decisions.md)). Today the code builds for both, and only Linux has a window. `tools/portable_check.sh` checks this.

## Layout

| Folder              | Contents                                            |
| ------------------- | --------------------------------------------------- |
| `crates/bare/`      | The app                                             |
| `crates/net/`       | DNS, TCP, HTTP/1.1 and HTTP/2                       |
| `crates/tls/`       | TLS 1.3 and 1.2 and their crypto, by hand           |
| `crates/url/`       | URL parser and IDNA                                 |
| `crates/html/`      | HTML: encoding, tokenizer, DOM tree, tree builder   |
| `crates/css/`       | CSS: tokenizer, parser, selectors, style cascade    |
| `crates/layout/`    | Layout: box tree, text, block and inline layout     |
| `crates/js/`        | JavaScript: parser, heap, tree-walking interpreter  |
| `crates/js-diff/`   | test262 and differential runners for both engines   |
| `crates/image/`     | PNG, JPEG and WebP decoders                         |
| `crates/paint/`     | Display list, pixel buffer, PNG output              |
| `crates/window/`    | Wayland window and input                            |
| `crates/unicode/`   | NFC and Unicode data tables                         |
| `crates/drive/`     | `bare-drive`, the test driver for user journeys     |
| `crates/mcp/`       | `bare-mcp`, an MCP server for end-to-end tests      |
| `crates/bench/`     | Time and memory benchmarks for each module          |
| `crates/stress/`    | `bare-stress`, load tests in separate processes     |
| `data/unicode/`     | Unicode data files and their license                |
| `data/html/`        | The WHATWG entity list and its license              |
| `data/css/`         | The CSS color names and their license               |
| `data/fonts/`       | Noto font subsets and their licenses                |
| `data/crypto/`      | ECDSA test vectors and their license                |
| `tools/`            | Code generators, test fixtures and check scripts    |
| `docs/engine-plan.md` | The JavaScript engine plan and its result         |
| `docs/open-source.md` | Open-source parts that `bare` uses or copies      |
| `docs/resources.md` | Time and memory measurements                        |
| `docs/decisions.md` | Design decisions and their reasons                  |
| `docs/progress.md`  | The module plan, what is done, and known gaps       |

## License

Licensed under either of these, at your option:

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT License ([LICENSE-MIT](LICENSE-MIT))

The files in `data/unicode/` are under the Unicode License v3 ([data/unicode/LICENSE.txt](data/unicode/LICENSE.txt)). The file in `data/html/` is under CC BY 4.0 ([data/html/LICENSE.txt](data/html/LICENSE.txt)). The file in `data/css/` is under the MIT License ([data/css/LICENSE.txt](data/css/LICENSE.txt)). The fonts in `data/fonts/` are under the SIL Open Font License ([OFL.txt](data/fonts/OFL.txt), [OFL-Arabic.txt](data/fonts/OFL-Arabic.txt), [OFL-Symbols2.txt](data/fonts/OFL-Symbols2.txt)). The Wycheproof vectors in `data/crypto/ecdsa/` are under the Apache License 2.0 ([LICENSE-wycheproof-APACHE-2.0](data/crypto/ecdsa/LICENSE-wycheproof-APACHE-2.0)).

Engine monitoring is under integration. See [monitoring status and controls](docs/monitoring.md).
