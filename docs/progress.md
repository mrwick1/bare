# Progress

This file records what is built, what comes next and what is missing. Update it after each merged PR.

The reasons for the design are in [decisions.md](decisions.md). Time and memory numbers for each module are in [resources.md](resources.md).

## Modules

| #   | Module                                                 | State       |
| --- | ------------------------------------------------------ | ----------- |
| 1   | URL, DNS, TCP and HTTP                                 | done        |
| 2   | HTML tokenizer and DOM tree                            | done        |
| 3   | CSS parser and style cascade                           | in progress |
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
| 4   | HTTP/1.1 messages: requests, responses, where a body ends | done    | `crates/net/src/http.rs` |
| 5   | Connections: keep-alive                                   | done    | `crates/net/src/http.rs` |
| 6   | Redirects, status codes, caching                          | done    | `crates/net/src/http.rs` |

What each done topic covers:

- URLs: the WHATWG URL Standard for `http://`. Relative links, IPv4 and IPv6 hosts, percent-encoding, and Unicode host names (UTS #46 with NFC, bidi and joiner rules, Punycode).
- DNS: `localhost`, `/etc/hosts`, a TTL cache, A and AAAA queries over UDP with a TCP fallback, alias (CNAME) chains, and checks against fake answers.
- TCP: Happy Eyeballs (IPv6 first, a new try every 250 ms, the first connection wins). Time limits: 10 seconds for one connection try, 30 seconds for a read or write.
- HTTP/1.1 messages: a status line and headers parser. Three rules for where a body ends: chunked, `Content-Length`, or connection close. Chunked wins over `Content-Length`. A short body is an error. 1xx, 204 and 304 have no body.
- Connections: keep-alive. The `Client` keeps one idle connection for each server and reuses it. It drops a connection that is idle for more than 15 seconds, that the server closed, or that has extra bytes after a reply. A reused connection that fails is retried once on a new one (safe for GET only).
- Redirects, status codes, caching: `Client::get` follows 301, 302, 303, 307 and 308 (at most 20) and returns any status, so a 404 page is shown. A redirect to `https://` gives an error. The in-memory cache uses `max-age` and `Age`, honours `no-store` and `no-cache`, and revalidates a stale copy with `If-None-Match` and `If-Modified-Since` (a 304 reuses the copy).

## Module 2: HTML tokenizer and DOM tree

| #   | Topic                                                | State   | Code                            |
| --- | ---------------------------------------------------- | ------- | ------------------------------- |
| 1   | Bytes to text: encoding, newlines                    | done    | `crates/html/src/encoding.rs`   |
| 2   | Character references (the entity table)              | done    | `crates/html/src/charref.rs`    |
| 3   | Tokenizer (state machine, raw text)                  | done    | `crates/html/src/tokenizer.rs`  |
| 4   | DOM tree (arena)                                     | done    | `crates/html/src/dom.rs`        |
| 5   | Tree builder (implied tags, void elements), app      | done    | `crates/html/src/tree.rs`       |

What each done topic covers:

- Bytes to text: BOM, then the `Content-Type` charset, then a `<meta charset>` in the first 1024 bytes, then a guess (UTF-8 if valid, else windows-1252). UTF-8 and windows-1252 (also for latin1 and iso-8859-1). `\r\n` and `\r` become `\n`.
- Character references: all 2231 names from the WHATWG list (generated into `crates/html/src/entities.rs`), decimal and hex numbers, the old names without `;`, the stricter rule in attribute values, and the fixes for 0, surrogates, numbers that are too big, and `&#128;` to `&#159;` (windows-1252).
- Tokenizer: the state machine of the standard for tags, attributes (quoted, unquoted, repeats), comments (including `<!-->` and `--!>`), bogus comments, the doctype name, and raw text for `script`, `style`, `title`, `textarea`, `xmp`, `iframe`, `noembed`, `noframes` and `plaintext`. A tag cut by the end of the page is dropped. It is an iterator: it gives one token at a time and reads the page text in place, so the whole token list is never in memory. Names, texts and attribute values are slices of the page (`Cow::Borrowed`). A new `String` is made only when the text changes: capital letters in a name, a character reference, or a NUL. A test feeds it 3000 random pages to check that it never crashes or loops.
- DOM tree: an arena (one `Vec` of nodes, a `NodeId` is the place in it, 4 bytes). Nodes know their parent, and the first and last child and the next and previous sibling (a linked list, so no `Vec` per node). Names of elements and attributes are stored once in a table, and a node holds a number (an `Atom`). Texts, attribute values and comments are slices of one text buffer (a `Span`), and all attributes are in one list, so a node has no allocation of its own (36 bytes). Text next to text joins into one node. `dump` prints the tree in the html5lib test format.
- Tree builder: the modes before the body (initial, before html, before head, in head, after head), in body, text, after body. Missing `html`, `head` and `body` are added. Void elements. A new `p`, `li`, `dd`, `dt`, `option`, heading or `button` closes the open one. A stray end tag is ignored and never closes past a special element. Tables: missing `tbody`, `tr` and `colgroup` are added, a new cell closes the old one, a table in a cell nests, and text or tags that cannot be in a table move in front of it (foster parenting). The app prints the DOM tree (`--source` prints the page source). A test feeds it 4000 random pages and checks the links of every tree.

## Module 3: CSS parser and style cascade

| #   | Topic                                                | State   | Code                          |
| --- | ---------------------------------------------------- | ------- | ----------------------------- |
| 1   | CSS tokenizer                                        | done    | `crates/css/src/tokenizer.rs` |
| 2   | CSS parser (rules, declarations, error recovery)     | done    | `crates/css/src/parser.rs`    |
| 3   | Selectors (matching, specificity)                    | done    | `crates/css/src/selector.rs`, `crates/css/src/matching.rs` |
| 4   | Values (colors, lengths, shorthands)                 | planned |                               |
| 5   | Cascade and computed style, app prints the styles    | planned |                               |

What each done topic covers:

- CSS tokenizer: CSS Syntax Level 3, section 4. Idents, functions, at-keywords, hashes (with the id flag), strings, urls (quoted and unquoted), numbers (integer flag, exponents), percentages, dimensions, comments, `<!--` and `-->`. Escapes (`\41 `), NUL, bad strings and bad urls follow the standard. It is an iterator, and names and strings are slices of the style sheet (`Cow::Borrowed`) unless an escape changes them. On three real style sheets (bootstrap and two GTK themes) it gives no bad token, at about 170 MB/s.

- CSS parser: CSS Syntax Level 3, section 5. Style rules and at-rules, declarations with `!important`, and the error recovery of the standard: a broken declaration is skipped to the next `;`, a block that is not closed keeps its content, a rule with no `{` at the end is dropped, and `;` `}` inside strings, urls and `(...)` `[...]` `{...}` do not end anything. `@media`, `@supports`, `@layer`, `@container` and `@keyframes` hold rules, `@font-face` and `@page` hold declarations, and other blocks are skipped. The prelude and the values stay as slices of the style sheet for the next topics. Blocks inside blocks stop at 32 levels, and a run of 100,000 `(` does not overflow the stack. On bootstrap.min.css the at-rule count matches Chrome's (71 against 69: Chrome drops `@-o-keyframes` and `@-ms-viewport`).

- Selectors: the parser reads type, `*`, id, class, the seven attribute tests (with `i` and `s`), the combinators (space, `>`, `+`, `~`), `:first-child`, `:last-child`, `:only-child`, `:root`, `:empty`, `:link`, `:nth-child()`, `:nth-last-child()`, `:nth-of-type()`, `:nth-last-of-type()` (with `odd`, `even` and `an+b`), `:first-of-type`, `:last-of-type`, and `:not()` with a list. `:hover`, `:focus`, `:active`, `:visited`, `:invalid` and the like are valid and never match. A selector is stored from the right to the left, with names as `Atom`s of the document, and its specificity is one packed `u32`. One bad selector throws away the whole list, as the standard says. The matcher starts at the element, tests the last compound first, and goes up with a loop (a deep tree does not overflow the stack). On three style sheets from my disk, the only selectors that fail are pseudo-elements, the old `:before` and `:after`, vendor-prefixed names, and `:invalid` was added after the count.

## Known gaps

- HTTP: `Transfer-Encoding` codes other than `chunked` (for example `gzip`) are not supported. No `Accept-Encoding`, so no compression.
- HTTP: HTTP/2 and HTTP/3 are not built. They move to after module 7, because browsers use them only over TLS. HTTP/3 also needs QUIC.
- HTTP: one idle connection for each server. No pool of several at once, no parallel requests.
- HTTP cache: no `Expires`, no heuristic freshness (both need HTTP date parsing), and no 301 caching.
- HTTP cache: a reply with a `Vary` header is not stored. No disk cache. No size limit. Module 10 needs a limit for memory accounting.
- HTTP: redirects only for GET. The cache is not split by top-level site, as Chrome does.
- URL: only `http://`. `https://` needs module 7.
- DNS: no short names from the `search` line of `/etc/resolv.conf`.
- DNS: no DNS-over-HTTPS. It needs module 7.
- Encoding: no UTF-16 and no other encodings. The meta scan is simple: it does not skip comments. The guess is not a real detector.
- Tokenizer: the doctype's public and system names are skipped. Quirks mode needs them (modules 3 and 4).
- Tokenizer: no script data escapes (`<!--<script>` inside a script). `<noscript>` is read as a normal element. NUL characters in text are dropped.
- Tokenizer: it switches to raw text by itself. In a real browser the tree builder does it.
- DOM: the tree can only grow. No way yet to move or delete a node (the adoption agency algorithm and JavaScript need that). A `NodeId` from another document is not detected.
- Tree builder: no adoption agency algorithm and no list of active formatting elements. `<b><p>x</b>y</p>` and `<p><b>x<p>y` differ from browsers. This is a good topic for later, and a favourite interview question.
- Tree builder: no foreign content (SVG and MathML), `<template>`, `<select>` modes, framesets, `<noscript>`, quirks mode, and no merge of the attributes of a second `<html>` or `<body>` tag.
- CSS parser: no CSS nesting (a rule inside a style rule is skipped). At-rules inside a style rule are skipped. Nothing checks that a property or an at-rule name is known: that is for the later topics. The style rule count is a little higher than Chrome's (1335 against 1326 on bootstrap) because Chrome also drops rules with a selector that it does not know. Topic 3 should close this gap.
- Selectors: no `:is()`, `:where()`, `:has()`, `:only-of-type`, `:checked`, `:disabled` and the other state pseudo-classes. No namespaces. No pseudo-elements (`::before`, `::after`, `::placeholder`): a rule with one is dropped, so no generated content until module 4 decides. The matcher has no ancestor filter yet (topic 5 adds it with the rule index), so a long descendant selector walks all the ancestors. `:nth-child` counts the siblings for each element: a parent with thousands of children and an `:nth-child` rule is slow.
- Tree builder: a very deep tree is slow. 2,500 nested `div` take 9 ms, 5,000 take 38 ms and 10,000 take 151 ms: the time grows with the square of the depth. (Found by a test on selectors. To verify: the scope checks that walk the whole stack of open elements are the likely cause.)
- CSS tokenizer: the caller gives text. A style sheet that is not UTF-8 needs the encoding rules of the CSS standard (`@charset`, BOM, the `Content-Type` header). The tokenizer does not preprocess the input: it reads `\r`, `\r\n` and `\x0C` as newlines where the standard needs it.
- Tree builder: no parser pause for scripts (`document.write`), and no preload scanner. Both need module 9 and the network in the loop.
