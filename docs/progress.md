# Progress

This file records what is built, what comes next and what is missing. Update it after each merged PR.

The reasons for the design are in [decisions.md](decisions.md). Time and memory numbers for each module are in [resources.md](resources.md).

## Modules

The module numbers below remain stable for existing code and benchmark references. The next implementation order is S1 to S5 in the staging plan below.

| #   | Module                                                 | State       |
| --- | ------------------------------------------------------ | ----------- |
| 1   | URL, DNS, TCP and HTTP                                 | done        |
| 2   | HTML tokenizer and DOM tree                            | done        |
| 3   | CSS parser and style cascade                           | done        |
| 4   | Layout (box model, text flow)                          | in progress |
| 5   | Window (Wayland) and paint, bitmap font                | planned     |
| 6   | Input and navigation (links, scroll)                   | planned     |
| 7   | HTTPS (TLS 1.3 and crypto, by hand)                    | planned     |
| 8   | Tabs and event loop (one engine, a task queue per tab) | planned     |
| 9   | JavaScript (interpreter, no JIT)                       | planned     |
| 10  | Tab isolation and memory accounting (heap per tab)     | planned     |
| 11  | Resource guard (watchdog, freeze, discard, reload)     | planned     |

## Voicebot staging plan

Priority: run the deployed voicebot dashboard in `bare` and test its login journey. Implement S1 to S5 in order. Each topic must pass its checks before the next topic starts. A topic can need several small PRs. All topics without a completion note below are planned.

Keep the existing rules: Rust, no external crates, small interfaces, and one-way crate dependencies. Chrome is a reference for tests. It is not the runtime of `bare`.

### Acceptance journey

Target: <https://staging.app.8vox.ai>. The old hostname `staging.voicebot.qilinlab.com` did not resolve on 2026-11-04.

Chrome passed this journey on 2026-11-04:

1. Open the site without a session. The site redirects to login.
2. Enter the staging account credentials and submit the form.
3. Verify the Overview page and its authenticated data.
4. Follow the Agents link. Verify that agent data loads.
5. Search for `Score800` and press Enter. Verify the matching agent and the query in the URL.
6. Reload the page. Verify that the session remains valid.
7. Open the account menu and sign out. Verify that the token is removed.
8. Open Agents without a session. Verify the redirect to login.

The deployed journey uses JavaScript bundles, client-side navigation, authenticated `fetch` requests, and a token in `sessionStorage`. The login offers a remember-me option. Local source uses `localStorage` for that option. Browser restart persistence and remember-me remain untested on staging.

Use a dedicated staging account. Read credentials from the vault at run time. Do not put credentials, tokens or authenticated response bodies in fixtures, traces or reports. The acceptance journey does not edit agents or place calls. Counts and agent names can change. Select a stable test agent when the automated suite is added.

### Work through each topic

Use this cycle for each topic: implement, run huge load tests, then optimize CPU time and RAM use. Complete the cycle before moving to the next topic.

1. Read the current code, tests and commit history. Define the expected behavior. For a bug, add a failing regression test first.
2. Implement the topic in a worktree. Establish correctness with unit and integration checks. Keep the work in small, reviewable PRs.
3. Extend `bare-stress` and `bare-bench`. Run the existing 1x, 10x and 100x scale pattern. Add large inputs for the topic: many small resources, large bodies, deep trees, large scripts, repeated mutations or repeated journeys. Run each stress case in a separate process with time and memory limits. Save failing inputs and fuzz seeds.
4. Measure release CPU time, wall time, throughput, peak heap, retained heap and RSS where relevant. Record exact input sizes, machine, build and seeds. Separate network delay from engine costs. Compare increasing sizes to find work that grows faster than the input.
5. Optimize the measured causes. Change algorithms, data layout and ownership when needed. Remove repeated scans, unnecessary copies, per-item allocations and retained buffers. Avoid keeping state that can be computed cheaply. Browser architecture is open to change.
6. Rerun correctness, load, fuzz and resource checks after each change. Repeat the load-and-optimize cycle until large measured costs are resolved or an explicit tradeoff is recorded. Compare the same inputs before and after. Check both CPU and RAM so a gain in one does not hide a regression in the other.
7. Run `cargo test --workspace`. Record results, decisions and remaining gaps. Merge before starting the next topic.

Chrome and Safari do not define the internal architecture. Use standards, test vectors and Chrome behavior to check the observable behavior that the target application needs. Choose algorithms, storage and scheduling for the measured needs of `bare`. The existing design already uses this freedom: it computes box-tree rules instead of storing a second tree.

Previous changes show the same direction: streaming tokens (`59b058f`), compact DOM nodes (`34d7ca7`), shared DOM buffers (`6e516a8`), load tests and fixes (`6b11b8a`), and streaming HTTP bodies (`2cd8730`). Keep the before-and-after evidence in `resources.md`.

Optimization is part of every module's completion gate. Start with measured behavior, then improve the core design as far as the evidence supports. Never remove validation, certificate checks, origin checks or accessibility to improve a benchmark. Huge load tests must stop at controlled resource limits rather than exhaust the host.

Existing baseline: `cargo test --workspace` passed on 2026-11-04 at `8231fea`. Existing Chrome comparisons and load results are recorded in `resources.md`; those historical runs were not repeated for this plan. Some old gap entries are stale. Check the code before treating them as missing work.

### S1 HTTPS and resource loading

Build on modules 1 and 7. Keep URL and HTTP work in their existing crates. Put TLS and crypto behind a separate crate interface.

The SHA-256, SHA-384, HMAC-SHA256 and HKDF-SHA256 foundations of topic 2 are complete. `bare-tls` has streaming APIs, portable code and a CPU-detected SHA instruction path. The `force-portable` feature runs all crypto vectors through the fallback. AES-128-GCM record protection is also complete. It authenticates ciphertext before decrypting and supports portable and CPU-detected accelerated paths. SHA-384 has a streaming API and fixed memory use for certificate-signature hashes. X25519 key exchange and OS random input are complete. The TLS-facing key-exchange API rejects all-zero shared secrets. Linux uses `getrandom` with flags 0; source errors clear partial output and have no fallback. Topic 2 crypto primitives are implemented; TLS integration is still open. Topic 2 record protection is complete. The bounded record layer enforces record sizes, separate direction sequences and per-key usage limits. It rejects altered ciphertext before returning plaintext. The connected handshake must install keys and enforce message order.

The URL prerequisite of topic 4 is complete: HTTP/HTTPS schemes, default ports, relative links, serialization and origin comparison. Network requests to HTTPS are refused before cache, DNS or socket use. TLS is not connected yet. Cache and connection-pool keys now include the scheme. Keep the guard until the verified TLS transport is connected.

Topic 1 is complete (2026-11-04). See [staging-network.md](staging-network.md) for the observed TLS chain, public resource manifest and repeatable inspection commands. The first HTTPS check fails until the TLS implementation is connected. The observed minimum uses AES-128-GCM, X25519, SHA-256, SHA-384, and ECDSA P-256/P-384 verification. Use an independently trusted root source; do not trust a root only because the server sent it.

| Topic | Work | Check before closing |
| --- | --- | --- |
| 1 | Inspect the staging TLS chain, algorithms, content encodings and resource URLs. Record a public resource manifest and expected failures. | Establish the exact TLS and response-decoding requirements. Do not infer them from Chrome success alone. |
| 2 | Implement the required crypto primitives, secure random input and TLS record protection. | Standard known-answer vectors; altered records fail; malformed and oversized input is bounded. |
| 3 | Implement TLS 1.3 handshake, SNI, certificate parsing, trust chains, hostname, validity and signature checks. | Trusted and untrusted fixtures; wrong host, expired chain and altered handshake fail. No certificate bypass. |
| 4 | Connect the completed HTTPS URL support to HTTP/1.1 over TLS. Include the scheme in cache and connection-pool keys; preserve origin identity across redirects. | Existing HTTP tests still pass; staging HTML downloads with certificate verification. Advertise only protocols implemented. |
| 5 | Add request methods, headers and bodies for API calls. Keep cache, redirect, credential and retry rules correct for each method and origin. | POST is not retried as GET. Redirect and cache tests cover authenticated requests and errors. |
| 6 | Load external scripts and styles. Preserve script async/defer metadata for S3 scheduling. Reject failed responses and invalid script MIME types. Decode the response encodings found in topic 1. Bound resource size, total memory and time. | Download the staging login resources. Compare decoded bytes with the reference manifest. |

Resource checks: handshake cost, throughput, peak memory for large bodies, repeated resources, slow servers and many small resources. Reuse existing streaming and connection code. Avoid a second full copy of each resource. Introduce bounded concurrency when measurements justify it.

Exit: `bare` downloads the real staging login HTML, scripts and CSS over verified HTTPS. Script execution and login are later gates.

Certificate parsing and ECDSA P-256/P-384 verification in S1 topic 3 are complete. `der` and `x509` borrow signed bytes and fields from bounded inputs. Unknown critical extensions are exposed for the validator to reject. Parsing does not verify signatures, trust, a path or a hostname. ECDSA accepts only validated uncompressed public keys and in-range signature scalars. It is variable-time verification code for public data. Hostname checks and OS trust-store loading are complete. Hostname checks use SANs only and support exact DNS names and IP addresses. Wildcard names are unsupported. TLS 1.3 hello encoding, ServerHello parsing, typed key derivation and transcript hashing are complete. The next gate is to connect signature verification, hostname checks and independently loaded OS roots into path validation and the connected handshake. No HTTPS request is enabled by these foundations.

S1 topic 5 request APIs are complete. GET and POST support validated caller headers and borrowed request bodies. POST is never retried. Redirects preserve or rewrite the method and body as required. Cross-origin redirects drop credential headers. Header-bearing requests bypass the cache. Successful POST responses invalidate the cached target. Topic 4 still gates HTTPS transport and staging checks.

### S2 JavaScript and event loop

Build the JavaScript interpreter from module 9. Bring forward the single-page scheduler from module 8. Tabs remain deferred.

| Topic | Work | Check before closing |
| --- | --- | --- |
| 1 | Inventory syntax and built-ins in the deployed bundles. Save versioned public inputs for repeatable tests. | Record bundle hashes and explicit unsupported features. A small sample is not proof of complete compatibility. |
| 2 | Implement tokenizer, parser and executable representation. | Parse all target bundles; test malformed syntax, nesting and source locations. |
| 3 | Implement values, scopes, closures, calls, objects, prototypes and property descriptors. Add managed heap storage and collection. | Language tests; cyclic objects; reachable objects survive collection; released objects do not accumulate. |
| 4 | Implement the language features and built-ins required by the target bundles. | Compare language behavior with Chrome. Parsing alone does not close this topic. |
| 5 | Implement tasks, microtasks, promises, exceptions and timers. Add execution limits and cancellation. | Check task ordering, rejected promises, timer cancellation and termination of an infinite loop. |
| 6 | Add host bindings and diagnostics for the browser APIs in S3. | Run a fixed integration fixture with a small test host. Report unsupported host APIs with source locations. |

Resource checks: parse time and memory per bundle, property access, closure calls, promise chains, timer queues, collection pauses and retained heap after repeated runs. Check strings and executable code for unnecessary copies. Measure before adding caches or alternate representations.

Exit: the interpreter passes the target language tests and scheduler checks. Full React/Next.js startup also requires S3. Do not mark compatibility complete until the real application boots.

### S3 DOM and browser APIs

Extend the existing HTML arena, CSS selector matcher and URL code. Keep browser host state outside the language core.

| Topic | Work | Check before closing |
| --- | --- | --- |
| 1 | Add DOM creation, attributes, text replacement, insertion, removal and queries required by the target. | DOM invariants, node identity, detached nodes and mutation tests agree with Chrome. |
| 2 | Add event dispatch, capture, bubbling, cancellation, focus and form state. | Test event order, delegated listeners, input/change/submit and default actions. |
| 3 | Add script scheduling and document readiness. Connect parser, scheduler and DOM. | Check inline/external script order, dynamic chunks, readiness events and React hydration. |
| 4 | Add `fetch`, request/response objects, JSON and text decoding, cancellation, same-origin policy and required CORS behavior. | Authenticated API fixtures pass. Cross-origin access and redirect tests enforce the intended restrictions. |
| 5 | Add origin-scoped session/local storage, location, history and URL APIs. Add cookies if the runtime audit requires them. | Navigation, reload, back/forward, logout and storage isolation pass. Session storage has the correct lifetime. |
| 6 | Run the actual app and implement remaining APIs that block this journey. Audit observers, animation frames, streams, encoders and browser feature checks. | Record each runtime blocker with a reduced regression fixture. Do not replace framework behavior with app-specific shortcuts. |

Resource checks: DOM mutations, repeated queries, event listener growth, fetch cancellation, navigation teardown and detached-node retention. Release obsolete page resources and JS roots. Bound storage and response buffers.

Exit: the real bundles boot, login runs, authenticated Agents data appears in the DOM, and search/navigation update it through the event system. This is a DOM-level gate. Visible mouse and keyboard operation is the S4 gate.

### S4 Window paint and input

Build on layout and the original modules 5 and 6. Paint into an OS-independent pixel buffer. Keep Wayland behind the window interface described in `decisions.md`.

| Topic | Work | Check before closing |
| --- | --- | --- |
| 1 | Add a software paint pipeline, pixel capture and built-in bitmap text. | Fixed fixtures give repeatable pixels. Painted text uses the same metrics as layout. |
| 2 | Add a Wayland window, buffer lifecycle, resize and frame scheduling. | Open, resize, redraw and close without busy loops, stale buffers or leaks. |
| 3 | Draw backgrounds, borders, text and the clipping and stacking needed by the target. Add required icons and CSS gaps from the staging audit. | Compare reduced fixtures with Chrome. Forms and account menus remain visible and usable. |
| 4 | Add hit testing, pointer events, keyboard input, focus traversal, caret, selection, text editing and password masking. | Pointer and keyboard operate controls. Tab, Shift+Tab, Enter and editing work through normal events. |
| 5 | Add document/container scrolling, navigation and default form actions. | Hit tests respect scroll offsets and clipping. Links, search and menu controls work after scrolling. |
| 6 | Recompute style, layout and paint after DOM, form, viewport and focus changes. | Run the full staging journey by visible input. Repeated changes do not leave stale output. |

Start with correct full-page updates. Measure them before adding invalidation, dirty regions or cached layout. Add real font/image/SVG support when a measured rendering or interaction failure requires it. Record visual differences from the initial bitmap font; exact Chrome pixels are not an initial gate.

Resource checks: frame time, idle CPU, resize allocations, pixel-buffer copies, repeated layout, long-page scrolling and redraw after small changes. Fix large costs while retaining pixel and hit-test correctness.

Exit: a user can complete the staging acceptance journey in the `bare` window with mouse and keyboard. Chrome remains the reference only.

### S5 Test automation

Expose a small local control interface to the same navigation, event, layout and paint paths used by the window. Keep a separate test driver outside the engine.

| Topic | Work | Check before closing |
| --- | --- | --- |
| 1 | Add navigation, element queries, clicks, typing, keys and scrolling. | Actions target visible, enabled elements and use normal hit testing and input dispatch. No direct token injection. |
| 2 | Add labels/roles for the controls used by the journey, condition waits and assertions. | Duplicate, hidden, disabled, covered and detached targets produce useful failures. No fixed sleeps for readiness. |
| 3 | Add pixel capture, console/network diagnostics and machine-readable results. | A deliberate failure produces useful evidence and a nonzero exit status. Secrets remain redacted. |
| 4 | Add a deterministic local app fixture and automate its complete login journey. | Pass and fail paths run without staging availability. Include invalid credentials, expiry and logout. |
| 5 | Run the same journey against voicebot staging using a dedicated account. | Every step in the acceptance journey passes in `bare`, including reload and unauthenticated redirect. |
| 6 | Repeat the journey and measure resource retention and reliability. Add the suite to the normal checks. | Record run count, failures, build SHA and staging version where available. Investigate sustained heap/RSS growth. |

Resource checks: action latency, test duration, repeated journeys, driver overhead and failure-capture size. Keep local fixtures independent of staging. Run live staging checks only when requested or through an explicitly configured job.

Exit: the complete acceptance journey runs unattended in `bare` and produces a clear result with failure evidence. A Chrome pass or an API-only login is not this gate.

### Scope after the first journey

Defer tabs, cross-platform windows, advanced tab isolation and resource management, voice calls, WebRTC, microphone and audio playback. Keep basic execution, allocation and network limits in the first implementation.

Next candidates after the first pass: remember-me across browser restarts, invalid-login UI, session expiry and one controlled staging edit. Each needs its own acceptance steps. Add browser features when those steps expose a requirement.

Start with S1 topic 1: inspect the staging TLS and resource requirements, define repeatable fixtures, and choose the first failing HTTPS check. Do not start all five modules at once.

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
| 4   | Values (colors, lengths, shorthands)                 | done    | `crates/css/src/values.rs`, `crates/css/src/color.rs` |
| 5   | Cascade and computed style, app prints the styles    | done    | `crates/css/src/cascade.rs`, `crates/css/src/ua.css` |

What each done topic covers:

- CSS tokenizer: CSS Syntax Level 3, section 4. Idents, functions, at-keywords, hashes (with the id flag), strings, urls (quoted and unquoted), numbers (integer flag, exponents), percentages, dimensions, comments, `<!--` and `-->`. Escapes (`\41 `), NUL, bad strings and bad urls follow the standard. It is an iterator, and names and strings are slices of the style sheet (`Cow::Borrowed`) unless an escape changes them. On three real style sheets (bootstrap and two GTK themes) it gives no bad token, at about 170 MB/s.

- CSS parser: CSS Syntax Level 3, section 5. Style rules and at-rules, declarations with `!important`, and the error recovery of the standard: a broken declaration is skipped to the next `;`, a block that is not closed keeps its content, a rule with no `{` at the end is dropped, and `;` `}` inside strings, urls and `(...)` `[...]` `{...}` do not end anything. `@media`, `@supports`, `@layer`, `@container` and `@keyframes` hold rules, `@font-face` and `@page` hold declarations, and other blocks are skipped. The prelude and the values stay as slices of the style sheet for the next topics. Blocks inside blocks stop at 32 levels, and a run of 100,000 `(` does not overflow the stack. On bootstrap.min.css the at-rule count matches Chrome's (71 against 69: Chrome drops `@-o-keyframes` and `@-ms-viewport`).

- Selectors: the parser reads type, `*`, id, class, the seven attribute tests (with `i` and `s`), the combinators (space, `>`, `+`, `~`), `:first-child`, `:last-child`, `:only-child`, `:root`, `:empty`, `:link`, `:nth-child()`, `:nth-last-child()`, `:nth-of-type()`, `:nth-last-of-type()` (with `odd`, `even` and `an+b`), `:first-of-type`, `:last-of-type`, and `:not()` with a list. `:hover`, `:focus`, `:active`, `:visited`, `:invalid` and the like are valid and never match. A selector is stored from the right to the left, with names as `Atom`s of the document, and its specificity is one packed `u32`. One bad selector throws away the whole list, as the standard says. The matcher starts at the element, tests the last compound first, and goes up with a loop (a deep tree does not overflow the stack). On two builds of bootstrap.min.css, the selectors that it throws away are vendor-prefixed pseudo-classes (`:-moz-placeholder`, `:-ms-input-placeholder`), as Chrome does. A known pseudo-element (`::before`, `:after`, `::marker`, `::-webkit-…`) at the end of a selector is valid and never matches, so `li::before, p` still styles `p`, as in Chrome. (Until 2026-11-03 bare threw away the whole list. A Wikipedia page showed the bug.)

- Values: a declaration (name and value text) becomes typed items: a `Property` (1 byte), a `Value` (8 bytes: color, length, number, keyword, or `inherit`/`initial`/`unset`) and the `!important` flag, 12 bytes in all, with no `String` and no allocation while reading. 48 properties: display, float, clear, overflow-x, overflow-y, position, top, right, bottom, left, visibility, white-space, text-align, text-decoration-line, text-transform, list-style-type, box-sizing, color, background-color, font-size, font-weight, font-style, line-height, width, height, min/max sizes, margin, padding and border (width, style, color for each side), and, from module 4 topic 11, `border-collapse`, `border-spacing`, `table-layout`, `caption-side` and `vertical-align`. Shorthands are split at once: `margin`, `padding`, `border`, `border-{width,style,color}`, `border-{top,right,bottom,left}`, `background` (color only), `font`, `list-style`, `text-decoration`, `overflow`, `inset`, `border-spacing`. Colors: hex (3, 4, 6, 8 digits), 148 names (a table generated from `data/css/color-name.js`), `transparent`, `rgb()`/`rgba()` and `hsl()`/`hsla()` in the old and the new syntax. Fixed units (`pt`, `in`, `cm`, ...) become `px` at once. `em`, `rem`, `%`, `vw`, `vh`, `vmin` and `vmax` stay for the cascade. A bad value drops the whole declaration. Checked against Chrome (`CSS.supports`) on 2,350 pairs of property and value: 6 differences, all in the loose parts of `background` and `text-decoration` (see the gaps).

- Cascade: `RuleSet::add` reads a style sheet (the browser's `ua.css`, then the page's sheets in order), and `compute` gives the style of every element. The winner for each property is the biggest key of (importance and origin, specificity, order), kept in one `u64`, so nothing is sorted. The `style=""` attribute beats all selectors. Rules are in lists by the id, class or tag at the right end of the selector, so an element tests only a few rules. A counting Bloom filter of the ancestors skips most walks up the tree. Elements with the same parent style and the same matching rules share one `ComputedStyle` (an array of one 8-byte `Value` for each property), and styles with equal values are merged, so an element costs 4 bytes. Computed values: `em`, `rem`, `vw`, `vh`, `vmin`, `vmax` are pixels, `currentcolor` is the color, `bolder` and `lighter` are numbers, `line-height: 150%` is pixels, a border with no style has no width. Inheritance for ten properties, and `inherit`, `initial`, `unset` for all. Elements with `display: none` and what is inside get no style. `sources` lists the `<style>` and `<link rel=stylesheet>` of a page (it skips `media` that is not `all` or `screen`, and alternate sheets). The app has `--styles`: it fetches the linked sheets and prints the tree with the computed style of each element. On a small page, the styles that we print are the same as `getComputedStyle` in Chrome for display, color, font, margin, padding, border and text-decoration.

## Module 4: Layout

| #   | Topic                                                | State   | Code                          |
| --- | ---------------------------------------------------- | ------- | ----------------------------- |
| 1   | Box tree (which box each node makes)                 | done    | `crates/layout/src/boxes.rs`  |
| 2   | Text measurement (font metrics, spaces, words)       | done    | `crates/layout/src/font.rs`, `crates/layout/src/text.rs` |
| 3   | Block layout (widths, heights, margin collapsing)    | done    | `crates/layout/src/block.rs`, `crates/layout/src/fragment.rs` |
| 4   | Inline layout (line boxes, line breaks, baselines)   | done    | `crates/layout/src/inline.rs`, `crates/layout/src/intrinsic.rs` |
| 5   | App (`--layout`), checks against Chrome, load tests  | done    | `crates/bare/src/main.rs`, `crates/bench/src/main.rs` |
| 6   | Floats, `clear`, and blocks that avoid floats        | done    | `crates/layout/src/floats.rs`, `crates/layout/src/block.rs`, `crates/layout/src/inline.rs` |
| 7   | `overflow`: scroll containers and scrollbars         | done    | `crates/layout/src/block.rs`, `crates/layout/src/boxes.rs`, `crates/css/src/values.rs` |
| 8   | `position`: relative, absolute, fixed, sticky        | done    | `crates/layout/src/block.rs`, `crates/layout/src/inline.rs`, `crates/layout/src/fragment.rs` |
| 9   | Flex layout                                          | done    | `crates/layout/src/flex.rs`   |
| 10  | Grid layout (placement, track sizing, alignment, `repeat()`, areas) | done | `crates/layout/src/grid.rs`, `crates/css/src/grid.rs`, `crates/css/src/values.rs`, `crates/css/src/cascade.rs` |
| 11  | Table layout (auto and fixed, spans, collapsed borders) | done | `crates/layout/src/table.rs`, `crates/css/src/hints.rs`, `crates/css/src/ua.css`, `crates/layout/src/block.rs`, `crates/css/src/values.rs` |

What each done topic covers:

- Box tree: `Tree::kind` says what box a node makes: none (`display: none`, comments), block, list item, inline, atomic inline (`inline-block`, `img`, `button`), text, or line break (`<br>`). The root element is always a block. `Tree::children` gives the child boxes of a block: block children, and runs of inline content between them (the anonymous blocks). A run that is only collapsible spaces between blocks makes no box. A block inside an inline (`<span><div>`) stays in the run: inline layout handles it. List numbers follow `<ol start>` and `<li value>`, with one counter for each list. The box tree is not stored: layout calls these functions while it walks the DOM (see `docs/decisions.md`).
- Text measurement: the metrics of Noto Sans Mono (a letter is 0.6 em, ascent 1.069 em and descent 0.293 em, rounded to whole pixels as Chrome does, so `line-height: normal` is 22px at 16px). Widths are whole numbers in tenths of an em: a letter is 6, a wide letter (Chinese, Japanese, Korean, emoji) is 10, an accent and a zero-width space are 0. `text::pieces` cuts the text of a node into words, spaces, newlines and tabs, with the six `white-space` values and `text-transform` (`ß` in uppercase is two letters wide). A run of spaces is one piece, also across text nodes (`Flow`). The text is not copied: a piece is a byte range in the DOM text.
- Block layout: `layout` walks the blocks from the root element, which fills the window. Widths follow CSS 2.1 10.3.3: `auto` width, `auto` margins that center, an over-constrained right margin, `box-sizing`, `min-width` and `max-width`, `%` and `calc()` of the parent's width (padding and vertical margins too). Heights: from the children, or `height`, with `min-height`, `max-height`, and `%` only of a parent with a known height (the root has the window's height). Margin collapsing with a strut (as LayoutNG does): siblings, a parent and its first or last child, empty blocks that margins go through, negative margins. The root, inline-blocks, table cells, flex and grid containers keep their margins apart. The result is a list of fragments in tree order (36 bytes each), with places relative to the parent while layout runs and page places at the end. A list item with no line gets its marker at its top. Layout stops at 400 levels of blocks, as Firefox does.
- Floats: `float: left` and `right` leave the normal flow and go to the side of their block (CSS 2.1, 9.5.1). A float is a block with its own block formatting context (BFC), and shrinks to fit with `width: auto`. Lines next to a float get shorter, and a line that cannot hold its first unit next to the floats goes down below them. A float in the middle of a line goes on that line if it fits next to the text already there, else at the top of the next line. `clear` moves a block (or a float) below the floats of its sides. A block that makes its own BFC (`display: flow-root`, an inline-block, a float, a table, a flex or grid container) does not overlap floats: it gets narrower, or goes down until it fits. A BFC root grows to contain its floats. A test page with 18 float cases gives the same 51 boxes and 39 text lines as Chrome, to 0.01 px. `display: flow-root` is new in the CSS values.
- App: `bare <url> --layout` prints one fragment for each line: the element, its `id`, its page place and its size. `--width=N` sets the window width (default 1280, height 800). A test page with 33 boxes (headings, wrapped spans, centered and right text, inline-blocks, lists, `pre`, `<br>`, margin collapsing, `box-sizing`, `nowrap`, a block in a span, `calc()` and `%`) gives the same places and sizes as Chrome, to 0.01 px. `bare-bench` has a layout step (see `resources.md`).
- Inline layout: `Events` walks the inline content of a run (text, start and end of inline boxes, inline-blocks, `<br>`, blocks inside inlines) with no list in memory. Greedy line breaking at the break opportunities: after a space that wraps, before and after an inline-block, never inside a unit like `a<b>b</b>`. A word that is too long sticks out. A space at the end of a line hangs. Kept newlines, tabs (stops every 8 letters), `<br>`. Line height from half-leading of the block (the strut) and of each inline box on the line, all on one baseline; inline-blocks sit on their last line (or their bottom edge). `text-align` left, right, center. An inline box over two lines makes a fragment on each, with its left edge on the first and its right edge on the last. Inline boxes with nothing to paint, no edges and the same font as their parent make no fragment (culled, as in Chrome). Inline-blocks shrink to fit (`intrinsic.rs`: min-content and max-content widths). A block inside an inline takes lines of its own. The list marker sits on the first line. Lengths stop at 33,554,431 px and font sizes at 10,000 px, as in Chrome.

- Overflow: `overflow`, `overflow-x` and `overflow-y` with `visible`, `hidden`, `clip`, `scroll` and `auto`. If one side scrolls, a `visible` on the other side becomes `auto`, and a `clip` becomes `hidden`. A scroll container (`hidden`, `scroll`, `auto`) makes its own BFC, so it keeps the margins inside and does not overlap floats. `clip` does not. The root element gives its overflow to the window, and so does the first `body` when the root has `visible`: the body is then not a scroll container. Scrollbars are 15 px, as the classic scrollbars of Chrome on Linux. They take room inside the content box. `scroll` always has both. `auto` has one only on a side where the content does not fit, so the box is laid out again with it. Below, a scrollbar makes a box with `height: auto` 15 px taller. Next to the content, a box that shrinks to fit gets 15 px wider. An inline-block that scrolls sits on the bottom of its margin box. A test page with 16 cases gives the same 31 boxes as Chrome, to 0.5 px. Nothing is clipped yet and nothing scrolls: that is paint (module 5) and the app.

- Position: `position` (`static`, `relative`, `absolute`, `fixed`, `sticky`), `top`, `right`, `bottom`, `left` and the `inset` shorthand. `relative` moves a block or an inline box, with all that is inside it, after layout (`left` wins over `right`, `top` over `bottom`). `absolute` and `fixed` boxes leave the flow. They leave a "static position", where they would be with `position: static`: at the place in the line for a box that was inline, at the start of a line for one that was a block. The containing block is the padding box of the nearest positioned block, of a positioned inline box (from its first fragment to its last), or the window. `fixed` always uses the window. The width and the height follow CSS 2.1 10.3.7 and 10.6.4: shrink-to-fit with `auto`, the room between `left` and `right` (or `top` and `bottom`), `auto` margins that center, and percents of the containing block. `float` does nothing on them. A test page with 15 cases gives the same 32 boxes as Chrome, to 0.5 px.
- Flex: `display: flex` and `inline-flex` (CSS Flexbox 1, section 9). The items are the child boxes, and each run of text that is not only spaces (an anonymous item). `order` sorts them. The flex base size comes from `flex-basis`, else `width` or `height`, else the content. The automatic minimum size (`min-width: auto`, now the initial value) keeps an item from shrinking below its longest word, except in a scroll container. `flex-wrap` (`wrap`, `wrap-reverse`) makes lines, with `gap`, `row-gap` and `column-gap`. On each line, `flex-grow` shares the free space, and `flex-shrink` times the base size shares the space that is missing; an item that hits its min or max size is frozen, and the others share again. `flex-direction` with `row`, `column` and the reverse ones. `justify-content` (`flex-start`, `flex-end`, `start`, `end`, `left`, `right`, `center`, `space-between`, `space-around`, `space-evenly`) and `margin: auto` on the main axis. `align-items` and `align-self` (`stretch`, `flex-start`, `flex-end`, `start`, `end`, `self-start`, `self-end`, `center`, `baseline`) on the cross axis, and `align-content` for the lines. A single-line container with a known cross size gives it to its line. A column with `height: auto` grows into its `min-height`. The container is as wide as its items side by side (a row) or as its widest item (a column, or the min of a wrapped row) when it shrinks to fit. Its baseline is the first item of the first line, or the first one aligned by baseline. Two test pages with 65 cases give the same 217 boxes as Chrome, to 0.5 px.

- Grid: `display: grid` and `inline-grid` (CSS Grid Layout 1). The items are the child boxes, and each run of text that is not only spaces (an anonymous item). `grid-template-columns` and `-rows` take lengths, `%`, `fr`, `auto`, `min-content`, `max-content`, `minmax()`, `fit-content()`, `repeat(<int> | auto-fill | auto-fit, ...)` and line names (`[a b]`, kept). `grid-template-areas`, `grid-auto-columns`, `grid-auto-rows`, `grid-auto-flow` (`row`, `column`, `dense`), `grid-row-start`, `-end`, `grid-column-start`, `-end` (integers, negative ones, `span n`, names) and the shorthands `grid-row`, `grid-column`, `grid-area`, `place-items`, `place-self`, `place-content`. Placement follows section 8.5: items with both places, then items with a row, then the rest with a cursor, sparse or dense, with implicit tracks on both sides of the explicit grid. Track sizes follow section 11: the items of one track, then the items that span more tracks by their span, then the flexible tracks, then the stretch of the `auto` tracks. `min-width: auto` gives an item its automatic minimum size. `justify-content` and `align-content` (`start`, `end`, `center`, `space-*`, `stretch`), `justify-items`, `align-items`, `justify-self` and `align-self` (`stretch`, `start`, `end`, `center`), `auto` margins, `order`, gaps (`%` too), and an `auto-fit` track with no item collapses. A grid with no height is as high as its rows, and a `min-height` gives its rows free space. The intrinsic widths of a container are the sums of its tracks. Absolute children are not items. The first check against Chrome was 109 boxes on two pages, 107 within 0.5 px (`docs/resources.md`).
- Tables: `display: table`, `inline-table`, `table-row-group`, `table-header-group`, `table-footer-group`, `table-row`, `table-cell`, `table-caption` and `table-column(-group)`, with `border-collapse`, `border-spacing` (one or two lengths, inherited), `table-layout`, `caption-side` and `vertical-align` in cells (`baseline`, `top`, `middle`, `bottom`; the other values act as `baseline`, and inline layout ignores the property). `colspan` and `rowspan` come from the attributes. A table lays itself out in one place (`crates/layout/src/table.rs`) and keeps no state: it reads the children, builds the grid in a few `Vec`s, and gives the fragments. The captions go above or below as blocks as wide as the table box. A row group with `thead` goes first and `tfoot` last. Cells, rows and groups that the page leaves out are made as in CSS 2.1 17.2.1 for the common cases: loose cells and rows in a table (or in a `display: table` box) get an anonymous row, an anonymous group, or both, and they make no fragment. Auto layout follows the algorithm of Chrome (TablesNG) and of Firefox: each column has a least width, a most width, a percent and a fixed width, and the spanning cells are spread after the cells of one column. A table width of `auto` is the most width, but not more than the room, and not less than the least width. The extra width goes to the auto columns in proportion to their most width, then to the fixed columns, then to the percent columns. `table-layout: fixed` uses the first row only. Each cell is laid out once. Row heights come from the cells with `rowspan` 1, the extra of a tall cell and the extra of the table `height` are shared in proportion to the heights of the rows, and then the cells are stretched and their content moves for `vertical-align`, with no second layout. The baseline of the table is the baseline of its first row. With `border-collapse: collapse`, the borders at each edge become one (the widest wins, `hidden` removes it) and each cell and the table get half of it, as a half pixel when the width is odd, as Chrome does. The padding of the table is not used. Presentational hints (`crates/css/src/hints.rs`): `width`, `height`, `cellspacing`, `cellpadding`, `border`, `align`, `valign`, `nowrap` on tables and cells, `align` and `valign` on rows and groups, `width` on `col`, and `width` and `height` on `img` (only in a page that has a `<table>`). They are a declaration with the lowest priority of the author origin, so any rule of the page wins over them and they win over the browser's sheet. Checked against Chrome: three test pages with 95 boxes (auto, fixed and percent widths, spans, captions, groups, collapsed borders, nested and inline tables, anonymous cells, hint attributes) are all within 0.5 px; Hacker News is 290 of 329 boxes. Details in `resources.md`.

Real pages (2026-11-03): six sites, laid out by bare and by Chrome at the same width with one font forced on both. Elements with no unbuilt feature in them, above them or beside them match Chrome in size: 100% on example.com and on info.cern.ch with a doctype, 96% on a Wikipedia article. The other 4% on Wikipedia come from `::before` text that bare does not make. Everything else differs because of the gaps below (flex, tables, floats, quirks mode). The check found two style bugs, both fixed: a selector list with `::before` was thrown away, and `<input type=hidden>` was shown. Details: `resources.md`.

Known gaps of module 4:

- Position: `sticky` is laid out as `relative` with no offset: nothing scrolls yet. An absolute box does not count for the scrollbars of an `overflow: auto` box. A block inside a relative inline box does not move with it. The static position of a box that was a block does not follow `text-align`. No `z-index` (paint), no `transform` (which makes a containing block too), no right-to-left. Places are `f32`. Chrome rounds them to 1/64 of a pixel (`LayoutUnit`), so a long sum can differ in the last digit. Content deeper than 400 blocks is not laid out.
- Inline: no `text-align: justify` (it is left), `vertical-align` is read in table cells only (all inline content is on the baseline), no `word-break`, `overflow-wrap` or hyphens, no `::first-line`, no bidi (right-to-left text). The marker of a list item sits on the first line of the first run of lines in the item, also if that run is deep inside it; it does not add to the line height. A block inside an inline gets its margins, but they do not collapse with the lines around it. The intrinsic widths count percent sizes as 0.
- Floats: a line asks for the room next to the floats with the height of the block's own line (the strut). A taller line (a big font, an inline-block) is not checked again lower down, so it can overlap a float that starts inside it. A BFC root next to floats is checked at its top only, not over its whole height. A float in an empty block that collapses through is placed where its first line would be, before the margins below are known. No `shape-outside`. `<br clear>` and the `align` attribute of `<img>` are not read.
- Overflow: the window has no scrollbar: the app gives the width. The scrollbars are always 15 px (macOS and phones use overlay scrollbars that take no room). An `auto` box finds that it overflows from the content height and the right edge of the boxes inside, not from the full scrollable overflow (a relative box or a negative margin is not counted). In the second layout of an `auto` box, the `auto` boxes inside keep what they found in the first, also if their width changed. A box that shrinks to fit gets wider for an `auto` scrollbar on the right only, and the intrinsic widths do not count it. `overflow` on an inline box does nothing, as in Chrome.
- Flex: the static position of an absolute child is the corner of the content box: `justify-content` and `align-self` do not move it. `last baseline` is `flex-end`, and `baseline` in a column is `flex-start`. `safe` and `unsafe` are read and ignored. No `aspect-ratio`, no `visibility: collapse`. The width of a container that shrinks to fit adds the items as they are, without their `flex-basis`. A column that wraps with `height: auto` breaks its lines at `max-height` only. A percent `min-height` or `max-height` of the container does not limit its single line. An anonymous item in a column that wraps is as wide as the container.
- Grid: `baseline` is `start` and `last baseline` is `end`. No subgrid, no masonry, no `grid` and `grid-template` shorthands, no `calc()` in a track size, no writing modes. `safe` and `unsafe` are read and ignored (centring overflows on both sides). `span name` counts as `span 1` if the name is not found. The static position of an absolute child is the corner of the content box, and its grid lines are ignored. A percent track in a grid with no height is `auto`. A percent `row-gap` in a grid with no height is 0: Chrome gives it the percent of the final height, after the rows are sized (2.2 px of difference in 2 of 109 boxes). `max-height` does not limit the free space of the rows. An anonymous text item ignores `justify-items`. A stretched item that holds boxes is laid out a second time: a chain of stretched items inside each other pays one more layout for each level. A grid has at most 10,000 tracks on each side.
- Not built at all: clipping and scrolling; multi-column layout; quirks mode (a page with no doctype is laid out in standards mode); generated content (`::before`, `::after`, `content`); form controls and images (an `img` or `input` is an empty inline-block, and `<input type=hidden>` is hidden); `writing-mode` and vertical text; `transform`.
- Tables: no wrapper table for loose cells inside a block that is not a table (a `td` in a `div` is laid out as a block). `rowspan="0"` is read as 1. `visibility: collapse` and `empty-cells` are not built (the last is for paint). Collapsed borders read the cells, the table, the rows and the groups, but not the columns, and a row group border does not reach the cells. A percent `height` of a cell is not resolved. Anonymous cells do not collapse the margins of the blocks inside. A table next to a float keeps the width that the float gave it. A table with more than 16,384 columns, a `colspan` over 1,000 and a `rowspan` over 65,534 are cut. A `col` has no fragment. A table with more than 2,000,000 edges in the collapsed model uses the borders of its cells. `-webkit-center` (`<center>` around a table) is not built, so the table is not centered: use `margin: auto` or `align=center`. A block in an inline (`<a><div></div></a>`) makes a line too short; the Hacker News page differs by 6 px at the end for this (the cause is in inline layout, not in tables). An `img` has no size from its file.
- Text: one monospace font for all text. No `font-family`, no shaping, no kerning. Bold and italic have the same width. The wide letters are a short list of Unicode blocks, not the full East Asian Width table. Spacing marks (Unicode category Mc) are 0 wide, and they should not be. No `word-spacing`, `letter-spacing` or `tab-size` (a tab is 8 spaces). A letter is 0.6 em. In the font file it is 1229/2048 em (0.60009), so Chrome puts 80 letters at 16 px 0.13 px further right than we do. No break after a zero-width space, and no breaks between Chinese letters (Unicode line breaking, UAX #14).

## Load tests (modules 1 to 3)

`crates/stress` has the scaling cases, the fuzzer and a bad HTTP server. Results and fixes are in [resources.md](resources.md#load-tests). Run it again after a big change to a parser.

## Streaming HTML

`encoding::Decoder` and `tree::Parser` read a page in pieces and give the same tree as the whole page. Details and checks: [resources.md](resources.md#html-in-pieces). `bare` uses them with `Client::get_stream`: the page is parsed while its bytes arrive.

## Known gaps

- HTTP: `Transfer-Encoding` codes other than `chunked` (for example `gzip`) are not supported. No `Accept-Encoding`, so no compression.
- HTTP: HTTP/2 and HTTP/3 are not built. They move to after module 7, because browsers use them only over TLS. HTTP/3 also needs QUIC.
- HTTP: one idle connection for each server. No pool of several at once, no parallel requests.
- HTTP cache: no `Expires`, no heuristic freshness (both need HTTP date parsing), and no 301 caching.
- HTTP cache: a reply with a `Vary` header is not stored. No disk cache. No size limit. Module 10 needs a limit for memory accounting.
- HTTP: GET and POST redirects work. The cache is not split by top-level site. Other request methods are unsupported.
- URL: HTTP and HTTPS parse. HTTPS network access remains refused until the verified TLS transport is connected.
- DNS: no short names from the `search` line of `/etc/resolv.conf`.
- DNS: no DNS-over-HTTPS. It needs module 7.
- Encoding: no UTF-16 and no other encodings. The meta scan is simple: it does not skip comments. The guess is not a real detector.
- Tokenizer: the doctype's public and system names are skipped. Quirks mode needs them (modules 3 and 4).
- Tokenizer: no script data escapes (`<!--<script>` inside a script). `<noscript>` is read as a normal element. NUL characters in text are dropped.
- Tokenizer: it switches to raw text by itself. In a real browser the tree builder does it.
- DOM: a node can be moved (`move_to`, `move_children`), taken out (`detach`) and copied (`clone_element`), but not deleted: the memory of a detached node stays. A `NodeId` from another document is not detected.
- Tree builder: the adoption agency algorithm and the list of active formatting elements work, with Noah's Ark and markers for cells, `applet`, `marquee` and `object`. 28 misnested pages give the same tree as Chrome. The tree builder is about 5% slower on a page with no formatting tags. Not done: foreign content (SVG, MathML) in these rules, and the table insertion modes (a table is read from the stack, so some foster parenting cases differ).
- Tree builder: no foreign content (SVG and MathML), `<template>`, `<select>` modes, framesets, `<noscript>`, quirks mode, and no merge of the attributes of a second `<html>` or `<body>` tag.
- CSS parser: no CSS nesting (a rule inside a style rule is skipped). At-rules inside a style rule are skipped. Nothing checks that a property or an at-rule name is known: that is for the later topics. The style rule count is a little higher than Chrome's (1335 against 1326 on bootstrap) because Chrome also drops rules with a selector that it does not know. Topic 3 should close this gap.
- Selectors: `:is()` and `:where()` take compounds only (no `:is(.a .b)`), and one bad argument drops the rule. No `:has()`, `:only-of-type`, `:checked`, `:disabled` and the other state pseudo-classes. No namespaces. Pseudo-elements (`::before`, `::after`, `::placeholder`) never match, so there is no generated content. The matcher has no ancestor filter yet (topic 5 adds it with the rule index), so a long descendant selector walks all the ancestors. `:nth-child` counts the siblings for each element: a parent with thousands of children and an `:nth-child` rule is slow.
- Tree builder: a stray end tag (like `</div>` with no `div` open) still walks the whole stack of open elements, so a page with a very deep stack and many stray end tags is slow. Chrome stops this by a depth limit of 512. Only `<p>` has a counter now.
- Values: `var()` and custom properties work (`var.rs`), but not `@property`, and the text is pasted without tokens (`var(--a)px` works here and not in Chrome). No `env()`, `min()`, `max()`, `clamp()` (`calc()` works, but a negative result is not set to 0, and a single-unit negative one drops the declaration, and `x / 0` is dropped): Chrome accepts them and we drop the declaration. No `hwb()`, `lab()`, `lch()`, `oklab()`, `color()`, `color-mix()`. No `font-family`, `background-image`, `border-radius`, `box-shadow`, `opacity`, `z-index`. `ex` and `ch` are half an `em`. `background` and `text-decoration` keep only the color and the line, and their check is loose: `background: 1px 2px 3px 4px` and `text-decoration: url(a.png)` pass here and fail in Chrome, and `background: border-box` fails here and passes there. `text-decoration: underline overline` keeps only `underline`. `list-style-type` takes five words only. `border-*-width` with a style of `none` or `hidden` must be 0 when the style is computed (topic 5).
- Cascade: `@media`, `@supports` and `@layer` blocks add their rules when they match (`at_rules.rs`), and `bare --styles` reads `@import` (8 levels deep, a cycle stops). `@layer` order works: nested and unnamed layers, `@import ... layer(x)`, and `!important` that turns the order around (20 cases agree with Chrome). A `@layer a, b;` statement in a sheet comes after the layers of that sheet's imports. `@container` is skipped until layout. `@font-face` and `@keyframes` are not used. The `media` of `<link>` and `<style>` uses the same evaluator. No `var()`. No `:visited` or `:hover` (they never match). No user style sheet. No change of the DOM or the style after the first run: `compute` runs on the whole tree. `larger` and `smaller` use 1.2 (Chrome uses a table). The style of a text node is the style of its parent: layout reads it there. The inherited `border-color` is the color that the parent computed, and not the keyword `currentcolor`.
- Cascade: a rule set with many rules for one class, id or tag makes each element test all of them (the ancestor filter helps for rules with an ancestor part, and nothing helps for a rule with one part). A tree with 50,000 levels is fine, but `--styles` prints a line indented by 2 spaces for each level, so its output grows with the square of the depth.
- App: `--styles` fetches `<link>` sheets one after the other, with no cache and no time limit other than the TCP ones, and it reads them with the encoding rules of HTML (a `@charset` rule is not read).
- CSS tokenizer: the caller gives text. A style sheet that is not UTF-8 needs the encoding rules of the CSS standard (`@charset`, BOM, the `Content-Type` header). The tokenizer does not preprocess the input: it reads `\r`, `\r\n` and `\x0C` as newlines where the standard needs it.
- Tree builder: no parser pause for scripts (`document.write`), and no preload scanner. Both need module 9 and the network in the loop.
- Load limits: a reply head may be 256 KB, a reply 128 MB and 120 seconds, and the HTTP cache 64 MB (it empties when full). The Noah's Ark check reads 256 entries only.
