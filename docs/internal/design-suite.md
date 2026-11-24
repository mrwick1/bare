# Design: suite reuse and on-demand style (steps 5 to 7)

Status: proposal. The owner reviews this document before any code is built.
Written on 2026-11-13 against main `a7ba0525`. Machine: archMachine (16 cores, 31 GiB, shared with up to 5 other agents).
This document covers steps 5, 6 and 7 of `perf-goal.md`. It does not change code.
I ran read-only measurements with a release build of `a7ba052586` (`base-build origin/main`). The scripts and logs are in `/data/bare-tmp/design/suite/` (not in the repository): `server.py` (fixture), `gen_steps.py`, `run1.sh`, `par.sh`, and the logs `results1.log` to `results4.log`, `results3b.log`.
Web access worked. Outside claims have a URL. The claims I did not check are in section 7.

## 0. Summary

Success criterion 7 asks for a suite of related tests with a shared login to run 2 times faster or more than sequential cold runs. Per-page work cannot give this (`perf-goal.md`, "What the profiles showed"). This document says what gives it, and what does not.

| Step | Idea | Measured or estimated gain | Verdict |
| --- | --- | --- | --- |
| 5 | Fresh documents in a warm process | Measured: 1.12 times fewer instructions on a 7-document test (10 tests in one process: 569 M per test against 636 M in a new process). | Build. Most of it exists. Add a CSS parse cache and fix the MCP `open`, which drops the warm network client (section 2.2). |
| 5 | Disk cache of immutable parse results | Estimate: at most 1.0% of a 100-test suite that runs in warm workers. At most 11% when every test is a new process. | Do not build now. The gate is in section 2.5. |
| 6 | Style on demand | Estimate: 1.02 to 1.05 times on the 48 journeys. 12 of 48 journeys need no geometry. Layout is the part that the driver can skip. | Build the cheap part (a styles-only frame). Count before the rest. Do not build partial layout. |
| 7 | Setup reuse across related tests | Measured: 1.23 times fewer instructions on the fixture (the login is 12% of a test). Measured wall time with a 300 ms server login: 4.1 times on one worker (estimate for V0, section 4.4). Estimate: 1.8 to 2.7 times in instructions when the login is half to 70% of a test. | Build. It is the largest CPU lever and it exists in `bare-mcp` and `bare-drive`. |
| 7 | Bounded parallel workers (a process pool) | Measured: 2.0, 3.7, 6.1 and 7.6 times less wall time at K = 2, 4, 8 and 16 (32 tests, against the best K = 1 run). Section 4.4. | Build. This is what meets criterion 7. |

Three findings change the plan:

1. **The suite gain is mostly wall time, not instructions.** Warm process plus shared login removes 19 to 20% of the instructions of the fixture suite. Workers remove wall time, not instructions. A slow server login makes setup reuse alone cut wall time 4 times. Criterion 7 says "wall time", so workers or a slow login alone can meet it. That says little about the engine. I ask the owner to add an instruction target (open question 1).
2. **The cost of a test is its documents.** A test of 7 documents costs 7 times 75 M instructions. Of the 75 M, about 34 M is the JS walk of a 300 KB bundle, and about 31 M is the first style and layout (measured). Lazy JS (`design-js.md` P1) and the sheet registry cut the same cost. Reuse removes the documents that the setup visits; it does not make a document cheaper.
3. **Only the driver asks for style and layout in the places that step 6 can change.** A headless `open` renders nothing (measured: 117 M with no locator step, 148 M with one). The 6 journeys with no locator step never render for the driver. Style that the page script forces (`getComputedStyle`, `offsetWidth`) is not on demand today and is not in the scope of the driver (section 3.4).

## 1. How to read this document

Rules for the numbers (as in `design-style.md`):

- "Measured" means I ran it. The command or the file is named.
- "Estimate" means I computed it from measured numbers. The method is next to it.
- "Hypothesis" means I believe it and did not check it.
- Instructions are `perf stat -e instructions:u` of one `bare-drive` process. Wall time on this machine is weak: the load average was 10 to 44 during my runs. I give wall time for the shape, not for the claim.
- Each fixture row ran 3 times. Instruction counts repeat to 0.5% except where I say so. Peak RSS is `VmHWM` from the driver report.

### 1.1 The fixture used for the measurements

A prototype server in `/data/bare-tmp/design/suite/server.py` (Python standard library, loopback only). It has a login form (`POST /login` sets an `HttpOnly` cookie `sid` and redirects to `/home`), 8 pages behind the cookie, a JSON API behind the cookie, and two shared assets: `app.css` (60 KB, about 800 rules) and `app.js` (300 KB, about 1,900 functions that never run, and a small `boot` that fetches the API and fills a list). The assets send `Cache-Control: public, max-age=31536000, immutable`. Each page has a 60-row table.

One test is `gen_steps.py`: log in (or restore a state file), then click through 5 pages. For each page it checks the heading and the list count (the count needs the `fetch` and the script). A test makes 7 documents when it logs in (login page, home, 5 pages) and 6 when it restores a state.

A fixture pitfall: the first version of the server wrote the head and the body in two sends. Nagle's algorithm and the delayed ACK of the client gave a 41 ms wait on each document, so wall time was several times the CPU time. The fixed server writes one buffer. The fixture app of the build must do the same, or its wall time measures the kernel (section 5.1).

### 1.2 Measured facts used here

All rows: release `bare-drive` `a7ba052586`, the fixture of section 1.1, 3 runs. "M" is millions of instructions.

| Fact | Value | Source file |
| --- | --- | --- |
| One test, new process, login each (`cold1`) | 636 M (runs: 607, 636, 638; the first run 607 is the outlier); wall 190 to 220 ms; peak RSS 24.8 to 25.2 MiB | `results1.log` |
| One test, new process, state restored (`reuse1`) | 560 to 562 M; peak RSS 24.1 to 25.1 MiB | `results1.log` |
| The login setup alone (`login`: open, type, type, click, save state) | 224 M | `results1.log` |
| 10 tests in one process, login each (`cold10`) | 5,669 to 5,693 M: 567 to 569 M per test; 1,720 to 1,774 ms | `results1.log` |
| 10 tests in one process, state restored (`reuse10`) | 4,938 to 4,947 M: 494 M per test | `results1.log` |
| `open` of one page, restored state, wait for the list (300 KB JS): 1, 2, 5, 10 documents in one process | 150, 225, 451, 824 to 827 M. A document after the first costs 75 M. | `results1.log` |
| The same with a 1 KB script and a 1 KB sheet | 17, 33, 79, 157 M. A document costs 16 to 17 M. | `results1.log` |
| The same with a 600 KB script | 254, 362, 684, 1,219 M. A document costs 107 to 108 M. | `results1.log` |
| One document, no locator step (`r0`), then with `expect css=h1 visible` (`r1`) | 117 M, then 148 M | `results2.log` |
| One document then a click to the next, one page (`r3`) | 196 M | `results2.log` |
| Peak RSS: 1 document, 10 documents in one process | 22.8 MiB, 24.4 MiB | `results1.log` |
| Cost of the first document over a later one, 300 KB script | 150 - 75 = 75 M. 71 M of it is JS parse: (254 - 108) - (150 - 75) = 71 M for 306 KB more script, so about 230 instructions per byte | derived from the rows above |
| Of a later document (75 M) at 300 KB script | about 34 M is the per-realm JS work (110 instructions per byte, from the 600 KB row: 33 M for 306 KB more), about 31 M is style and layout (`r1` minus `r0`), the rest (about 10 M) is HTML, network and the driver | derived, estimate |

Facts from the earlier reports (`design-style.md` section 1.1, `design-js.md` section 2.1): style and cascade 17.7% of journey instructions, layout 6.9%, paint 4.7%, CSS parse 8.9%, JS parse 6.5%, realm creation 0.5%.

## 2. Step 5: fresh documents in a warm process

### 2.1 The current code path

| Where | What happens |
| --- | --- |
| `crates/drive/src/run.rs:253` `Session::new` | Makes one `bare_net::http::Client` for the run. Every `open` of the run uses it. |
| `run.rs:106` `Host::open` | The old page ends first (`*self = Host::None`). Then `Page::open_in_with` makes a new page with empty storage, or with the state of `state=`. |
| `crates/bare/src/page.rs:362` `config_in` | `net.fork()`. The fork shares the idle connections, the HTTP/2 sessions, the reply cache, the root certificates and the worker threads (`crates/net/src/http.rs:379`). |
| `crates/bare/src/browser_session.rs:433` | The client of the new document takes the cookie jar of the new storage (`use_jar`). Cookies do not cross a document that has other storage. |
| `crates/js/src/parse_cache.rs:73` | A thread-local cache of parse results. Key: hash of source bytes and grammar. 32 MiB (`MAX_BYTES`, line 26). Sources under 512 B are not kept. An entry that the last 2 realms did not use is dropped (`MAX_IDLE`, line 30). |
| `crates/net/src/http.rs:60`, `:104` | The reply cache: 64 MiB. When a reply does not fit, **the whole cache is emptied**. The key is scheme, host, port and path. Only a GET with no caller header uses it (`:273`). |
| `crates/mcp/src/main.rs:185` `start` | Calls `close`, then `Session::new` (so a new `Client`, a new cache, new connections) for every `open`. |

### 2.2 What repeats

Per document, in a warm process (fixture, measured or derived in section 1.2):

| Work | Per document | Shared across documents today? |
| --- | --- | --- |
| JS parse | 230 instructions per byte (71 M for 300 KB) | Yes, in the thread-local parse cache. A hit costs nothing in the second document. |
| JS first walk, function objects, regex compile | 110 instructions per byte (34 M for 300 KB) | No. Each realm walks the tree again. `design-js.md` P1 (lazy functions, shared regex) targets this. The lazy JS agent builds it. |
| CSS parse | about 182 instructions per byte (`design-style.md` section 3.4): 11 M for the 60 KB sheet (estimate) | No. Every document tokenizes and parses the same sheet text again. |
| Style and layout of the first frame | 31 M (measured) | No. It depends on the DOM. It is not reusable. |
| HTTP: connection, reply cache | one request for each asset without the cache; zero with it | Yes in a step file (`Session::net`). **No in `bare-mcp`**: `start` makes a new client for each `open`, so the connections and the reply cache of the old `open` are gone. |
| TLS handshake, root certificate load | not measured (the fixture is plain HTTP) | The roots and the connections are shared in a step file. In `bare-mcp` each `open` loads them again (hypothesis, to verify with an https fixture). |
| Realm creation, builtins | 0.5% of journey instructions | No. It is too small to matter. |

Two facts from the table:

1. In a step file, the warm process already exists. The measured gain of `cold10` over `cold1` is 67 M per test (636 to 569 M, 10.5%). That is the first parse of the bundle (71 M). A test of 7 documents already hits the parse cache for 6 of its 7 documents. So the gain from a warm process is the parse of the first document, once for each test. A single-page app test (1 document) gains more.
2. In `bare-mcp`, an agent loop that calls `open` for each test starts with a cold network client each time. The parse cache survives because it lives in the thread, but connections, TLS and cached assets do not. This is a code fact (`main.rs:185`, `run.rs:253`). The size of the loss is a hypothesis: not measured, because the fixture has no TLS.

### 2.3 Designs

**A. A warm session with fresh documents (extend what exists).** Keep one process. Per `open`, drop what the document owns (page, realm, storage, timers, its request ids). Keep what is immutable and keyed by content. Add three things:

- A1. `bare-mcp`: keep one `bare_net::http::Client` for the server and give it to every `Session` (`Session::with_client`). `open` forks it, as a step file does.
- A2. A CSS parse cache next to the JS one: `HashMap<Digest, Rc<ParsedSheet>>`. The key is the hash of the flattened sheet text (after `absolute_urls`, `sheets.rs:456`), so a different base URL gives a different key. A hit also compares the bytes. Same limits as the JS cache (a byte cap, an idle age). Thread-local, as the JS one.
- A3. Count hits and misses of both caches per `open` (the replay agent adds "cache hits"; this adds the CSS ones).

Data structures: a hash map keyed by a 64-bit hash with byte verification; values are `Rc` trees that hold no heap handles. Complexity: O(bytes) to hash, and O(1) per hit. Memory: bounded by the cap. Time: removes the CSS parse of every document after the first (estimate 11 M of 75 M, 15%, on the fixture).

**B. A pool of ready pages.** Keep a blank page with its realm built, and give it to the next `open`. Realm creation is 0.5% of instructions. Rejected: it removes nothing that matters.

**C. A fork server (a zygote).** A warm parent process loads the roots, parses the app bundle once (the parse cache holds it), and then forks one child for each test. The child has the warm caches (copy on write) and a clean process: cookies, storage, timers, sockets and memory growth are the process's own, and the OS removes them at exit. Chromium forks its renderers from a zygote for the same reasons: shared pages, and one-time initialization paid once ([zygote.md](https://chromium.googlesource.com/chromium/src/+/main/docs/linux/zygote.md), which gives about 6 MB and 60 ms per GHz saved per process for relocations alone, and says forking after costly initialization lets processes share memory pages).
- Constraints: `fork` keeps only the calling thread, so the parent must fork before it starts the network worker threads, or it must start them again in the child. Open sockets go to the child: the child must close the idle connections it inherits (a TLS session is not safe to share: two processes would use one session key and one sequence number). `fork` is not on Windows. The core must stay OS-independent (project rule), so the fork code lives in the driver, behind a platform switch, and the fallback is design A.
- Gain: each test starts with the first-document cost paid (71 M of parse, estimate) and with no process start. About the same CPU as design A. Its value is isolation and a lower peak of memory for K workers (shared pages count once in PSS).

**D. A disk cache of immutable parse results** (the second part of step 5). Content-addressed files `<digest>.<format version>`. A value is a compact serialized parse result (function spans and scope data, not a tree of pointers). Verify the digest on read. Atomic write (temp file and rename), a byte cap, corruption means a miss. `design-review.md` section 3 gives the rules: URL is only a fetch key, content is the identity, the artifact never gives permission to run a script, cache-off is the oracle.

**Table: step 5 designs.**

| | A. Warm session + CSS cache | C. Fork server | D. Disk cache |
| --- | --- | --- | --- |
| Removes | CSS parse of later documents; the cold network client in MCP | the parse of the first document in each test; process start | the parse of the first document of each new process |
| Fixture gain (estimate) | 11 M of 75 M for each later document (15%) | 71 M of 636 M for each test (11%) | the same 71 M, only for a process that starts cold |
| New code | small: one shared client, one more cache | medium: a fork server, platform switch | medium: serialization, versioning, cap, recovery |
| Isolation | document-level (see 2.4) | process-level (strongest) | none needed (immutable) |
| Memory | one cap (32 MiB) | K children share the parent's pages | disk, not RSS |
| Risk | state that leaks between documents (2.4) | threads, sockets, Windows | stale or corrupt artifacts; no gain if the cache is warm already |

### 2.4 Isolation and correctness in design A

| Item | Today | Risk | Rule |
| --- | --- | --- | --- |
| Cookies | The client of a document uses the jar of its storage (`browser_session.rs:433`). A new `open` has a new jar. Measured on the fixture (`iso.steps`): after a login, `open` with no state lands on `/login`; `open` with the saved state lands on `/home` and the jar holds `sid`. All steps passed. The staging journey step 8 and `crates/drive/tests/fixture_journey.rs` check the same. | Low. | Keep the tests. Add a check in the benchmark: the server counts login POSTs (section 5.1). |
| `localStorage`, `sessionStorage` | New `WebStorage` for each `open` (`page.rs:362`). | Low. | None. |
| Timers and tasks | Belong to the page. The page is dropped before the new one starts (`run.rs:120`). | Low. | None. |
| Sockets and requests in flight | The worker threads and the idle pool are shared (`http.rs:379`). | A late reply of a request of the old document must not reach the new one (hypothesis: the request ids prevent it; to verify with a test: a slow `fetch`, then `open`). | Add the test before A1. |
| **The reply cache** | One cache for the run. Key: scheme, host, port, path. A response with `Cache-Control: max-age` and no `Vary` is stored. The jar of the request is not in the key. | **Medium.** A page of user 1 with `max-age=60` and no `private` can answer user 2 after a state change. Chrome keeps one cache per profile. The reply cache is per run today, but a run can switch state with `open ... state=`. | Key a stored reply by jar id unless it says `public` or `immutable`. Static bundles keep sharing. A private page does not. |
| The reply cache cliff | When a reply does not fit in 64 MiB the whole cache is emptied (`http.rs:124`). | A suite with a big image can empty it. Low for the fixture. | Out of scope. Mention: evict the oldest, not all. |
| Parse caches | Content-keyed and immutable. | A hash collision gives a wrong tree. | The JS cache compares bytes on a hit. The CSS cache does the same. |
| Memory growth across documents | Peak RSS in one process, 1 / 10 / 20 / 50 documents: 22.8 / 24.4 / 24.4 / 24.8 MiB (measured, one run each for 20 and 50). From 10 to 50 documents: +0.5 MiB, about 12 KiB per document. A later document costs 75 M instructions at every count (824 M at 10, 1,579 M at 20, 3,835 M at 50). `open_memory.rs` checks one case. | A leak of 0.1 MiB per document is 10 MiB per 100. | The gate is 0.1 MiB per document. Fixture result: 0.012 MiB. A real app can leak more; the benchmark reports it. |

### 2.5 The gate for the disk cache

Build D only if all of these hold, measured on the suite of section 5 and on the replay corpus of step 1:

1. The suite runs in processes that start cold, or workers that each parse the same bundle (K parses for K workers).
2. Misses cost at least 5% of the suite instructions. The counters come from A3.
3. The second run of the same suite, with the cache on, saves at least that much and still passes with the cache off.

Arithmetic for the fixture (estimate). A suite of 100 tests, K = 8 warm workers: each worker parses the 300 KB bundle once, so 8 parses of 71 M = 568 M. The suite costs 100 times 494 M = 49,400 M. The disk cache can save at most 7 of the 8 parses: 497 M of about 49,970 M, or 1.0%. It does not pass gate 2. If every test is a new process, each test parses once: 71 M of 636 M = 11%, which passes. So the disk cache matters for a CI that starts one process per test, and not for the warm workers of this design. A real bundle is 1 to 3 MB, so the parse is 3 to 10 times larger; the same ratios hold for tests of the same bundle.

### 2.6 How we measure step 5

- Same suite, same fixture, 3 runs, instructions and peak RSS.
- Rows: V0 (new process for each test), V1 (one warm process). The gate for step 5 is V1 below V0 by the amount the model gives: at least 10% on the fixture. A bundle of 1 MB or more should reach 20% or more (estimate: parse 230 instructions per byte times 1 MB is 230 M of about 1,000 M).
- A `bare-mcp` row: 10 `open` calls through the MCP server with and without A1. Needs an https fixture to show the TLS part (to verify).
- Memory row: 1, 10, 20 and 50 documents in one process (measured on the fixture: section 2.4). A growth above 0.1 MiB per document is a finding.
- Cache counters per `open`: JS parse hits, CSS parse hits, reply cache hits.

## 3. Step 6: style on demand

### 3.1 The current code path

`Page::frame_now` (`crates/bare/src/page.rs:596`) makes the frame when `frame` is empty or stale. A frame is `Frame { styles, layout, areas, bars }` (`browser_ui.rs:1125`). `Browser::render` (`browser_ui.rs:1152`) runs `cascade_now` (all elements) and `layout_areas` (the whole document). The incremental paths (`refresh`, `restyle_frame`, `page.rs:691` `render_all`) update the same frame.

Who calls it:

| Consumer | Where | What it reads |
| --- | --- | --- |
| `snapshot`, `find`, `read` | `crates/drive/src/snap.rs:540`, `:607`, `:641`, and `:764`, `:816` | `Page::inspect` gives the whole `Frame`. The code reads only `Property::Cursor`, `Display` and `Visibility` from `frame.styles` (`snap.rs:183`, `:187`, `:270`, `:688`). It never reads `frame.layout`. |
| Locators | `crates/drive/src/locate.rs:107`, `:163`, `:185`, `:195`, `:222` | The same three properties. |
| `visible_box`, `click`, `type`, hit test | `control.rs:320`, `page.rs:755`, `:795`, `:939` | Geometry of one element, and hit testing against all boxes. |
| Window paint | `page.rs:781`, `:993`, `:1073` | The whole frame, and then pixels (step 2 makes pixels on demand). |

The list shows the gap. `snapshot`, `find`, `read` and the text and count locators need three style properties and no layout. They pay a whole cascade of 106 properties (856 B per distinct style, `design-style.md` section 1.1) and a whole layout.

### 3.2 What the 48 journeys ask for

I classified the steps of `tools/drive/public/*.steps` (a script of 30 lines, counts below):

| Step kind | Steps | Needs |
| --- | --- | --- |
| `expect js`, `expect url`, `expect title`, `expect cookie` | 76 `js`, and the `url`/`title` steps | no frame (unless the script reads layout: 4 of 76 use `innerText` and similar) |
| `open`, `goto`, `reload` | 48 `open`, 21 `goto` | no frame (measured: a headless `open` with no locator renders nothing) |
| `click`, `type`, `scroll`, `press`, `expect LOC visible/hidden/enabled/focused` | 74 | geometry of one element (hit test: all boxes) |
| `expect LOC text/count/checked`, role locators | 50 | styles only (`Display`, `Visibility`) |

Per journey: 12 of 48 have no geometry step. 6 of those 12 have no locator step at all, so the driver never asks for a frame. The other 6 have only style-only locators. 36 of 48 have at least one geometry step, so they need a layout.

### 3.3 Designs

**A. A styles-only frame.** Split `Frame` into `Styles` and `Layout`. Add `Page::inspect_styles(f)` that builds only the cascade. `snap.rs` and `locate.rs` use it (they never read the layout). The window, hit tests and geometry steps still use `inspect`. Data structures: the same `Styles`; the layout is `Option<Layout>` that `frame_now` fills when a geometry consumer asks. Correctness: none changes. A consumer that reads only the cascade gets the same values.
Cost: a small change of `Frame` and of its 5 callers. Gain: removes layout (6.9% of journey instructions in the profile) for a journey that never needs geometry.

**B. A property-mask cascade.** The cascade computes only the properties in a mask (`Display | Visibility | Cursor`), for elements that the caller reads.
- Rule filter. At the build of the rule index, keep only the rules whose declaration block has a masked property. I counted this on two real sheets (script: a regular expression over the sheet, nested blocks counted separately): `bootstrap.css` 229 of 2,562 rule blocks (8.9%), `fontawesome.css` 2 of 2,583 (0.1%). Matching tests 11 to 1,000 times fewer rules (hypothesis for the corpus: two sheets are a small sample).
- Computed style. 3 properties instead of 106: about 8 B per distinct style instead of 856 B.
- Inheritance. `Visibility` and `Cursor` inherit, so the mask run walks parent first (O(depth) per element, one pass in document order).
- Traps: `display: var(--x)` reads a custom property that other rules set. If any rule with a masked property uses `var(`, the mask adds all custom properties (a cheap switch to the full cascade). `all: unset` and other shorthands that set a masked property must count as masked. `:has()` and sibling selectors stay in the rule, so matching stays right.
- Complexity: O(elements × masked candidates). The masked candidates are the 0.1 to 9% of rules.
- Gain: the style share of journeys with a style-only frame (17.7% of instructions in the profile) falls to about 2% (estimate: 8.76% of matching and 9% of the rest, both times 0.1).

**C. Geometry on demand with a dependency closure.** Lay out only what a target's rectangle depends on.
- Closure rule R1 (block flow): the target, its ancestors (used widths), all in-flow preceding siblings of each ancestor (heights, margin collapse), floats and positioned boxes that overlap. Closure R2: a flex, grid or table container is laid out as a whole, so is an inline formatting context that holds the target. A fallback to full layout for anything else.
- A hit test needs all boxes under the point. So `click` needs more than `visible_box`.
- Other engines do not do this: Blink and Servo lay out the whole document and skip work through caches and containment (`design-review.md` section 2 gives sources: Blink has "ensure style" for a query and it treats a style as possibly affected by layout). Chrome headless runs the same pipeline as headful ([headless](https://developer.chrome.com/docs/automation-and-testing/headless/)).
- Hypothesis: the closure of a random target is half of the page, and larger for a target low on the page. I have no count. The count is the first task of this step.

**D. Lazy per-element styles (Stylo style).** `Styles.of_node` already has a slot per node (`cascade.rs:1794`, `NONE` when unset). Fill the slot on `ensure(node)`: ensure the parent first, then match the node. Layout reads every node in its subtree, so D alone gains nothing for layout; D with B serves snapshot and find, where a caller reads a subtree. It adds a `RefCell` on a hot read path (`style()` is `&self` today). Rejected as the first step: A and B give the same gain with no change of the read path.

**Table: step 6 designs.**

| | A. Styles-only frame | B. Mask cascade | C. Closure layout | D. Lazy styles |
| --- | --- | --- | --- | --- |
| Removes | layout for style-only steps | most matching and style memory for style-only steps | layout of the part of the page that is not in a target's closure | style of elements nobody reads |
| Applies to | 12 of 48 journeys (no geometry step), and the agent tools `snapshot`/`find`/`read` | the same | 36 of 48 (geometry steps) | the same as B |
| Risk | low | medium (var(), shorthand, inheritance) | high (flex, grid, floats, intrinsic sizes) | medium (interior mutability) |
| Code | small | medium | large | medium |
| Gain (estimate) | layout share of those journeys | style and layout share of those journeys | unknown until counted | as B |

### 3.4 Expected gain with arithmetic (estimate)

Shares of journey instructions (profile): style 17.7%, layout 6.9%. Together 24.6%. This is the sum over all style and layout work, whoever asks for it (driver, page script, loader).

- Journeys with a driver frame that needs only styles: 6 of 48 (the others with no geometry have no locator at all). If each is 24.6% style and layout, A and B save at most 80% of that: 6/48 × 24.6% × 0.8 = **2.5% of the total**. That is 1.03 times.
- Journeys with a geometry step (36 of 48): they need full style. A and B save nothing there. C could save part of layout (6.9%). If the closure is half of the boxes: 36/48 × 6.9% × 0.5 = **2.6%**. 1.03 times, and it is the high-risk design.
- Together: about 5% of the total, 1.05 times (upper end). Lower end: 1.02 times, when style that page scripts force (`getComputedStyle`, `offsetWidth`) is a large part of the 24.6%. I do not know that part. It needs a count (6.0).

This agrees with `design-review.md` section 2: "avoiding 30 to 60% of style/layout ... about 1.11 to 1.24 times", and "If regular snapshots force broad style, the result could be near zero". My counts say near zero for the 48 journeys. The reason is in section 3.2: the journeys are mostly geometry steps.

Where step 6 can matter more:
- The agent tools of `bare-mcp`: `snapshot`, `find` and `read` only read styles. A session that reads and then types gets a frame at the first click. It gains only on the reads before it.
- A suite with many `expect text` and `count` checks (style-only) and few clicks. The benchmark suite has one such test class (section 5.2).

### 3.5 First task: count (6.0)

`perf-goal.md` asks to count the real dependency closure first. The count has three parts. Each part is an instrumented run on the replay corpus (no code stays in main):

1. **Who forces a frame.** For each journey: the number of frames built by the driver (`frame_now`), by page script (`getComputedStyle`, layout reads in `browser_computed.rs`), and by the loader (`plan`). The step 1 counters ("render triggers") give the first two.
2. **Style demand.** For each driver frame: the set of nodes whose style was read (`Styles::style` call sites in `snap.rs` and `locate.rs`), as a share of all elements.
3. **Geometry closure.** For each geometry step: lay out the whole page (the truth), then lay out only the closure of rule R1/R2, and compare the target rectangle. Report the share of boxes laid out and the number of mismatches. A mismatch is a bug in the rule.

Gate: build B only if part 1 shows that style-only driver frames are at least 15% of style instructions on the corpus. Build C only if part 3 gives a median closure below 25% of boxes and no mismatch on the 74 geometry steps. Otherwise stop at A.

### 3.6 How we measure step 6

- Instructions of the 48 journeys on the replay corpus, 3 runs, against the baseline (criterion 2). Report the 12 no-geometry journeys apart from the 36 others.
- A differential check: for every driver step, the result with the styles-only frame equals the result with the full frame (found elements, names, visibility, snapshot text). Run on the corpus and on the fixture.
- Memory: peak RSS of a `snapshot`-only session before and after (B shrinks the style table).
- Fallback test: a page that uses `display: var(--x)`, `all: unset`, `:has()`; each must give the same snapshot as the full cascade.

## 4. Step 7: setup reuse and bounded workers

### 4.1 The current code path

| Where | What it does |
| --- | --- |
| `crates/drive/src/steps.rs`, `state.rs` | `save-state "FILE"` and `open URL state="FILE"`. The file is Playwright `storageState` JSON plus `sessionStorage` (mode 0600). `progress.md`: "Session state save and load (2026-11-10)". |
| `crates/drive/src/run.rs:266`, `:470` | `Session::preload(state)` and `Session::export_state()`. |
| `crates/mcp/src/main.rs:234` `ensure_session` | Restores a saved session, runs `check`, and logs in again when the check fails. A saved session is in memory by default and in a file when asked (`main.rs:220` `store`). `progress.md`: "MCP session cache and ensure_session (2026-11-10)". |
| `crates/mcp/src/main.rs:46` | `saved: HashMap<String, StorageState>`: one process, many saved sessions. `live` is a map by name, but `start` closes a session of the same name, and each session has its own `Client`. |

What is not there:

- A runner for a list of tests: the tests, the setup and the order.
- A rule for which tests may share a login.
- Any worker pool: each driver process runs one step file; each MCP server has one agent.

### 4.2 What repeats, and the formula

If the setup is a fraction p of the cost of a test and N tests share it, the suite is faster by 1 / (1 - p + p/N) (the formula of `design-review.md` section 4, lever 3).

Measured p on the fixture (instructions): the login and typing are 636 - 562 = 74 M of 636 M, **p = 0.12**. For N = 10: 1.12 times. That matches the measured suite: V0 = 10 × 636 = 6,360 M; V2 (login once, 224 M, then 10 restored tests, 4,942 M) = 5,166 M; 1.23 times (the extra comes from the warm process, step 5).

The fixture login is cheap: one form, one redirect. A real login is more:

| p | What it looks like (hypothesis) | N = 10 | N = 40 |
| --- | --- | --- | --- |
| 0.12 | measured: the fixture | 1.12 | 1.13 |
| 0.30 | a login page with a 1 MB bundle and one redirect | 1.37 | 1.42 |
| 0.50 | an SSO redirect chain and a 2 s server hash | 1.82 | 1.95 |
| 0.70 | a login with a heavy bundle per page, as in the staging app | 2.70 | 3.15 |

To measure a p that is not 0.12, the fixture server has `--login-ms N` (a server delay on `POST /login`), and `--js-kb` and `--css-kb` (size of the assets). I will report the measured wall time with `--login-ms 300` in section 4.4.

### 4.3 Designs

**A. One setup test, a state in memory or in a file (the existing pieces).**
- Run the setup once: log in, `save-state`. Each test starts with `open URL state=NAME`.
- Data structure: a `StorageState` (cookies, `localStorage`, `sessionStorage` as plain data), shared by name.
- Isolation: a restored state is a copy. The server session is not a copy: a test that signs out, or that changes the account, ends the session for every sibling. Rule: a test is `shared` (read-only against the account) or `own` (it gets a fresh login). The runner marks each test; the default is `own` for a test that has `sign out`, `password` or `delete` in its steps (a lint, not a proof).
- Validation: run a check step after the restore (`ensure_session` does it). If the check fails, log in again, once, and tell the suite which test paid it.
- Gaps: `IndexedDB` is not saved (`progress.md`). A site that keeps its token there needs the login for each test. A CSRF token in a `<meta>` of a page is fine (the page is loaded again). A cookie with a short life: the check catches it.
- Coverage: the login runs once. The suite report lists the login as one test and says so; it must not count the restored tests as login tests (`design-review.md` section 5, gate "no false pass").

**B. A login for each worker (Playwright's model).** Each worker logs in once and keeps its own account when tests change shared server data. Playwright documents both: a shared account from a setup project for tests that do not change server state, and one account per worker for tests that do ([auth](https://playwright.dev/docs/auth)). Each worker is an OS process with its own browser context ([parallelism](https://playwright.dev/docs/test-parallel)). Cost: K logins, not 1. Use it when the test mix writes to the server. The fixture's isolated tests (section 5) can use it.

**C. A checkpoint of the logged-in page, forked for each test (zygote after login).** The same as step 5 design C, but the fork happens after the login and the first page. A test starts from a live page. Rejected for now: `perf-goal.md` defers page and process checkpoints. A JS heap carries timers, sockets and the clock; `design-review.md` section 4 (rank 8) says "very high" risk. Revisit after A and B are measured.

**Table: step 7 setup designs.**

| | A. Shared state | B. State for each worker | C. Forked after login |
| --- | --- | --- | --- |
| Logins for K workers and N tests | 1 | K | 1 |
| Server-state tests | need `own` login | safe if accounts differ | unsafe (shared server session) |
| Code | exists; add a runner | small on top of A | large |
| Risk | sibling tests break each other through the server | needs K accounts | stale sockets, clocks |
| Verdict | build | build (option) | defer |

### 4.4 Bounded parallel workers

**Process pool or thread pool.**

| | P1. A pool of processes | P2. A pool of threads in one process |
| --- | --- | --- |
| How | The runner starts K `bare-drive` processes and gives each a queue of tests. Each process runs its tests one after another in one warm session. | K pages on K threads in one process. |
| Isolation | OS: memory, sockets, globals | shared statics |
| Blocker | none | `bare_net::trace` is a process-global (`run.rs:254` calls `enable` and `take`), so the network records of two sessions would mix. The parse cache is thread-local, so K threads parse K times. `limit_malloc_arenas` (`drive/src/main.rs:20`) is process-wide. |
| Memory | K × RSS, minus shared pages | one heap, shared immutable data | 
| Verdict | build | defer: it needs the global state fixed first |

A pool of processes is what Playwright does, and what the prototype of section 1 does with `xargs -P`. It needs no change of the core.

**Scheduler.** A queue of tests, K workers pull the next test (not a fixed split). Order: the slowest tests first, using the times of the last run (a file; the first run uses the given order). A test that fails kills its worker; a new worker starts (the Playwright rule: a clean environment after a failure) and takes the next test. A test has a time limit. The output is one JSON report for each test, and one summary: counts, wall time, instructions, peak RSS of each worker, login POST count.

**How K is chosen.** Two limits, and the smaller wins:

- Memory: K_mem = floor(budget / peak RSS of a worker). Budget: half of `MemAvailable`. Now 11.8 GiB are available, because 5 agents share the machine, so the budget is 5.9 GiB. Peak RSS per worker: 25 MiB (fixture), 18 MiB (median public journey), 116 MiB (`python-docs-search-results`), 947 MiB (the 16 MiB sheet ramp). K_mem = 240, 330, 52 and 6.
- CPU: K_cpu = cores - 4 = 12 on this machine. The reserve is: 1 core for the fixture server (the Python server is one process with a GIL), 1 for the runner and the test harness, 2 for other users of the machine. A real remote server does not take a local core, so K_cpu is 14 then. Workers that wait for the network can exceed the cores, but the default stays at K_cpu.
- Default K = min(K_cpu, K_mem). On this machine: 12 for the fixture and for the median journey; 12 for the heaviest journey; 6 for a ramp.
- PSS: the program image is 77% of the median peak RSS (`design-style.md` section 1.1). It is file-backed and shared. Estimate: K = 12 workers of 25 MiB each are 300 MiB of RSS and about 12 × 5.8 + 19 = 89 MiB of PSS. That is a hypothesis; the benchmark reads `/proc/PID/smaps_rollup` at the peak.

**Measured scaling (fixture, T = 32 tests of `cold1`, each a new process, K workers, 3 runs).** Instructions per run do not change with K. Wall time is weak under the load of this machine.

| K | Wall time of 32 tests, 3 runs (s) | Speedup against the quietest K = 1 run (3.29 s) | Concurrent memory (K × 25.4 MiB mean peak) |
| --- | --- | --- | --- |
| 1 | 6.76, 5.94, 10.13 (load 33 to 37); 7.09, 4.25, 3.29 (load 12 to 13) | 1.0 | 25 MiB |
| 2 | 1.62, 1.77, 2.59 | 2.0 | 51 MiB |
| 4 | 0.94, 0.88, 0.94 | 3.7 | 102 MiB |
| 8 | 0.56, 0.54, 0.54 | 6.1 | 203 MiB |
| 16 | 0.43, 0.44, 0.52 | 7.6 | 406 MiB |

Reading:
- Instructions: 20.3 to 20.4 G for 32 tests at every K (one run at K = 16 gave 20.17 G). Workers do not change the work. 32 × 636 M = 20.4 G.
- The K = 1 wall time is not stable: 3.3 to 10.1 s, because other agents used the machine. I compare against the best K = 1 run, which is the least favorable to the pool. Against the median of the six K = 1 runs (about 6.4 s) every speedup is about twice larger. Wall time stays weak (`CLAUDE.md`).
- The curve bends after K = 8: 6.1 times at K = 8 and 7.6 at K = 16 (efficiency 0.48). The reasons are a hypothesis: 16 cores shared with other agents, the single-process Python fixture server, and the process start of each test. K = 8 to 12 is the useful range here.
- The total of the peak RSS of the 32 processes is 811 to 815 MiB, but at most K of them live at once. The last column is the concurrent estimate.
- Every run passed 32 of 32 tests.

**Login with a server delay.** `--login-ms 300`, 10 tests in one process, wall time (2 runs each): cold10 (login each, one process): 3,905 and 3,949 ms. reuse10 (state restored): 850 and 763 ms. The setup alone (`login`): 424 ms. Instructions did not change (5,718 M and 4,947 M). So the server delay of 10 × 300 ms is the difference. Estimate for V0 (a new process for each test, 0.2 s CPU each, 0.3 s login): 10 × 0.5 s = 5.0 s. V2 on one worker: 0.42 + 0.81 = 1.2 s. That is **4.1 times on one worker**, with no pool. When the login is slow on the server side, setup reuse alone meets criterion 7 in wall time.

### 4.5 Isolation checks for step 7

| Risk | Check |
| --- | --- |
| A shared login hides a login bug | The suite lists the login test apart. The server counts login POSTs: the suite expects `1 + own tests` (section 5.1). |
| A test ends the shared server session | `own` tests do their own login. The check step before each shared test catches the rest (the runner logs in again once and reports which test paid). |
| Two workers write the same account data | The fixture has a per-user list that tests append to. A test class `write` uses one account for each worker. |
| A worker leaks memory over many tests | The report has peak RSS at the end of each worker. A worker restarts after M tests (default 200) if the growth is above a limit (section 2.6 gives the growth per document). |
| A server rate limit | K is the only control. The runner has `--workers` and a pause between logins. |
| A state file holds live credentials | Mode 0600 (exists, `run.rs` `private`). The suite directory is deleted at the end. A reply of a tool gives counts only (exists). |

## 5. The benchmark suite

### 5.1 The fixture app (`tools/suite/`)

A small local server under `tools/suite/`, written like `tools/https_keepalive_server.py` (Python standard library, loopback, no dependency). The prototype of section 1.1 is the starting point. Step 1 of `perf-goal.md` also asks for a local login fixture: if the replay agent builds one first, use it and add what is missing here; do not build two.

| Part | Spec |
| --- | --- |
| Routes | `GET /login` (form: email, password), `POST /login` (sets `sid`, `HttpOnly`, `SameSite=Lax`, redirects to `/home`), `POST /logout`, `GET /home`, `GET /p1` to `/p8` (302 to `/login` with no valid `sid`), `GET /api/items` (JSON, needs `sid`), `POST /api/note` (writes to a per-user list; for the `write` tests), `GET /__stats` (counts). |
| Assets | `/static/app.css` and `/static/app.js`, `Cache-Control: public, max-age=31536000, immutable`, an `ETag`. Sizes by flag (`--css-kb`, `--js-kb`). The script has many functions that never run and a small `boot`. |
| Knobs | `--login-ms` (server delay on the login POST), `--js-kb`, `--css-kb`, `--users K` (K accounts for the per-worker mode). |
| Counters | `GET /__stats`: login POSTs, requests, static requests, API requests. The runner reads it before and after a suite. |
| Wire | One `send` for the head and the body. Keep-alive. (A two-send server adds 41 ms to each response through Nagle and the delayed ACK; measured in section 1.1.) |
| Determinism | Fixed page content. No time or random value in a page body. |
| Server state | `POST /logout` ends only that session id. `POST /api/note` appends to the list of that user. |

### 5.2 The tests

N tests in the standard suite: 40 (a smoke suite of 10 is the same shape). Each test logs in or restores, then visits 5 pages (click, check the heading, check the list count after the `fetch`). The test classes:

| Class | Share | What it does | Login |
| --- | --- | --- | --- |
| `read` | 70% | visits 5 pages, checks headings and counts (the fixture of the measurements) | shared |
| `text` | 10% | visits 5 pages, checks only text and counts (`expect text`, no click on the page after the first): style-only locators (step 6) | shared |
| `write` | 10% | adds a note through the page, checks the list | one account for each worker |
| `own` | 10% | signs out and checks the login page, or a wrong password | own login |

With 20% of the tests needing their own login, the best p-model result is limited: 1 / (0.2 + 0.8/N) + the cold cost of the 20%. I report the 100% shared variant too (`read` only), to show the ceiling.

### 5.3 The variants

| Variant | What runs | Measures |
| --- | --- | --- |
| V0 | **Baseline.** Sequential. A new `bare-drive` process for each test. Each test logs in. | criterion 7 baseline |
| V1 | Sequential, one warm process (one step file). Each test logs in. | step 5 |
| V2 | V1 with a shared login: the setup runs once and saves the state. `read`, `text` and `write` restore it. `own` logs in. | step 7, setup reuse |
| V3 | V2 with K workers, K = 2, 4, 8, 12. | step 7, workers (criterion 7) |
| V4 | V3 with a fork server (step 5 design C). | optional |

### 5.4 What we report

For each variant: 3 runs, the median and the spread.

| Metric | How | Why |
| --- | --- | --- |
| Suite wall time | `date` around the runner | criterion 7 |
| Sum of instructions of all processes | `perf stat -e instructions:u` for each process | trustworthy under load (`CLAUDE.md`) |
| Peak RSS of each worker and the sum | `VmHWM` in the report | memory per worker |
| Sum of PSS at the peak | `smaps_rollup` of each worker | shared pages count once |
| Login POSTs from `/__stats` | read after the suite | V2 and V3 must show `1 + own tests + K` (K only in the per-worker mode) |
| Passed tests | the reports | every variant passes all N tests, or it is not counted |
| Cold and warm apart | the first suite run after a fresh server, and the second | `perf-goal.md` step 1 |
| Login coverage | the setup test is listed apart | no false pass |

Criterion 7 reads: V3 (the chosen K) wall time is at most half of V0. The suite does not count as passed when a test failed or a login count is wrong.

### 5.5 Expected numbers (estimates)

Instructions per test from section 1.2: V0 636 M; V1 569 M; shared restored test 494 M; own test 567 M.

| Variant | CPU for N = 40 (instructions) | Ratio to V0 | Wall (estimate) |
| --- | --- | --- | --- |
| V0 | 40 × 636 = 25,440 M | 1.00 | 40 × 0.2 s = 8 s (measured per test: 0.19 to 0.22 s) |
| V1 | 40 × 569 = 22,760 M | 1.12 | about 7 s |
| V2 | 224 + 0.9 × 40 × 494 + 0.1 × 40 × 567 = 224 + 17,784 + 2,268 = 20,276 M | 1.25 | about 6.3 s |
| V3, K = 4 | the same 20,276 M | 1.25 | 6.3 s / 3.7 = 1.7 s, 4.7 times (3.7 is the measured scaling of section 4.4) |
| V3, K = 8 | the same | 1.25 | 6.3 s / 6.1 = 1.0 s, 7.8 times |

Reading: the instruction ratio is 1.25 times in V2 and V3. The wall ratio meets criterion 7 at K = 2 (6.3 s / 2.0 = 3.2 s, 2.5 times). The 0.2 s per test is the sequential cold run measured at a load of 10 to 20; a quiet machine ran it at 0.1 s, so all wall values are weak. So criterion 7 as written is a test of the worker pool and not of the engine. See open question 1.

## 6. Build order and gates

| # | Task | Gate before the next | Why in this order |
| --- | --- | --- | --- |
| 0 | The fixture app and the suite runner as a script (`xargs -P`), V0 to V3 | V0 and V2 reproduce the numbers of section 1.2 within 5% | Everything else is measured on it. |
| 1 | Step 5 A1: one network client in `bare-mcp`; A2: CSS parse cache; A3: counters | V1 is below V0 by 10% or more on the fixture; the isolation tests of 2.4 pass; the memory rows show no growth above 0.1 MiB per document | It is small and it unblocks the MCP loop. |
| 2 | Step 7 A: the suite runner with shared state, `shared` and `own` tests, check and re-login | V2 is below V1; login POSTs are right; the checks catch an ended session | The largest CPU lever. |
| 3 | Step 7 P1: the pool of processes, scheduler, K choice | V3 passes criterion 7 at the default K; memory sum is under the budget | Meets criterion 7. |
| 4 | Step 6.0: the count (section 3.5) | the three counts are in `progress.md` | The count decides whether B and C exist. |
| 5 | Step 6 A: the styles-only frame | the differential check passes; the no-geometry journeys gain | Cheap and safe. |
| 6 | Step 6 B, C, step 5 C and D | only after their gates | Gated. |

Interaction with the five agents that are building now (I reference them as planned):

- The replay corpus (step 1) gives the instruction counts of the journeys without network noise. Steps 6.0 and the V0 to V3 rows use it.
- The sheet registry and the cheap CSSOM objects remove style work for every consumer. They shrink the part that step 6 can save. Measure step 6 after them, as `design-review.md` section 5 says: do not add baseline percentages twice.
- Raster on demand (step 2) removes the paint cost that a window frame has. Step 6 A does the same for layout.
- Lazy JS (P1) cuts the per-realm walk of 34 M per document, the largest part of a warm document in the fixture. After it lands, V1 and V2 get smaller per test, and p gets larger (a fixed setup is a larger share of a smaller test). The ratios in section 5.5 change: measure again.

## 7. What I did not verify

- A real login. The fixture login is cheap (p = 0.12). The p values 0.30 to 0.70 are hypotheses from the formula, not measurements. The staging app was not run.
- TLS. The fixture is plain HTTP. The cost of a handshake and of loading the root certificates for each MCP `open` is a hypothesis.
- That the parse cache hits in the second document. I infer it from the cost split (the first document is 75 M above a later one, and that matches 230 instructions per byte of parse). I did not read `parse_cache::counts()`.
- The style share that page scripts force. The 17.7% and 6.9% are from the earlier profile; I did not split them by who asks.
- The closure of geometry. No count exists.
- The share of rules that set `display`, `visibility` or `cursor` on real sites. Two sheets only.
- That a late reply of an old document cannot reach a new document in a shared client. I read the structure; I wrote no test.
- Memory growth over many documents on a real app. The fixture ran 1 time each for 20 and 50 documents.
- PSS. I did not read `smaps_rollup`. The PSS number is an estimate.
- Windows and macOS. The fork server does not exist there. The process pool is portable.
- A fork server inside this process model with the network worker threads. Not built, not tried.
- Playwright's default worker count. The page does not say.
- Cypress's `cy.session` behavior is only in `progress.md`; I did not open its documentation.

## 8. Open questions for the owner

1. **Criterion 7 as written is met by workers, or by a slow login.** Wall time at K = 2 is already below half of V0 (measured scaling 2.0 times), and a 300 ms server login gives 4.1 times on one worker, while instructions fall only 1.25 times. Do you want a second target on instructions or CPU time (for example "the sum of instructions of the suite is 1.5 times lower or more with K = 1")? Without it, criterion 7 does not test the engine.
2. **What is a "related test" for the suite?** My default: the fixture of section 5.2 (70% read, 10% text, 10% write, 10% own login). Do you want a real staging-style login in the suite as well, with a real SSO redirect and a 1 MB bundle? It gives a true p, but it needs the vault and the network.
3. **Where does the worker pool live?** My default: a runner script in `tools/suite/` first, then `bare-drive --suite DIR --workers K`. The alternative is an MCP tool `run_suite`. Which one do you want for the product?
4. **How much isolation is enough between tests that share a process?** My default: documents are isolated by state and jar (design A), and a failed test kills its worker. The alternative is a fork server (step 5, design C) with process isolation for every test. It costs more code and does not run on Windows.
5. **A shared login: is the per-test `own` lint enough?** The runner marks `sign out`, `password` and `delete` tests as `own` by a text rule. Do you want the test author to mark it by hand, with no lint?
6. **Reply cache and `private` pages.** The reply cache keys on scheme, host, port and path, with no jar. I propose keying private responses by jar. It is a change of `bare-net` behavior (section 2.4). Do you accept it before A1?
7. **Step 6: stop at the styles-only frame?** My estimate is 1.02 to 1.05 times on the 48 journeys. Building the property-mask cascade or the closure layout needs the count first. Do you accept that rule, or do you want B built now?
8. **MCP: one shared client for the server.** `bare-mcp` makes a new client for each `open` today. Sharing one changes what an agent sees: a cached script survives between `open` calls. Do you want it on by default, or a flag per `open` (`fresh: true`)?

## 9. Owner decisions (2026-11-13)

1. Criterion 7 gets a second target: the sum of instructions of the suite is 1.5 times lower or more with one worker. The wall-time target of 2 times stays.
2. The suite uses the local fixture of section 5.2. A real SSO login is not part of it now.
3. The worker pool comes first as `bare-drive --suite DIR --workers K` (after a runner script in `tools/suite/`), then as an MCP tool `run_suite` on top.
4. Isolation: design A. Documents are isolated by state and jar, and a failed test kills its worker.
5. Shared login: the runner lint marks `own` tests, and a test author can also mark a test by hand.
6. The reply cache keys `private` responses by jar.
7. Step 6 stops at the styles-only frame. Count what forces style and layout before more is built.
8. `bare-mcp` shares one network client by default. An `open` with `fresh: true` gets a clean client.
