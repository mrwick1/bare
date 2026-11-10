# bare

`bare` is a web browser written from scratch in Rust. Its purpose is to learn how the core parts of a browser work.

The first parts were written by hand: URL parsing, DNS, HTTP, TLS, HTML, CSS, layout and JavaScript. Since 2026-11-05, `bare` also uses or copies open-source parts that are already fast and tested. See [docs/open-source.md](docs/open-source.md).

## Status

`bare` loads pages over HTTP/1.1, HTTP/2 and HTTPS (TLS 1.3 and 1.2). It parses HTML and CSS, runs JavaScript in a tree-walking interpreter, lays out and paints the page, and shows it in a Wayland window with pointer and keyboard input. A test driver (`bare-drive`) runs user journeys through the same input path. On 2026-11-07 it passed the 8-step staging login journey. On 2026-11-10 it passed 48 public-site journeys.

What is done, what comes next and what is missing: [docs/progress.md](docs/progress.md).

`bare` is a learning project. It is not safe for real browsing.

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
cargo test --workspace
```

Linux is the first target. Windows and macOS come later.

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
| `crates/image/`     | PNG, JPEG and WebP decoders                         |
| `crates/paint/`     | Display list, pixel buffer, PNG output              |
| `crates/window/`    | Wayland window and input                            |
| `crates/unicode/`   | NFC and Unicode data tables                         |
| `crates/drive/`     | `bare-drive`, the test driver for user journeys     |
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
