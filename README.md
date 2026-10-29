# bare

`bare` is a web browser written from scratch in Rust. Its purpose is to learn how the core parts of a browser work.

`bare` uses no crates. Every part is written by hand: URL parsing, DNS, HTTP, and later TLS, HTML, CSS, layout, paint and JavaScript. Only the Rust standard library and direct OS calls are allowed.

## Status

Early work. Today `bare` fetches a page over `http://` and prints its source.

What is done, what comes next and what is missing: [docs/progress.md](docs/progress.md).

`bare` is a learning project. It is not safe for real browsing.

## Build and run

You need Rust (edition 2024).

```sh
cargo run -- http://example.com
cargo test
```

Linux is the first target. Windows and macOS come later.

## Layout

| Folder              | Contents                                            |
| ------------------- | --------------------------------------------------- |
| `crates/bare/`      | The app                                             |
| `crates/net/`       | DNS, TCP and HTTP                                   |
| `crates/url/`       | URL parser and IDNA                                 |
| `crates/html/`      | HTML: encoding, tokenizer and DOM tree              |
| `crates/unicode/`   | NFC and Unicode data tables                         |
| `data/unicode/`     | Unicode data files and their license                |
| `data/html/`        | The WHATWG entity list and its license              |
| `tools/`            | Scripts that generate code from data files          |
| `docs/decisions.md` | Design decisions and their reasons                  |
| `docs/progress.md`  | The module plan, what is done, and known gaps       |

## License

Licensed under either of these, at your option:

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT License ([LICENSE-MIT](LICENSE-MIT))

The files in `data/unicode/` are under the Unicode License v3 ([data/unicode/LICENSE.txt](data/unicode/LICENSE.txt)). The file in `data/html/` is under CC BY 4.0 ([data/html/LICENSE.txt](data/html/LICENSE.txt)).
