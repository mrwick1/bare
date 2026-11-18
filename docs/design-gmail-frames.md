# Design: child frames and workers as separate realms (Gmail as a whole)

Status: design only. Nothing is built. The owner reviews this document before any code.

Base: main `6c18070b`. Measurement: archMachine, 2026-11-17, raw data in `/data/bare-tmp/gmail-prof-data/` (not in the repository: it holds session data).

## 1. Summary

The owner asked for a study of the whole Gmail journey, not one hot spot. The journey has six steps: load, settle, label and reload, search, account menu, compose and send.

The measurement gives two kinds of gap:

1. **Cost.** The JS interpreter is about 70% of the instructions in every step. `docs/design-gmail-js.md` (P1 to P6) covers it. This document adds no new design for it.
2. **Function.** One step fails in every run: the account menu (0/3 in bare, 3/3 in Chrome). Its cause is a design limit, not a bug: bare has one realm per page. An iframe with a `src` is never loaded. A worker never runs. `window.postMessage` drops its message. A `MessagePort` transfer list stops the page with `Unsupported`.

Gmail is a set of documents that talk to each other. Chrome shows 9 to 10 frames on this journey. bare loads none of them. Every part of Gmail that lives in a frame of another origin is absent or broken in bare: the account menu, the apps menu, chat, calendar and the side panel.

Recommendation: **D1. A child frame or a worker is a separate page with its own heap, on the same thread. Messages cross by structured clone into bytes. Ports are moved, not copied.** This is the model of every engine that runs cross-origin frames apart (Chrome, Firefox, Servo, Ladybird for workers). It needs no change to the JS engine's one-heap design. It removes the whole class of "the frame does not run" failures. It does not make the same-origin frame case exact; section 6 keeps that for later.

Load cost is the risk. Chrome loads 50 MB of JS on this journey against 10 MB in bare, mostly for the frames (calendar, chat, contacts). Section 5.4 sets a load rule so that bare runs only the frames that the page uses.

## 2. Measurement

### 2.1 Method

- bare: main `6c18070b`, release build with symbols, `google/youtube` account, saved session. 3 runs per step.
- Chrome: the `chrome-auto` profile, with the cache and service worker on ("warm") and off ("cold"). 3 runs per step.
- Instructions: `perf stat -e instructions:u`. bare: the process tree. Chrome: the whole cgroup (all processes). Per step: 100 ms intervals (edge error about 0.1 s).
- Memory: bare VmHWM. Chrome summed PSS minus the baseline before the run.
- Load average 0.9 to 3.2 (one run started at 10.4). Instruction counts hold under this load. Wall time is an upper bound.

Confounds:
- Google refused a headless sign-in of `google/youtube` in Chrome. So Chrome ran on the primary account. The two accounts get different mailboxes and different UI parts. Compare the failures and the shape of the cost, not exact ratios.
- The label in step 3 failed in both browsers (see 2.3).

### 2.2 Per step (median, range of 3 runs)

| Step | bare: result, wall, instructions | Chrome warm: result, wall, instructions |
| --- | --- | --- |
| 1. Open to first inbox row | 3/3, 10.9 s (7.6 to 13.0), 15.4 G (15.37 to 15.44) | 3/3, 4.0 s (2.5 to 7.2), 21.7 G (15.2 to 22.8) |
| 2. Settle (idle 2 s, or 30 s) | 3/3, 30.1 s (never idle), 75.6 G (67.1 to 99.4) | 3/3, 18.6 s (idle), 37.0 G (30.5 to 58.4) |
| 3. Label, reload, persisted | 0/3, 92 s, 111 G; reload to row 49.8 to 52.0 s | 0/3, 45 s, 56 G; reload to row 1.7 to 7.7 s |
| 4. Search | 3/3, 2.3 s, 9.6 G | 3/3, 2.2 s, 11.5 G |
| 5. Account menu | **0/3**, menu never opens | 3/3, 0.4 s, 3.8 G |
| 6. Compose, send, in Sent | 3/3, 13.6 s, 59.6 G | 2/3, 12.4 s, 88.7 G |
| Whole journey | 364 G (363 to 385); VmHWM 568 MiB | 233 G (173 to 310); PSS about 1.2 GiB |

Chrome cold, step 1: 3/3, 20.1 s, 29.1 G.

Notes:
- bare's step 1 uses fewer instructions than Chrome's whole cgroup, but Chrome does more work: 10 frames and 50 MB of JS against bare's 10 MB.
- bare never reaches idle in step 2. `signaler-pa` keeps 19 to 141 requests pending at the end, and `mail.google.com` keeps 88 to 101.
- bare sends 15 `bscframe` requests per run. Chrome sends 2. bare posts 37 to 39 `jserror` reports per run. Chrome posts 14 to 31.

### 2.3 Failures

| Failure | Runs | Bare only? | Cause |
| --- | --- | --- | --- |
| Account menu: the click sends the top page to the iframe URL `ogs.google.com/u/0/widget/account?...`. The page stops with `Unsupported("MessagePort.postMessage transfer list")` | 3/3 (one run failed earlier on DNS) | Yes | One realm: the `account` iframe has no document, and the port protocol is refused. Section 3. |
| Label: no save request (`POST /sync/u/0/i/s`) in 30 s; label gone after reload. bare throws `Error: int64` 9 times per run | 3/3 | Not proven: Chrome also sent no save (hypothesis: the automation profile) | Open: #568. Only bare throws `int64`. |
| DNS: `DNS query to 127.0.0.53:53 failed: Resource temporarily unavailable (os error 11)` | 3, 5 and 9 requests per run; once the top document | Yes | `crates/net/src/dns.rs`: a blocking UDP socket with a 2 s read timeout, 2 tries. On Linux a timed-out `recv` returns EAGAIN, so this is "no answer in 2 × 2 s". Why the local resolver does not answer: hypothesis (a burst of parallel cache misses for the same name). Not a design topic: a bug to file. |
| `mail.google.com` "the server stopped answering fetch" at about 30.4 s | 1 per run | Yes | Not studied. Hypothesis: a long poll that bare's read timeout ends. |

### 2.4 Subsystem split (bare, `perf record`, 2 recordings)

| Subsystem | Step 1 | Step 3 | Whole run |
| --- | --- | --- | --- |
| JS interpreter | 70 to 71% | 69 to 71% | 72 to 73% |
| JS parse | 15% | 4.5 to 5.4% | 5% |
| DOM bindings | 2% | 7 to 9% | 7 to 8% |
| Allocator | 4% | 5% | 5% |
| Style | 1.5% | 5% | 4% |
| Layout | 0.4% | 2.5% | 2% |
| GC | 1.5% | 1.5% | 1.6% |
| Network | 2.6% | 1% | 1% |
| Paint, HTML and CSS parse | under 1% | under 1% | under 1% |

`<&str as Pattern>::is_contained_in` is 2.9 to 4.1% of step 3 (the `str::contains` of P4 in `design-gmail-js.md`, not yet removed).

### 2.5 What the gap is made of

| Gap | Size | Covered by |
| --- | --- | --- |
| Interpreter speed | about 70% of 364 G; settle 2x Chrome | `design-gmail-js.md` P3, P6 |
| Reload 50 s against 2 to 8 s | 1 step | Not designed. Chrome reloads from its HTTP cache and service worker. bare has neither. A later study. |
| Frames and workers do not run | 1 step fails; menus, chat, calendar and the side panel are absent | **This document** |
| DNS timeouts | any step | A bug: file it |

The frames gap is the largest function gap that no document covers. That is why this document is about it.

## 3. The current design

| Part | Today | File |
| --- | --- | --- |
| Realm | One `Interp` owns one `Heap` and one global. A page has one `Interp`. | `crates/js/src/eval.rs:552`, `heap.rs:769` |
| iframe, blank | Shares the parent realm: `w.Array === Array`. Own `window`, `document`, `location`. | `browser_frames.rs`; decision "A blank iframe window shares the parent realm (2026-11-12)" |
| iframe with a `src` | An inert window. `contentDocument` is `null`. No fetch, no script. | `browser_frames.rs` |
| `window.postMessage` | Drops the message. | `browser_frames.rs` |
| `MessageChannel` | Both ports in one realm. Structured clone in one heap. A transfer list with items is `Unsupported`, which ends the page. | `browser_scheduler.rs:916-936` |
| `structuredClone` | The same: a transfer list is `Unsupported`. | `browser_encoding.rs:41` |
| `Worker`, `SharedWorker` | The object exists. A task later fires `error`, as for a script that does not load. | `browser_worker.rs` |
| `navigator.serviceWorker` | Absent. | |

Cost model today: a frame costs nothing. That is also why the frame's part of Gmail does not exist.

What Gmail does with frames on this journey (Chrome): `ogs.google.com` (account and apps menus), `chat.google.com/u/0/frame`, `calendar.google.com`, `contacts.google.com` (hover cards), `studio.workspace.google.com` (side panel), `accounts.google.com`, and hidden helper frames. They talk to the top page by `window.postMessage`, and then by a `MessageChannel` whose port is sent in the transfer list. The menu widget does that: the error text names the call.

## 4. How other engines do it

Read from the engines' public design documents and source layout. Details marked "to verify" were not re-read for this document.

| Engine | Frame of another site | Same-site frame | Worker | Cross-realm messages and ports |
| --- | --- | --- | --- | --- |
| Chrome (Blink, V8) | Site isolation: another renderer process. The parent holds a `RemoteFrame` proxy that has only the cross-origin window API. | Same process and the same V8 isolate (one heap). Each frame has its own V8 context (its own global and built-ins). Direct access is allowed. | Its own thread and its own V8 isolate (a separate heap). | `SerializedScriptValue`: the value is written to bytes in the sender and read into the target heap. A `MessagePort` is an endpoint of a message pipe; a transfer moves the endpoint. `ArrayBuffer` transfer detaches the source. |
| Firefox (Gecko, SpiderMonkey) | Fission: another content process. The `BrowsingContext` tree lives in the parent process; each document has a `WindowGlobal` actor. | One process, one runtime, one compartment per origin; frames in it are realms. | Its own thread and its own JS context. | `StructuredCloneHolder`: bytes plus a list of transferred objects. Ports move as identifiers. |
| WebKit (JavaScriptCore) | One process per page for most of its history; site isolation is in progress (to verify). | One VM, one global object per frame. | Its own thread and its own VM. | Serialization to bytes (`SerializedScriptValue`). A `MessagePortChannel` registry outside the page matches the two ends (to verify). |
| Servo | The constellation gives each "event loop" (group of similar-origin documents) its own script thread. A cross-origin frame runs in another script thread. | The same script thread; each document has its own global. | Its own thread and its own runtime. | Structured clone to bytes; the constellation routes ports by id. |
| Ladybird | Frames run in the same WebContent process, each in its own realm in one VM (to verify). | Same. | A separate WebWorker process. | Serialization to bytes; a transferred port carries its transport (to verify). |

What they have in common:
1. **A frame of another origin never shares objects with its parent.** The only link is a message: bytes and moved ports. So it can run in another heap, thread or process. Chrome and Firefox put it in another process.
2. **A worker always has its own heap.** No engine shares a heap between a page and a dedicated worker.
3. **Direct object access exists only between same-origin frames**, and only engines that run several realms in one heap allow it (V8 contexts, SpiderMonkey realms, JSC global objects).
4. **A port is a moved endpoint**, matched in a registry outside both pages.

The bare limit that the 2026-11-12 decision named ("a second realm needs a second heap global and a second copy of every host wrapper") holds only for point 3. Points 1, 2 and 4 need a second *page*, not a second realm in one heap.

## 5. Designs

### D1. A separate page for each child frame and worker (recommended)

**Data structures.**
- `FrameTree` in `Browser`: a `Vec<Child>` indexed by a `FrameId` (u32). A child is `{ parent: FrameId, element: NodeId in the parent, origin, kind: Frame | Worker, page: Option<Box<Page>> }`. `page` is `None` until the load rule (5.4) starts it.
- Each child `Page` has its own `Interp`, `Heap`, `Dom` (a worker: a `WorkerGlobalScope` host with no DOM) and its own network state. It shares the process caches: the DNS cache, the connection pool, the cookie jar and the parse cache (`parse_cache` is thread-local; same thread, so it is shared).
- `RemoteWindow` in the parent heap for a child of another origin: a host object with the cross-origin window API only (`postMessage`, `closed`, `frames`, `length`, `top`, `parent`, `opener`, `window`, `self`, `location` setter, `blur`, `focus`, `close`). Every other read throws `SecurityError`, as in a browser. The child sees its parent as a `RemoteWindow` too.
- `Ports`: a slab in `Browser` of port pairs `{ a: (FrameId, PortSlot), b: (FrameId, PortSlot), queue_a, queue_b }`. A `MessagePort` object in a heap holds only its port id.
- `Wire`: a byte buffer for a message: a tagged stream of values (the HTML structured serialize steps: primitives, strings, `Object`, `Array`, `Date`, `RegExp`, `Map`, `Set`, `ArrayBuffer` and views, `Blob`, `File`, errors, back-references for cycles) plus a list of transferred items (port ids, detached buffer bytes).

**Algorithm.**
1. Parent `postMessage(v, origin, transfer)` or `port.postMessage(v, transfer)`: check the target origin, serialize `v` into a `Wire` in the sender heap, detach each transferred `ArrayBuffer` (its bytes move into the `Wire`) and mark each transferred port as moved (the sender's object becomes neutered). Queue a message task in the target's scheduler.
2. The target task deserializes the `Wire` into its own heap, makes new `MessagePort` objects for the moved port ids, and dispatches `message` with `ports`.
3. One event loop drives all pages on the page thread: the existing `Sched` gets one queue per page; the pump takes the next due task across them in `(due, seq)` order. The virtual clock stays one clock, so replays stay determinate.
4. A child of another origin is laid out as a box of the iframe's size in the parent. Its own layout runs in its page. A hit test inside the box goes to the child page. Paint draws the child's display list in the box (to build after the message part).
5. A worker page runs the worker script with `self`, `postMessage`, `onmessage`, timers, `fetch`, `importScripts` and `close`. A worker on the page thread runs only in its own tasks, so it cannot block the parent mid-task. A worker thread is a later step (5.6).
6. Navigation of the parent drops its children: their pages, heaps and port ends. A port whose other end is gone drops later messages, as in a browser.

**Complexity.** A message: O(size of the value) to write and to read, as in every engine. A task pick across pages: O(log n) with one heap of next-due times (n = live pages, under 20 on Gmail). Hit test into a child: one box test more per level.

**Memory.** A child page costs one heap with its built-ins and host wrappers, plus its document. The cost of an empty bare page is not measured (to measure: RSS of `about:blank` pages, 1 and 10). The built-ins of a realm are made lazily today (`Worker` and `AbortController` are accessors until the first read), so an idle frame stays small (hypothesis). The load rule (5.4) keeps unused frames at zero.

**Expected gain.** Step 5 goes from 0/3 to working, if the widget needs nothing more than its frame, `postMessage` and a port (hypothesis: the error names only the port). The other frame parts of Gmail can then run. Instructions go **up**, not down, for each frame that the page uses: that is work that Chrome does and bare skips today. The load rule decides how much.

**Risk.** Medium. The single-realm code stays as it is for the top page. The new code is the frame tree, the wire format, the port registry and the multi-page pump. The 47 journeys have pages with frames (ads, embeds). With the load rule they keep loading no frame they do not use (to verify on the journeys and the cost matrix).

**Work.** Large, in steps that each work alone:
1. `Wire` serialize and deserialize between two heaps, with transfer. `structuredClone` with a transfer list uses it in one heap: this removes the `Unsupported` of `browser_encoding.rs` and `browser_scheduler.rs` at once.
2. `Ports` registry. `MessageChannel` with transfer in one page.
3. Child page for a cross-origin iframe: load, run scripts, `RemoteWindow`, `window.postMessage` both ways. No paint yet.
4. Hit test and paint of a child.
5. Dedicated worker page on the page thread.
6. Later: `SharedWorker` (one page per origin and name), a worker thread.

### D2. Several realms in one heap (the V8 context model)

**Data structures.** `Interp` keeps one `Heap`, but has a `Vec<Realm>`: each realm has its own global object, its own built-ins (`Object.prototype`, `Array`, `Function` and so on) and its own host wrappers. Every object records its realm (a u16 in the object header) for `instanceof` and for the security check. A cross-origin access goes through a wrapper that allows only the cross-origin window API.

**Algorithm.** A function call switches the current realm to the callee's realm. Built-in lookups (`[[Prototype]]` of a new array, the error constructors) read the current realm. A same-origin frame can read the parent's objects directly.

**Complexity.** One realm load per call more. Every intrinsic access becomes an indirect load.

**Memory.** Shared heap and shared strings: less memory per frame than D1. A larger object header (or a side table).

**Gain.** The same as D1 for cross-origin frames, and exact semantics for same-origin frames with a `src` (`w.Array !== Array`, direct DOM access).

**Risk and work.** High and very large. The engine assumes one global in about 85,000 lines of `crates/js`: the inline caches (shape ids are per heap, but the global name hints in `name_gpos` are per global), the built-in setup, the step budget and the GC roots. Every host wrapper in `crates/bare` is made once per page. The cost rows move with every change in the hot path (the realm load per call). No Gmail step that failed needs it: the menu frame is cross-origin.

### D3. Keep one realm; answer the calls (rejected)

Accept a non-empty transfer list and keep the port in the same realm. Fake a `RemoteWindow` that takes messages and drops them.

This removes the `Unsupported` stop and nothing else. The account widget still has no document and no script, so it never answers and the menu never opens. Each new frame feature of Gmail would need its own fake. This is the "patch per symptom" that the project rules forbid.

### 5.4 The load rule (for D1 and D2)

A frame costs a page. Chrome pays for all frames (50 MB of JS on this journey). bare's goal is the lowest cost that gives the same result. Options:

| Rule | Starts a child page when | Gain | Risk |
| --- | --- | --- | --- |
| R1. Eager | the iframe is in the tree with a `src` | Exact. | The full Chrome cost: calendar, chat, ads frames that the agent never uses. |
| R2. On use | the parent posts a message to it, an agent acts in its box, or the frame becomes visible with a size above 0 × 0 | Frames that nobody talks to and nobody sees cost nothing. | A frame that must start first and talk first (it posts to its parent on load) is late. Hidden helper frames of this kind exist in Gmail (hypothesis). |
| R3. Eager for small, on use for the rest | R1 for frames whose document is under a size budget; R2 else | | A budget is a limit, which the project rules call a sign of a design problem. Not chosen. |

Decision (owner, 2026-11-18): **eager, after parent work.** Every connected iframe with an http(s) `src` gets a child page at once. The fetch may start at once. The child runs its tasks only when the parent has no task due now. This is R1 with an order rule, so a frame that posts to its parent on load is not late. R2 is not built.

Measured facts that led to this: an empty `Browser::new` costs about 5.5 M instructions and 0.9 MiB. A child page that has loaded costs its document, its scripts and its heap on top of that. A page without iframes pays nothing, because no child state exists. Both origins are child pages. `contentDocument` stays `null` for a same-origin frame (D2 would be needed for a live same-origin document).

## 6. Ranking

| Rank | Design | What it gives | Instructions | Risk | Work |
| --- | --- | --- | --- | --- | --- |
| 1 | D1 steps 1 and 2 (wire format, ports, transfer) | No more `Unsupported` stop on a transfer list (Gmail and any page that uses `MessageChannel` or `structuredClone` with transfer) | About 0 | Low | M |
| 2 | D1 step 3 with the eager rule (cross-origin child page, `postMessage`) | Account and apps menus; other frame parts of Gmail can run (hypothesis) | Up, only for frames in use | Medium | L |
| 3 | D1 steps 4 and 5 (paint and hit test of a child; dedicated worker) | Visible frame content; pages that need a worker (OpenStreetMap, svelte.dev: decisions.md) | Up, only for frames in use | Medium | L |
| 4 | D2 several realms in one heap | Exact same-origin frames | Up on every call (a realm switch) | High | XL |
| — | D3 answer the calls | Nothing that works | 0 | Low | S, rejected |

## 7. Checks before and after a build

Before: 
1. The empty-page cost (RSS and instructions of 1 and 10 `about:blank` pages).
2. Which Gmail frames talk on load and which only on use (instrumented Chrome run).
3. Whether any Gmail frame on `mail.google.com` reads its parent directly (decides whether D2 is ever needed).

After each step: the six-step journey, 3 runs, both browsers, the same table as 2.2. The 47 journeys and the cost matrix: no row worse than the noise.

## 8. Separate findings (not this design)

- DNS: no answer in 2 × 2 s from the local resolver, 3 to 9 requests per run, once the top document. File as a bug in `crates/net/src/dns.rs`. Check the burst of cache misses for the same name first (hypothesis).
- `Error: int64` and the missing label save: open in #568. Only bare throws `int64`. Chrome's own save also failed in this setup, so the label result is not proven to be a bare gap.
- Reload 50 s against 2 to 8 s: no HTTP cache and no service worker. A later design.

## 9. Not measured

- Chrome on the same account as bare.
- The cost of an empty bare page; so the memory of D1 is not given as a number.
- The frames that Gmail starts on load and the ones it starts on use.
- Whether the account widget needs more than its frame, `postMessage` and a port.
- Wall time on an idle machine.
- The engine details marked "to verify" in section 4.
