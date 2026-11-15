# Design: why Gmail loads slower in bare than in Chrome

Status: proposal. The owner reviews this document before any code is built.
Written on 2026-11-15 against main `b5efbde0` (task R2-G). Machine: archMachine (16 cores, shared). Load averages at the start of the bare runs: 2.2 to 4.4 (1 minute). Instruction counts (`instructions:u`) are the main figure. Wall times are marked with their spread.
A claim marked "hypothesis" or "to verify" is not checked. Nothing here is built.

Safety. Every run was read only: the inbox was opened and waited on. bare used its saved session. Chrome used the `chrome-auto` profile, one new tab per run, closed after the run. No cookie moved between the browsers. This file has no account name, address, subject or mail content. The raw recordings (network records, Chrome traces, perf data) hold session data. They are only under `/data/bare-tmp/gmail-profile/` and `/data/bare-tmp/r2/gmail-load/raw/`. None is in the repository.

## 0. Summary

The question: why is Gmail "8 times slower" in bare (PR #548)?

1. **The 8x compares two different events.** #548 compares bare's `open` return (about 21 s) with Chrome's first row (1.2 s). The `eval` poll of #548 also ran the page for 8 s before it returned, so its "about 9 s to first row" is not the first row. At the same event, the first `tr.zA` row, bare needs **4.4 s** (4.28 to 4.52, 3 runs, 18.0 to 20.5 G instructions) and Chrome needs **1.2 to 1.5 s** (cache on or off, service worker on). That is 3.4x in time.
2. **Instructions at the first row are about equal. Total instructions are 1.9x.** bare: 17 to 20 G at the first row, 106 G at the end of `open`. Chrome (whole `chrome-auto` cgroup, all processes): 12 to 19 G at the first row, 53 to 57 G after 30 s. Chrome's renderer alone was not measured. bare reaches Chrome's 47 G (6 s) at about 8 s, so its pace per instruction is close; it needs twice the instructions and starts 2 s late.
3. **The time gap at the first row has two parts: about 2 s of waiting and about 2.5 s of one thread running JS.**
   - bare is idle for the first 2.0 s: it waits for the whole document (first byte at 1.24 s, body 0.77 s). Chrome with its service worker has the document in 0.01 s. Chrome with the worker bypassed (cold, like bare) has the first byte at 0.43 s (with the handshake) and the end at 1.4 to 1.6 s, and it starts the scripts at 0.43 s, while the document still streams.
   - After the document, bare needs 2.5 s and 17 G instructions to the first row, on one thread. Chrome's main thread is busy 1.4 s for the same step, and its JS self time is 0.8 s.
4. **Chrome and bare run the same code.** Both get the same ES5 bundle. At the first row bare has run 3.02 M function calls and Chrome 4.06 M (ratio 0.74). The top five functions by steps in bare have call counts within 0.72x to 0.96x of Chrome's. So there is no big feature-detection detour in bare. One difference: bare has no `navigator.serviceWorker`, so Gmail serves a server-rendered document (325 KB, not 447 KB) and no data frame. Hypothesis: this does not add work.
5. **The settle rule is not the problem.** `open` returns at 21.1 to 22.4 s, when Gmail's start-up work ends (the instruction rate falls to 0.2 G/s at about 19 s, then five `bscframe` requests of 0.3 to 1.3 s). Chrome's JS calls also end at about 14 to 22 s (96% by 13.6 s). bare does in 19 s what Chrome does in about 8 s.
6. **Recommendation** (section 5): **A. Stream the document and start its scripts at the first bytes** (first row 4.4 s to about 2.5 to 3 s, hypothesis), **B. Make the document request equal to Chrome's** (find why the first byte comes at 1.24 s against 0.43 s), then **C. Cut the cost of a call in the tree walker** (3.0 M calls at 5.7 K instructions each are the whole budget to the first row).

## 1. Measurements

All bare runs: release build with symbols of `b5efbde0` plus a temporary probe (a mark when `document.querySelectorAll('tr.zA').length>0` first holds; the patch is in `/data/bare-tmp/r2/gmail-load/probe.patch`, not in the repository). One fresh `bare-mcp`, `open` with the saved session, `timeout_ms` 150000, 1280x800, no HTTP cache. Chrome 155.0.8059.39, headless, one new tab, `Page.navigate` to the inbox, 1280x800, row polled every 100 ms.

### 1.1 The first row and the end of `open`

| Event | bare (3 runs) | Chrome, cache on, SW on (3 runs) | Chrome cache off, SW on (3 runs) | Chrome cold: cache off, SW bypassed (3 runs) |
| --- | --- | --- | --- | --- |
| First row | 4.31 / 4.28 / 4.52 s | 1.42 / 1.29 / 1.16 s | 1.27 / 1.17 / 1.26 s | 1.47 / 2.20 / 1.35 s |
| Requests to the first row | 156 in `open` (to 22 s) | 239 to 255 | 249 to 258 | 183 to 207 |
| Instructions at the first row | 18.3 / 20.5 / 18.0 G | 11.8 to 19.6 G (6 runs) | 15.2 to 17.1 G | not measured |
| `open` returns | 22.4 / 21.4 / 21.1 s | | | |
| Instructions at `open` return | 108.3 / 106.2 / 105.7 G | 52 to 57 G (after 22 s) | | |

Spread: bare 0.24 s (6%) in time, 2.5 G (14%) in instructions. Chrome cache on 0.26 s. The slowest cold Chrome run (2.20 s) had a slow document (1.6 s). The Chrome instruction counts are from the #548 raw runs (`instructions:u`, `perf stat -a -G` on the cgroup of `chrome-auto.service`, 0.5 s intervals, rounded up to the next interval, so the first-row figure has about 0.5 s of error). They include the browser, GPU, network and every renderer.

Instructions over time, same runs (G, cumulative):

| Time after navigation | 1.5 s | 3 s | 6 s | 10 s | 22 s | 30 s |
| --- | --- | --- | --- | --- | --- | --- |
| Chrome, cache on (3 runs) | 18.5 to 19.6 | 29.8 to 34.1 | 43.3 to 47.0 | 44.8 to 48.1 | 52.3 to 55.7 | 52.6 to 55.9 |
| bare (3 runs, rounded down to the 0.5 s interval) | 0 | 5.2 to 7.7 | 29.1 to 31.6 | 60.2 to 63.6 | 104.5 to 106.6 | |

Chrome reaches 47 G at 6 s; bare reaches 47 G at about 8 s. So per instruction bare is not 8x slower than the whole of Chrome: it runs 7 to 9 G/s on one thread, and it needs about twice the instructions. bare's rate is 7 to 9 G/s from 3 s to 11 s, 2 to 6 G/s from 12 s to 18 s, and 0.2 to 0.6 G/s after 19 s.

### 1.2 Split of bare by subsystem

`perf record -e instructions:u -c 4000000 -g --user-callchains`, frame-pointer build (`rec-2`: 26,845 samples, 107.4 G, first row at 4.46 s, `open` return at 21.6 s). A sample goes to the first subsystem frame found from the leaf up.

| Subsystem | 0 to first row (17.1 G) | first row to 9.8 s (44.5 G) | 9.8 s to 21.6 s (44.4 G) | Whole run (107.4 G) |
| --- | --- | --- | --- | --- |
| JS interpreter (`bare_js::eval`, heap, shapes) | 70.8% | 87.9% | 73.2% | 78.9% |
| DOM bindings (`bare::browser_*`) | 2.2% | 2.1% | 7.9% | 4.6% |
| JS parse (lazy function bodies and scripts) | 13.5% | 2.4% | 3.4% | 4.5% |
| Allocator | 4.6% | 2.7% | 5.0% | 4.0% |
| Style (`bare_css`, `cascade`) | 2.0% | 1.9% | 4.3% | 3.0% |
| JS garbage collector | 1.6% | 1.1% | 2.1% | 1.6% |
| Layout | 0.3% | 0.9% | 2.0% | 1.2% |
| Decompression (brotli) | 1.5% | 0.3% | 0.5% | 0.6% |
| Regexp | 1.5% | 0.1% | 0.5% | 0.5% |
| DOM, HTML parse, CSS parse, TLS, network, paint | 1.9% | 0.6% | 1.1% | 1.3% |

Parts of the first row, in time:
- 0 to 1.95 s: the process is idle (6 samples). It waits for the document (the body ends at 2.0 s).
- 1.95 to 2.1 s: 0.3 G. HTML parse and the first scripts.
- 2.1 to 3.0 s: 7.1 G. Top-level scripts run, and lazy functions parse at their first call (js_parse 28%).
- 3.0 to 4.46 s: 9.7 G. Interpreter 86%. This is Gmail's own start-up code and the first render.

The network is not a cost in bare after the document: TLS, network and decompression together are 0.7% of all instructions.

### 1.3 Top functions and scripts

Top self cost (share of all samples): `Interp::link_inner` 11.3% (the member and call chain, with the inline-cache hit inlined), `eval_expr_inner` 9.7%, `exec_simple` 3.6%, `exec_list` 3.0%, `member_ic` 2.7%, `call_fn_inner` 2.5%, `get_name` 2.4%, `Heap::get_dict` 2.4%, `call_script` 2.2%, `select` 2.2%, `link_base` 2.1%, `machine` 2.0%. Outside the interpreter: `str::contains` in `Dom::plan` (the style pass) 1.9%. Inclusive, `call_script` is on 87.7% of the samples.

Cost to the first row, by script (counter build, per function: steps and calls; 44.0 M steps, 3.02 M calls, 14,536 functions run):

| Script (URL pattern, host `mail.google.com`, `k=gmail.mainalt…es5.OL`) | Size | Steps | Calls |
| --- | --- | --- | --- |
| `m=b` | 1.46 MB | 36.6 M (83%) | 2.75 M |
| `m=ebdd,sps,l,spit,…` | 2.03 MB | 6.1 M (14%) | 0.22 M |
| `m=a` | 1.42 MB | 0.85 M (1.9%) | 0.03 M |
| `www.gstatic.com … og.qtm … m=qgl,q_dnp,q_sf` | 0.28 MB | 0.18 M (0.4%) | |
| all others | | under 0.5% | |

Top-level script runs are not the cost. 77 scripts run top-level in 602 ms in total (`m=NVcOs,rn4kU,…` 88 ms, `m=ps` 74 ms, `m=cm,e,be` 68 ms, `m=ebdd,…` 44 ms, `m=b` 35 ms, `m=a` 28 ms). The cost is in the functions that the page calls later. The five top functions of `m=b` (`_.ybb` 3.2 M steps, `_.uB` 3.0 M, `_.pz` 2.7 M, `_.qz` 1.2 M, `_.SA` 0.6 M) run 0.7x to 0.96x as often as in Chrome.

Steps and calls: 44.0 M steps and 3.02 M calls give 14.6 steps per call, 389 instructions per step, 5.7 K instructions per call (17.1 G). Two of the top four functions are tiny (10 and 11 steps per call, 120 K and 250 K calls).

### 1.4 Timeline of bare (`pr-settle-1`, seconds after `open`)

| Time | Event |
| --- | --- |
| 0.00 | Document request starts. |
| 0.16 | TLS handshake done (DNS 57 ms, TCP 49, TLS 55). |
| 1.24 | First byte of the document (time to first byte 1.08 s after the handshake). |
| 2.0 | Body ends (325 KB, 0.77 s). HTML parse and the first requests start at once (wait 0 to 17 ms). |
| 2.03 to 2.07 | `m=b`, `m=a`, `m=ebdd,…` and the `sv` script are requested on the open connection (time to first byte 52 to 230 ms, body 36 to 134 ms). |
| 2.22 to 2.45 | `m=b`, `m=a`, `m=ebdd,…` run top-level (35, 28 and 44 ms). |
| 2.2 to 4.46 | Lazy parse and Gmail start-up. 3.02 M calls. |
| 4.46 | First row. |
| 6 to 11 | About 14 lazy modules (`m=NVcOs,…` 643 KB at 6.2 s and so on) are fetched and run. |
| 11 to 19 | `sync/u/0/i/fd` (7 requests of 0.4 to 0.5 s) and later modules (`m=cm,e,be` 506 KB at 15.3 s). |
| 19 | Instruction rate falls to 0.2 G/s. |
| 19.3 to 21.4 | Five `mail/_/bscframe` requests (0.3 to 1.3 s each, no body). |
| 21.1 to 22.4 | `open` returns. |

"Wait" in the network records of lazy scripts (4 to 17 s) is the time since the page start, not a queue: a script that the parser did not find has no `wanted` time (`bare_net::trace`).

### 1.5 Chrome

Main renderer thread to the first row, cache on (trace of one run, first row at 1.42 s): `RunTask` busy 1.42 s. JS self time 0.80 s (`m=b` 291 ms, `m=ebdd,…` 125 ms, the document's inline code 107 ms, `m=a` 73 ms). `v8.compile` on the main thread 334 ms (27 compiles; four use a code cache of 0.3 to 1.6 MB; the scripts `m=b`, `m=a` and `m=ebdd` are streamed and compiled off the main thread). `ParseHTML` 108 ms, style 32 ms, layout 19 ms, GC 17 ms, paint 3 ms. A second renderer (an iframe) is busy 0.89 s.

Calls (precise coverage), per interval: 4.1 M to the first row (1.28 s), 13.7 M to 3.1 s, 2.6 M to 4.6 s, 10.5 M to 7.6 s, 3.1 M to 13.6 s, 1.3 M to 21.6 s, 0.03 M to 31.6 s. Total 35.4 M calls. 96% is done at 13.6 s.

Documents. With the service worker: the document is 0 bytes from the worker in 0.01 s, then a second document of 362 KB streams in 1.76 s while the scripts come from the worker (0.06 s). Cold (cache off, worker bypassed): one document of 447 KB, first byte 0.43 s, end 1.41 to 1.60 s, over HTTP/2; the scripts are requested at 0.43 s and take 0.09 to 0.21 s. The page has `serviceWorker`, `SharedWorker` and `Worker` in Chrome. The first row (1.35 to 1.47 s) comes at about the end of the document.

### 1.6 Feature detection in bare

At the first row: `'serviceWorker' in navigator` is false, `SharedWorker` and `Worker` are functions, `MessageChannel` is a function, `BroadcastChannel` and `navigator.locks` are undefined. 5 iframes. 50 rows. The console shows no entry. bare gets no data iframe (`/mail/u/0/data?token=`) and a 325 KB document (Chrome cold 447 KB). Hypothesis: Gmail serves a different page to a client with no service worker; the page differs in size, not in the main code path. To verify: run bare with a stub `serviceWorker` and compare the call counts.

## 2. Split of the gap

Times at the first row, in seconds, bare 4.4 s (4.28 to 4.52) against Chrome 1.2 to 1.5 s. The 21.6 s is the end of `open`.

| Part | bare | Chrome | Gap | What it is |
| --- | --- | --- | --- | --- |
| Measurement (#548: an `eval` poll that ran the page for 8 s) | 9 s in #548, 4.4 s here | 1.2 s | 4.6 s of the old 9 s | Not a browser cost. The first row is now marked inside the loader. |
| Document, to the end of the body | 2.0 s | 0.01 s (SW) / 1.4 to 1.6 s (cold) | 1.9 s (SW) / 0.4 s (cold) | The server: first byte 1.24 s in bare, 0.43 s in cold Chrome (both with the handshake). Hypothesis: different request headers (section 4). |
| Order: scripts start after the document | scripts at 2.03 s | scripts at 0.43 s (cold) / 0.013 s (SW) | 1.6 s (cold) | Chrome streams the document and starts the scripts at its first bytes. bare parses after the last byte. |
| JS to the first row, after the document | 2.5 s, 17 G, one thread | about 1.0 s (main thread busy 1.4 s with the stream overlap) | 1.5 s | The same 3 to 4 M calls. Tree walker against V8. |
| `open` settle rule | 21.6 s | not applicable | 0 | bare returns when the start-up work ends (19 s) plus 2.5 s of `bscframe`. Chrome's start-up work also ends at 14 to 22 s. |

Hypothesis on the last row: the 2.5 s tail of five `bscframe` requests is waiting for pending fetches. To verify: end `open` at the first quiet 100 ms of the instruction rate and compare.

Instructions. At the first row: bare 17 to 20 G, Chrome 12 to 19 G (whole cgroup). For the whole start-up: bare 106 G, Chrome 53 to 57 G. The gap is 1.9x. Chrome's renderer alone would be lower than the cgroup figure: not measured.

## 3. Current design

### 3.1 Document and script order (`browser_session.rs`, `browser_loader.rs`)

- `load_document` fetches the document to its end (`fetch_hop`, `send_stream_once`), then parses it in slices ("document parsed in slices"). The parser finds `<script src>` after the last byte. A fetch of the scripts starts then, in `Run`. Cost: time = document end + script fetch + run. Memory: the whole document.
- Chrome starts the scripts from the first chunk (the preload scanner), parses while it receives, and executes a blocking script as the parser reaches it.
- The fetches of the scripts run on `bare_net::parallel` (a thread pool). The page thread runs the JS between rounds.

### 3.2 JS execution

- Tree walker (`eval.rs`): `eval_expr_inner`, `link_inner`, `exec_simple`; shapes with inline caches (own-property hit inlined in `link_inner`). The bytecode lane stopped (decisions.md, "Stopped at R1": 2.49x on `eval_load loop 10`).
- A function longer than 128 bytes in a script of 16 KiB or more is lazy: the first parse drops its nodes and the first call parses it again (`design-js-reparse.md`). A lazy function is its own script slot; a call across slots swaps eleven fields (about 190 instructions).
- Parse cache: thread-local and per process. A cold process has no hit.
- Cost model measured here: 389 instructions per step, 5.7 K instructions per call at 14.6 steps per call. Complexity is linear in steps; the constant is the problem.

### 3.3 Page and settle

`open` runs `wait_load` and then `Page::idle` (no pending fetches, loaded, no live work). The loader runs rounds of tasks in slices. No other cost was found: network, TLS and decompression are 0.7% after the document.

## 4. Causes

| Cause | Status |
| --- | --- |
| The document is read to its end before the page starts | Measured: scripts requested at 2.03 s, the body ends at 2.0 s. |
| Chrome overlaps the scripts with the document | Measured: scripts at 0.43 s with the document ending at 1.4 to 1.6 s (cold). |
| The first byte of the document comes at 1.24 s in bare (1.08 s after the handshake) and at 0.43 s in cold Chrome | Measured. Cause: hypothesis. The server may render a heavier page for a client with other headers or cookies, or it may add time for a client with no service worker. The 325 KB and 447 KB sizes show different pages. To verify: send the exact request headers of Chrome (including `Sec-Fetch-*`, client hints, `Accept-Language`) from bare and compare the first byte. |
| The tree walker costs 389 instructions per step and 5.7 K per call | Measured. About 78 to 88% of all instructions are the interpreter. |
| Lazy function parse is 13.5% of the first-row instructions | Measured (js_parse 13.5% to the first row, 4.5% over the whole run). |
| `Dom::plan` runs `str::contains` over the rules: 1.9% | Measured. Cause in the code: hypothesis (a check of a pseudo-class text for each rule). Small. |
| Start-up work is the same as in Chrome (same calls, no feature-detection path) | Measured at the first row (0.74x calls); not measured to the end. |
| Single thread against Chrome's parallel compile, network and raster | Measured in part (Chrome's main thread 1.4 s of 1.42 s; compile off thread for the three big scripts). |

## 5. Options

### Part A: the document and script order (about 2 s of the 4.4 s)

**A1. Stream the document into the parser and start the scripts at the first bytes.** The HTTP body gives chunks. A tokenizer reads the chunks as they come. A preload scan starts the `<script src>` and `<link>` fetches at once. A blocking script runs when the parser reaches it and its bytes are there. Data: a chunk queue between the network thread and the page thread. Complexity: linear in bytes; no re-scan. Memory: the same (one document buffer; chunks are freed after the parse). Expected: first row 4.4 s to about 2.5 to 3 s (hypothesis). The document ends at 2.0 s; the 2.0 s of top-level and lazy start-up work of `m=b` and `m=ebdd` (about 2 to 3 G instructions to the top-level calls and the first start-up code) can overlap with the last 1.5 s of the stream; the rest needs the document data and stays after 2.0 s. Instructions: 0 (the same work, earlier). Work: large (the parser takes bytes in pieces; document `write` and `document.open` need care).

**A2. Keep the whole-document fetch. Scan the first chunk and start the script fetches.** The fetches start at 0.5 s instead of 2.03 s. The scripts then wait in memory until the document ends. Expected: first row 4.4 s to about 4.1 to 4.2 s (the fetch of `m=b` takes 0.2 s). Instructions: 0. Work: small (a scan over the first 16 KB; a fetch started with no script element). Not enough alone.

**A3. Document pre-connection and a persistent HTTP cache of the bundle.** Not chosen: the scripts are 0.2 s of the path, and the cache hit is not the gap (Chrome cache on or off differs by under 0.1 s).

### Part B: the first byte of the document (0.4 to 0.7 s)

**B1. Make the headers equal to Chrome's.** Send the Chrome 155 request headers (`sec-ch-ua*`, `sec-fetch-*`, `accept-language`, `upgrade-insecure-requests`, header order). Compare first byte, size and the data frame in a loop of three. Expected: first byte 1.24 s to 0.43 s if the cause is server work for a different client; then first row about -0.6 s. Hypothesis. Work: small (a header table and a comparison run). Risk: the server may serve the service-worker page (447 KB with a data frame) that bare cannot run well. To verify: the call counts of the page.

**B2. Stub `navigator.serviceWorker` with a registration that never activates.** Only to find out whether the server decides on it. Not a product change.

### Part C: the JS cost (2.5 s and 17 G at the first row; 80% of 106 G)

**C1. Cut the cost of a call in the tree walker.** 3.02 M calls at 5.7 K instructions. The call path (`call_fn_inner`, `call_script`, `machine`, `select`, `get_name`, `get_dict`) has 11 to 12% of the first-row instructions as self cost (2 G, about 650 per call). Tiny functions (10 to 11 steps per call, 370 K calls for two functions) pay a full frame. Options: a call-site cache that holds the callee slot (no slot swap check); a frame from a pool with no zeroing; arguments written into the callee's `Env` slots with no `arg_pool` vector. Expected: calls cost 650 to about 400 instructions: -0.75 G (-4%) at the first row. Instructions on the whole run: -4 to -5 G. Work: medium. This is a patch on one hot spot; it removes no kind of work.

**C2. Parse a lazy function once for all realms and from a thread.** Lazy parse is 13.5% to the first row (2.3 G). Idea: a worker thread parses the bodies that the page will call (the second-parse list comes from the first run's skip data) while the page thread runs. The parse cache must be `Sync` (it is thread-local now). Expected: first row about -0.2 to -0.3 s; instructions unchanged on the page thread -2.3 G, total unchanged (the work moves to another core). Memory: the tree stays resident for the lifetime of the cache (the lazy design dropped it for RSS: 3.4% lower RSS over the 47 journeys, so this costs memory). Work: large. Not first.

**C3. Persistent code cache on disk (a V8-style code cache).** A cache of the AST or a compact form by (URL, hash of the bytes). A repeat visit to Gmail skips parse of the top-level and the lazy bodies: -2.3 G (13%) at the first row, -3 G in total. First visit: no gain. Not useful for the owner's cold login case. Work: large (a serialization format for the tree).

**C4. Revive the bytecode lane on hot functions only.** `byte` reached 2.49x on one loop benchmark; no Gmail number exists. Gmail is 80% interpreter, so a 2x step cost would give about -40% of the instructions (-7 G at the first row; -40 G in total) if the hot set were all compiled. Work: very large and the owner stopped lane B on 2026-11-08. Listed only because it is the one design that changes the 389 instructions per step.

## 6. Ranked recommendation

| Rank | Design | Work | First row (time) | Instructions at the first row | Instructions in total |
| --- | --- | --- | --- | --- | --- |
| 1 | B1 equal request headers (check first) | small | 4.4 s to about 3.8 s (hypothesis) | 0 | 0 |
| 2 | A1 stream the document, start scripts at the first bytes (A2 as a first step) | large (A2 small) | 4.4 s to about 2.5 to 3 s (hypothesis); A2 alone about -0.2 s | 0 | 0 |
| 3 | C1 call path | medium | -0.1 s | -0.75 G (-4%) | -4 to -5 G |
| 4 | C2 parse lazy functions on a thread | large | -0.2 to -0.3 s | -2.3 G on the page thread | 0 |
| 5 | C3 code cache on disk | large | none on the first visit | -2.3 G on a repeat visit | -3 G on a repeat visit |
| 6 | C4 bytecode on hot functions | very large | about -1 s | up to -7 G | up to -40 G |

Reasons. B1 costs one afternoon and can remove the largest single part that is not ours (0.7 s of first byte); do it first and measure whether the page changes. A1 removes a kind of work from the critical path (waiting) with no change in the instruction count; it is the largest gain that does not depend on the interpreter. C1 and C2 are patches or moves; they stay behind them. C4 is the only design that changes the main cost (the interpreter is 79% of the run) but it was stopped by the owner.

None of the designs closes the gap to Chrome's 1.2 s at the first row: the document alone ends at 1.4 to 1.6 s in cold Chrome, and bare's JS stays about 2x slower than Chrome's main thread (2.5 s against 1.0 s). With A1 and B1 the first row of bare would be at about 2.5 s, about 2x Chrome cold (hypothesis).

Settle: the 21.6 s is a pace problem of the interpreter (the same start-up work, 106 G against 55 G, one thread), not of the rule. A rule that ends at the first quiet 100 ms of the instruction rate would save about 2.5 s (hypothesis); it would be a limit that hides slow work, so it is not recommended.

## 7. Not measured

- Chrome instructions of the renderer alone, and the instructions of Chrome up to the first row from a run that has the same marker (the Chrome figures are whole-cgroup and rounded to 0.5 s).
- The number of calls in bare at the end of `open` (the counters dump only at the first row).
- Cold Chrome instructions, and Chrome with a code cache cleared.
- The effect of a service-worker stub or of Chrome's headers on bare (B1, B2 are to verify).
- bare with a quiet machine: the bare runs had load 2.2 to 4.4. Instruction counts hold; the wall times (4.3 to 4.5 s) are an upper bound.
- Gmail with a slow network, a second account, a phone width, and the page after the first `open` (mail open, search, compose).
- Memory (Pss) of this page; it is in #548.

## 8. Gmail: Stage 2 check (2026-11-15)

Owner decision (2026-11-15): verify the Stage 2 hypothesis before any build. Main `53d8809b` (with PR #567), archMachine, load average 2 to 8. This section changes no product code. A temporary probe (the patch of #565 plus a dump of the document text and the arrival time of each piece) is in `/data/bare-tmp/r2/gmail-verify/probe-full.patch`, not in the repository.

Safety. Read only. bare used its saved session. Chrome used `chrome-auto` through CDP, one new tab per run, closed after the run, no click and no input, no cookie moved. This section has structure only: offsets, tag kinds, URL patterns and times. It has no account name, address, subject or mail text. The saved documents and the raw records hold session data. They are only under `/data/bare-tmp/gmail-verify/`.

### 8.1 Hypothesis and verdict

Hypothesis: the big scripts (`m=b`, `m=a`, `m=ebdd,…`) are added by inline scripts of the document. bare runs no script before the whole document is in, so it requests them after `DOMContentLoaded`. Chrome runs those inline scripts while the body streams and requests the scripts early. Up to 0.8 s off the first row (4.4 s; Chrome 1.2 to 1.5 s).

Verdict: **half right.** The effect is real. The mechanism is not the one that was guessed.
- The four scripts are **not first requested by an inline script**. Four `<link rel=preload as=script>` tags name them. The tags are in the first 7 KB of the document (0.3% of its bytes). Chrome requests the scripts at the **first byte of the document**, with the initiator `parser`. Chrome has no second request when the inline code adds the `<script>` elements later: the elements use the preloaded answers.
- An inline script **runs** them. It is `_GM_loadJS();` at byte 233,464 (10% of the body). It makes four `<script>` elements (`async = false`) and appends them to `document.body`. Chrome runs this while the body streams. bare runs it after the last byte.
- bare ignores the preload links of the parsed document on purpose (`browser_loader/preload.rs`, "Skipped on purpose"), and the scan of #567 reads only `<script src>` and `<link rel=stylesheet>` (`browser_loader/scan.rs`). So bare requests the scripts at the last byte plus 0.02 s.

So two different things are late in bare: the **request** (a scan rule is enough) and the **run** (only Stage 2 moves it).

### 8.2 The document, as bare receives it (structure only)

One run, `Content-Encoding` undone: 2,318,159 bytes of text (333 KB on the wire), 80 pieces. Chrome without its service worker gets 2,637,9xx bytes (449 KB on the wire) of the same layout. The first 233 KB have the same offsets in both. There is no external `<script src>`: all 16 `<script>` elements are inline.

| Byte offset | What it is | Size |
| --- | --- | --- |
| 4,907 to 6,882 | `<link rel=preconnect>`, `canonical`, `shortcut icon`, `alternate`, `manifest`, then **four `<link rel=preload as=script>`** at 5,807, 6,156, 6,516 and 6,882 (the CDP initiator position of Chrome: line 11, columns 1220, 1580, 1946, 2337). Their targets: `m=b`, `m=a`, `m=m,m_i`, `m=ebdd,sps,l,spit,t,it,anc`, all under `/_/scs/mail-static/_/js/k=gmail.mainalt.en.…es5.OL/…/cb=loaded_N/` | |
| 68,677 to 74,039 | inline script #1: defines `GM_JS_URLS`, an array of the same four URLs (at 70,908, 71,244, 71,601, 71,964). The text of each URL equals the `href` of its preload link (4 of 4) | 5.3 KB |
| 126,125 | `<body>` | |
| 126,386 to 201,823 | inline script #3: the page loader. It defines `_GM_loadJS`: it reads `GM_JS_URLS`, makes a `SCRIPT` element for each (`document.createElement`), sets `async = false`, and appends it to `document.body`. After the last one loads it calls `_B_init` and sets `_jsl = true` | 75 KB |
| 233,425 to 233,486 | inline script #7: `_GM_loadJS();` (the call, at byte 233,464) | 13 bytes |
| 301,061 to 2,318,145 | inline scripts #11 to #15: page data (`_GM_setData` calls), 385 B, 189 KB, 224 KB, 105 KB and 1.5 MB. The first row does not need #15 (see 8.4) | |
| 205,853 | inline script #5: one more URL with `m=ebdd,sps,l,spit,m_i,t,it,anc` (a different module list, used later) | |

The `<link>` URL and the `GM_JS_URLS` URL are the same text. So a download that the scan starts for the link has the key that the later `<script>` element looks for (`Net::key(Expect::Script, url)` in `browser_loader.rs`).

### 8.3 Per big script: source, Chrome, bare

Times are seconds. "First byte" and "last byte" are those of the document. Chrome: cache off, service worker bypassed (cold, like bare), 3 runs. bare: 7 runs (the 4 of the first-row probe and the 3 of the `_jsl` probe). Chrome with its service worker is in 8.5.

| Script | Source in the document | Chrome: request time and initiator | bare: request time | Run (both browsers) |
| --- | --- | --- | --- | --- |
| `m=b` (1.46 MB) | preload link at byte 5,807; `GM_JS_URLS` entry (byte 70,908); `_GM_loadJS()` makes the element (call at byte 233,464) | first byte +0.001 (0.610, 0.440, 0.503), last byte −1.01 to −1.18. Initiator `parser`, link tag, line 11 col 1220. `initiatorType` in the page: `link` | first byte +0.76 (0.70 to 0.83), last byte +0.023 (0.020 to 0.026). The element that `_GM_loadJS()` made | by `_GM_loadJS()`, after the body in bare; during the body in Chrome |
| `m=a` (1.42 MB) | preload link at 6,156; `GM_JS_URLS` (71,244); `_GM_loadJS()` | first byte +0.001, last byte −1.01 to −1.18. Initiator `parser`, line 11 col 1580 | first byte +0.76, last byte +0.023 (0.020 to 0.026) | same |
| `m=m,m_i` (17 KB) | preload link at 6,516; `GM_JS_URLS` (71,601); `_GM_loadJS()` | first byte +0.001, last byte −1.01 to −1.18. Initiator `parser`, line 11 col 1946 | first byte +0.80, last byte +0.062 (0.020 to 0.142) | same |
| `m=ebdd,sps,l,spit,t,it,anc` (2.03 MB) | preload link at 6,882; `GM_JS_URLS` (71,964); `_GM_loadJS()` | first byte +0.001, last byte −1.01 to −1.18. Initiator `parser`, line 11 col 2337 | first byte +0.81, last byte +0.069 (0.020 to 0.151) | same |

Notes on the table.
- bare requests `m=b` and `m=a` 0.02 s after the last byte (the round after `_GM_loadJS()`), and `m=m,m_i` and `ebdd` 0.02 to 0.15 s after it (the same round in 4 of 7 runs; later in 3 of 7, hypothesis: a limit of the worker slots).
- The other 45 to 58 `mail-static` scripts that Chrome requests (`m=f`, `m=mc0RAf`, and so on) have the initiator `script`, and the top frame is code inside `m=b`. They are lazy modules that `m=b` loads when it runs. They are not in the document at all (they are the "later code" case).
- Chrome's byte offset of the request is 5,807 to 6,882 of about 2.6 MB, which is the first piece. bare's is the last byte.

### 8.4 Chrome and bare timelines

Chrome, cold, 3 runs (seconds after the request of the document; `_jsl` is the moment the page sets `_jsl = true`, that is, all four scripts ran and `_B_init` returned; "row" is the first `tr.zA`):

| Run | First byte | Byte 233,139 (loader call) | Four scripts requested | `_jsl` | Row | Last byte | `DOMContentLoaded` |
| --- | --- | --- | --- | --- | --- | --- | --- |
| 1 | 0.610 | 0.841 | 0.610 | 1.025 | 1.530 | 1.625 | 1.682 |
| 2 | 0.439 | 0.567 | 0.440 | 0.752 | 1.247 | 1.488 | 1.532 |
| 3 | 0.502 | 0.565 | 0.503 | 0.822 | 1.335 | 1.686 | 1.754 |

Chrome runs the four scripts 0.60 to 0.86 s **before** the last byte, and shows the first row 0.10 to 0.35 s before the last byte. The data pieces that had arrived at the row end at the end of inline script #14 (about 1.06 MB of 2.64 MB), so the row does not wait for the 1.5 MB script #15. The delay from the loader call to `_jsl` is 0.18 to 0.26 s, and to the row 0.68 to 0.77 s.

bare, 7 runs (seconds after the request of the document; the piece times come from the dump of each piece, the other times from the probe):

| Run | First byte | Byte 233,464 | Last byte | Window (last byte − byte 233,464) | CPU busy from (after last byte) | `_jsl` (after last byte) | Row (after last byte) |
| --- | --- | --- | --- | --- | --- | --- | --- |
| row probe 1 | 1.187 | 1.501 | 1.997 | 0.495 | +0.11 | | +2.29 |
| row probe 2 | 1.084 | 1.387 | 1.762 | 0.375 | +0.14 | | +2.21 |
| row probe 3 | 0.564 | 0.792 | 1.357 | 0.565 | +0.15 | | +2.27 |
| row probe 4 | 1.173 | 1.478 | 1.884 | 0.405 | +0.15 | | +2.21 |
| `_jsl` probe 2 | 1.227 | 1.544 | 1.983 | 0.438 | +0.12 | +0.71 | |
| `_jsl` probe 3 | 1.125 | 1.423 | 1.801 | 0.377 | +0.20 | +0.81 | |
| `_jsl` probe 4 | 0.592 | 0.822 | 1.355 | 0.533 | +0.15 | +0.75 | |

"CPU busy from": the time after the last byte at which 0.5 G more instructions have run (`perf stat -I 100`; the bins are 0.1 s, so the error is 0.05 s). Instructions at the last byte: 0.15 to 0.34 G (the parse of the streamed text, #567). From the last byte to `_jsl`: 5.5 to 5.6 G instructions (0.71 to 0.81 s). To the row: 16.4 to 16.7 G (2.21 to 2.29 s). So the work after `_jsl` is about 10.9 G, 1.5 s. The row comes at the last byte + 2.2 to 2.3 s in every run (spread 0.08 s), whatever the first byte (0.56 to 1.23 s).

Where the 0.71 to 0.81 s to `_jsl` goes (hypothesis, from the idle gap and the instruction counts, not from a profile): about 0.15 s the CPU waits for the scripts (they are requested at the last byte; `m=b` is ready at +0.10, the last of the four at +0.11 to +0.32), and about 0.6 s the four scripts run (5.6 G at 8 to 9 G/s, the top-level code of `m=b`, `m=a` and `ebdd`, the lazy parse of what they call first, and `_B_init`).

### 8.5 Chrome with its service worker (cache on or off)

6 runs. The service worker serves a 311 KB shell at 0.005 to 0.015 s. The four preload links are in it at the same offsets. The four scripts are requested at 0.006 to 0.017 s, initiator `parser`, and the worker answers from its cache (0.007 to 0.155 s). `_jsl` is at 0.07 to 0.27 s. The data document (2.34 MB, a hidden frame) streams in 0.46 to 1.69 s. The row is at 1.03 to 1.15 s. With the cache on or off the times differ by under 0.1 s. bare has no service worker, so it cannot take this path (design section 1.6). The cold runs of 8.4 are the fair comparison.

### 8.6 What Stage 2 can give at most

Method. The first row is the last byte + the idle wait + the CPU work to `_jsl` + the CPU work after `_jsl`. Stage 2 lets the work start when `_GM_loadJS()` runs, which is the "window" before the last byte. Facts that the arithmetic needs, from 8.4:
- the CPU work to `_jsl` is 5.6 G, about 0.65 s: more than the window (0.37 to 0.57 s), so the whole window can be used;
- the data that the work after `_jsl` needs is not late: the end of #14 (byte 819,584 in bare) arrives at the last byte − 0.1 to −0.4 s, and `_jsl` would come after it in every run (the loader call is at the last byte − 0.37 to −0.57 s, and the work to `_jsl` takes 0.65 s); Chrome's row also comes before the last byte, so the row does not wait for #15;
- the tree builder already runs while the body streams (#567), so the parse that Stage 2 would share the thread with is 0.15 to 0.34 G (0.02 to 0.04 s).

| Case | What changes | Gain on the first row (mean; range of 7 runs) | First row (from 4.29 s) |
| --- | --- | --- | --- |
| P. Request each big script at Chrome's offset (the first piece), run them as now | The fetch is done before the last byte. The CPU starts at the last byte + 0.02 s (inline scripts and `DOMContentLoaded`), not at +0.15 s | 0.13 s (0.09 to 0.18) | about 4.15 s |
| S2. Stage 2 alone (scripts run during the body, requests as now: at the loader call) | The work starts at the loader call (the window earlier) and still waits for the fetch (0.15 s) | 0.46 s (0.37 to 0.57) | about 3.8 s |
| S2+P. Stage 2 and the preload rule | The fetch is done before the loader call; the work starts at once | 0.58 s (0.50 to 0.70) | about 3.7 s |

The hypothesis said "up to 0.8 s". The measured bound is **0.58 s** (0.50 to 0.70 s) with both changes, and 0.46 s with Stage 2 alone. The number is the window (0.46 s) plus the idle wait (0.13 s). It cannot be larger, because the loader call is at byte 233 KB: 0.3 s after the first byte, and the body then has 0.37 to 0.57 s left. With a faster server (first byte 0.56 s) the body has the same length (0.8 s), so the window is the same. The computation assumes that the CPU runs the same instructions at the same rate (8 to 9 G/s) in the window as after it. It does not include the cost of the new code path (a shared document).

What none of this changes: the work after the scripts (about 16.5 G, 2.0 s on one thread). With S2+P the first row of bare is about 3.7 s. Chrome's is 1.2 to 1.5 s. The gap that is left (2.2 to 2.5 s) is JS speed (parts C of section 5), not the order of the network.

### 8.7 Options

| Option | How | Gain | Complexity | Risk |
| --- | --- | --- | --- | --- |
| **P. A preload rule in the scan** | In `Scan::start`, a `<link rel=preload as=script href>` starts `Expect::Script` for its URL, as a `<script src>` does. `Net::fetch` finds it by its key, when the `<script>` element that `_GM_loadJS()` makes asks for the same URL. (The same for `as=style`, if wanted: `sheets.rs` keeps sheets by URL.) | 0.13 s (0.09 to 0.18) on Gmail. The same rule helps any page that adds its scripts by code and names them in `<link rel=preload>` (the loader of a bundler). It does nothing for a page with only static scripts. | Small: about 6 lines in `scan.rs`, two tests (the download starts at the first piece; a link that no script takes does not block `Page::idle` and does not hold memory beyond `WINDOW` and the byte cap). | Low to medium. `preload.rs` skips the links of the parsed document on purpose ("bare has no HTTP cache, so a fetch would only cost time and memory"). A preload that no script uses stays in `Net::started` (at most 32 entries, counted in the byte cap) and counts as a fetch. To check: `Page::pending` and `born` must not wait for an unused preload. `crossorigin` and credentials are not matched (the page has no CORS mode). |
| **S2. Stage 2: run the blocking inline scripts while the body streams** | The plan of #567 (a) to (e): a `Document` shared by the tree builder and the realm, an incremental `discover`, a `Run` that starts before the tree ends, `document.write` at the insertion point, a new rule for the virtual clock. | 0.46 s alone, 0.58 s with P (13.5% of 4.29 s) | Large: a rewrite of the front half of `browser_loader.rs` (about 2,000 lines and its tests). | High: it changes the order of effects of every page that has a blocking script (the same page can then show a half tree to a script, as in Chrome). The gain is for pages with a long body and an early script: here a body of 0.8 s. A page of 160 KB (Wikipedia, #567) streams in about 0.2 s, so the window is small. The replay tape and the benchmark clock need a new rule. |
| **L. An early runner limited to the scripts before the first parser-blocking point** | Run only the first inline scripts (here #0 to #7) early, in a realm over the tree built so far | The same as S2 for Gmail, since `_GM_loadJS()` is the 8th script | Not much smaller than S2. Every script of the prefix can read or change the tree (inline script #7 appends to `document.body`), so the realm must see the live tree. There is no "limited" version that avoids (a), (c) and (e) of Stage 2. | The same as S2, plus a rule for where the prefix ends. Not recommended: it has the cost of S2 and the gain of S2, and adds a rule. |
| **U. Start the fetch of URLs that appear in inline script text** | Scan inline script text for URL-like strings (here `GM_JS_URLS` at byte 70,908) and start them | The same request time as P (+0.03 s: the array arrives at 1.215 s, the links at 1.187 s), so 0.1 s | Small, but a guess about program text | Not recommended. P gets the same four requests from an explicit hint of the page. U would fetch strings that the page never loads (wasted bandwidth, requests the page did not make, a side effect on a server). |

### 8.8 Recommendation

**Build option P. Do not build Stage 2 now.**
- P costs about 6 lines and gives 0.13 s on Gmail (3% of 4.29 s). It is the only change that moves the *request* to Chrome's offset.
- Stage 2 gives at most 0.58 s (13.5%) on Gmail, and only on pages with a long body and an early blocking script. It costs a rewrite of the front half of the loader and changes the order of effects of every page. On the 47 journeys it gives nothing that can be measured: they use the virtual clock and see no stream at all. On live pages with a body of about 0.2 s the window is under 0.2 s (hypothesis: not measured).
- With Stage 2 the first row is still about 3.7 s against Chrome's 1.2 to 1.5 s. The remaining gap is 16.5 G of JS on one thread (designs C1 to C4 of section 5).
- The gain of Stage 2 does not shrink if JS gets faster: it stays the window (0.4 to 0.6 s, as long as the work to `_jsl` is longer than the window). So it gets a larger share of the first row after the JS work shrinks. If the owner wants it later, the order is: P, then the JS work, then Stage 2 when the window is 15% or more of the first row. This is a recommendation. The owner decides.

### 8.9 Not measured

- Stage 2 itself (nothing was built): the gain of 8.6 is an upper bound from the measured window, idle wait and instruction counts. The cost of a shared document and of the sharing of one thread is not in it.
- bare with the preload rule (P): the 0.13 s is the measured idle gap (0.11 to 0.20 s after the last byte) minus the 0.02 s that the inline scripts need. To verify: build P and run 6 pairs.
- Whether the work after `_jsl` has other waits on the late data pieces. Chrome's row comes before the last byte; bare's work after `_jsl` is 10.9 G and starts after #14 has arrived in every run. A page that needs the last piece would give less.
- The idle gap is read from 100 ms bins of `perf stat -I`, so it has 0.05 s of error. The 0.6 s of work to `_jsl` is not split into fetch decode, top-level run and `_B_init`.
- A slow network, a second account, a mailbox that has more mail, and a document that the server sends in another piece size.
- The 8 to 9 G/s rate in the window: bare runs 7 to 9 G/s on one thread in the first seconds (section 1.1); under load it is lower.
