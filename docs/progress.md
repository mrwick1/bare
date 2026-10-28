# Progress

This file records what is built, what comes next and what is missing. Update it after each merged PR.

The reasons for the design are in [decisions.md](decisions.md).

## Modules

| #   | Module                                                 | State       |
| --- | ------------------------------------------------------ | ----------- |
| 1   | URL, DNS, TCP and HTTP                                 | in progress |
| 2   | HTML tokenizer and DOM tree                            | planned     |
| 3   | CSS parser and style cascade                           | planned     |
| 4   | Layout (box model, text flow)                          | planned     |
| 5   | Window (Wayland) and paint, bitmap font                | planned     |
| 6   | Input and navigation (links, scroll)                   | planned     |
| 7   | HTTPS (TLS 1.3 and crypto, by hand)                    | planned     |
| 8   | Tabs and event loop (one engine, a task queue per tab) | planned     |
| 9   | JavaScript (interpreter, no JIT)                       | planned     |
| 10  | Tab isolation and memory accounting (heap per tab)     | planned     |
| 11  | Resource guard (watchdog, freeze, discard, reload)     | planned     |

## Module 1: URL, DNS, TCP and HTTP

| #   | Topic                                                     | State   | Code                     |
| --- | --------------------------------------------------------- | ------- | ------------------------ |
| 1   | URLs                                                      | done    | `crates/url/`            |
| 2   | DNS                                                       | done    | `crates/net/src/dns.rs`  |
| 3   | TCP: time limits, Happy Eyeballs                          | done    | `crates/net/src/tcp.rs`  |
| 4   | HTTP/1.1 messages: requests, responses, where a body ends | next    | `crates/net/src/http.rs` |
| 5   | Connections: keep-alive, HTTP/2, HTTP/3                   | planned |                          |
| 6   | Redirects, status codes, caching                          | planned |                          |

What each done topic covers:

- URLs: the WHATWG URL Standard for `http://`. Relative links, IPv4 and IPv6 hosts, percent-encoding, and Unicode host names (UTS #46 with NFC, bidi and joiner rules, Punycode).
- DNS: `localhost`, `/etc/hosts`, a TTL cache, A and AAAA queries over UDP with a TCP fallback, alias (CNAME) chains, and checks against fake answers.
- TCP: Happy Eyeballs (IPv6 first, a new try every 250 ms, the first connection wins). Time limits: 10 seconds for one connection try, 30 seconds for a read or write.

## Known gaps

- HTTP: no chunked bodies. The example.com body shows the chunk sizes (`241`, `0`). Topic 4.
- HTTP: only status 200 is accepted. A redirect (301, 302) gives an error. Topic 6.
- URL: only `http://`. `https://` needs module 7.
- DNS: no short names from the `search` line of `/etc/resolv.conf`.
- DNS: no DNS-over-HTTPS. It needs module 7.
