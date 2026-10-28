# bare

`bare` is a web browser written from scratch in Rust. Its purpose is to learn how the core parts of a browser work.

`bare` uses no crates. Every part is written by hand: URL parsing, DNS, HTTP, and later TLS, HTML, CSS, layout, paint and JavaScript. Only the Rust standard library and direct OS calls are allowed.

## Status

Early work. Today `bare` fetches a page over `http://` and prints its source.

Done:

- URL parsing as in the WHATWG URL Standard: relative links, IPv4 and IPv6 hosts, percent-encoding, and Unicode host names (UTS #46 with NFC, bidi and joiner rules, Punycode).
- A DNS client: `/etc/hosts`, a TTL cache, queries over UDP with a TCP fallback.
- A minimal HTTP/1.1 client.

Next: the rest of HTTP, then the HTML parser and the DOM. The full plan is in [docs/decisions.md](docs/decisions.md#module-plan).

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
| `crates/net/`       | DNS and HTTP                                        |
| `crates/url/`       | URL parser and IDNA                                 |
| `crates/unicode/`   | NFC and Unicode data tables                         |
| `data/unicode/`     | Unicode data files and their license                |
| `tools/`            | Scripts that generate code from data files          |
| `docs/decisions.md` | Design decisions, the module plan and their reasons |

## License

Licensed under either of these, at your option:

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT License ([LICENSE-MIT](LICENSE-MIT))

The files in `data/unicode/` are under the Unicode License v3 ([data/unicode/LICENSE.txt](data/unicode/LICENSE.txt)).
