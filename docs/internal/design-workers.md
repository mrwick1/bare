# Design: dedicated workers that run (Microsoft Teams first)

Status: approved by the owner on 2026-11-24. Build W1 in the steps of section 5.

Base: main `62ad2894`. Measurement: archMachine, 2026-11-24, release `bare-drive`, Teams signed out (`https://teams.microsoft.com/v2/`), 30 s.

This document is step 5 of D1 in [design-gmail-frames.md](design-gmail-frames.md) ("Dedicated worker page on the page thread"). D1 is the chosen model: a child frame or a worker is a separate page with its own heap, on the same thread. Steps 1 to 3 (clone between heaps, ports, child pages for iframes) are built (`browser_child.rs`, `browser_port_hub.rs`). This document gives the worker step in detail.

## 1. Summary

Teams v2 does not start in bare. After the TLS fix (#999), `window.close` (#1000) and `BroadcastChannel` (#1002), the page loads its app bundle and its auth code, then stays on the loading spinner. The first hard stop is the worker: Teams makes a `Worker` for its data layer, and bare's `Worker` never runs a script (`browser_worker.rs`). Teams logs `ExperienceLoader: Error in worker` and waits.

Recommendation: **W1. A dedicated worker is a child page of the kind `Worker`: its own `Browser` and heap, on the page thread, driven by the child driver of `browser_child.rs`. Its global is the page global with the window-only names removed and the worker names added.** It reuses the load, the clone between heaps, the outbox, the port hub, the clock, the budgets and the diagnostics that iframes use today. It needs no change in Vex.

Load cost is the risk. The Teams worker loads about 19.6 MB of JS. Section 6 lists the checks before the build.

## 2. Measurement

Teams console after #1002 (unique lines; `tape3` replay patched to print the reason of each unhandled rejection):

| Line                                                                                        | Cause                                                |
| ------------------------------------------------------------------------------------------- | ---------------------------------------------------- |
| `Worker scripts do not run in bare (…/v2/worker/precompiled-web-worker-….js#{…})`           | The data worker is made and never runs.              |
| `ExperienceLoader: Error in worker: undefined`                                              | The `error` event of the stub (`browser_worker.rs`). |
| `Failed to pre-load CacheStorage` (2 names)                                                 | No `caches` (warning only).                          |
| `Unhandled error/rejection cannot read properties of undefined (reading 'getRegistration')` | No `navigator.serviceWorker`.                        |
| `Unhandled error/rejection [_getUserIdentifier] user missing`                               | Signed out (expected).                               |
| `Unhandled error/rejection false`                                                           | Not known.                                           |

The page makes no request to `login.microsoftonline.com` in 30 s. Chrome sends a signed-out user to the sign-in page. Hypothesis: the auth start waits for the worker. To verify after the worker runs.

### 2.1 What the Teams worker uses

The worker script (`precompiled-web-worker-<hash>.js`, 193 KB) is a webpack loader. It reads a manifest and loads six chunks with `importScripts` from `teams.public.onecdn.static.microsoft/teams-modular-packages/hashed-assets/`: 19.6 MB of JS in total (1.3 to 4.4 MB each). A count of names in the six chunks (text matches, not calls that run):

| Name                                                                          | Count                | In bare today                     |
| ----------------------------------------------------------------------------- | -------------------- | --------------------------------- |
| `performance.now`                                                             | 439                  | yes                               |
| `fetch(`                                                                      | 93                   | yes                               |
| `WebSocket`                                                                   | 77                   | yes (`shims/websocket.js`)        |
| `.postMessage(`                                                               | 71                   | ports yes; worker global no       |
| `indexedDB`                                                                   | 64                   | yes (`shims/idb.js`, in memory)   |
| `Intl.`                                                                       | 33                   | yes                               |
| `Blob(`                                                                       | 29                   | yes                               |
| `typeof window`                                                               | 24                   | must be `"undefined"` in a worker |
| `XMLHttpRequest`, `TextEncoder`                                               | 24 each              | yes                               |
| `MessageChannel`                                                              | 20                   | yes                               |
| `Atomics`, `SharedArrayBuffer`                                                | 13, 9                | to verify                         |
| `caches.`                                                                     | 12                   | no                                |
| `FileReader`                                                                  | 10                   | yes                               |
| `WebAssembly`                                                                 | 5                    | no                                |
| `importScripts`                                                               | 3 (+1 in the loader) | no                                |
| `navigator.storage`, `crypto.subtle`, `CompressionStream`, `BroadcastChannel` | 2 to 3 each          | partly                            |

So most of the API that the worker needs exists in a bare page now. What a page global lacks is the worker shape: `importScripts`, a global `postMessage(message, transfer)` and `onmessage`, `close()`, and no `window` or `document`.

## 3. Current state

- `Worker` and `SharedWorker` (`browser_worker.rs`): the object exists. A task after the constructor fires `error`. `postMessage` sends nothing. See the module head and `docs/internal/decisions.md`.
- Child pages (`browser_child.rs`): one `Browser` for each iframe with an http(s) `src`, on the page thread. A slot of `Kids` is keyed by the iframe `NodeId`. The driver (`kids_turn`) loads the document, makes the `Browser` (`child_browser`), runs it when the parent has no task due, moves messages (`Kids::outbox`, `Side::{Child, Parent, Top}`), takes port wires and moves console lines to the parent.
- Clone between heaps: `clone_state_with` gives a heap-free `Cloned`. Ports cross as hub ends.
- Network inside a task: a synchronous `XMLHttpRequest` stops the page with `Unsupported` (`browser_xhr.rs`), because the pump is the only place that waits for the network. The net client can do a blocking download (`docs/internal/decisions.md`, "Worker threads, not async I/O").

## 4. How other engines do it

QuickJS (`quickjs-libc.c`, release 2026-06-04, `os.Worker`): `new os.Worker(module)` starts a POSIX thread with a new `JSRuntime` and `JSContext`, and loads the module there (`worker_func`). The worker global is a normal context global with the same helpers as the main one; the parent is `os.Worker.parent`. `postMessage` writes the value to bytes (`JS_WriteObject2`, with references and `SharedArrayBuffer` by reference count), copies them into a queue under a mutex, and wakes the receiver through a pipe. The receiver reads the bytes into its heap (`JS_ReadObject`) and calls `onmessage` with `{ data }`. No `importScripts` (modules only). A worker cannot make a worker. So QuickJS is W3 below: one runtime and one thread per worker, a byte copy for each message. bare already has the byte copy (`Cloned`); the thread is what bare cannot take today (section 5, W3).


All engines give a dedicated worker its own heap and its own global (`DedicatedWorkerGlobalScope`), on its own thread. Messages cross by structured serialize. `importScripts` is a synchronous fetch and run, on the worker thread. (Chrome, Firefox, Servo, Ladybird: see section 4 of design-gmail-frames.md.) bare runs all pages on one thread; a worker on the page thread runs only in its own tasks, so it cannot stop the parent in the middle of a task. The page sees the same order of events as with a slow worker thread.

## 5. Designs

### W1. A worker is a child page with a worker global (recommended)

**Data structures.**

- `Slot` gets a kind: `Frame(NodeId)` or `Worker(Obj)` (the `Worker` object in the parent heap). A worker slot has the script URL and the options (`type`, `name`).
- `WorkerHost.objs` maps a worker object to its slot index (`FrameId`) instead of the `error` flag.
- The child `Browser` of a worker: an empty document (`<!doctype html><html><head></head><body></body></html>`), document URL = the script URL, no viewport work.
- No new message type. A message to the worker is an `Out` with `to: Side::Child(id)`. A message from the worker is an `Out` with `to: Side::Parent`, delivered at the `Worker` object (not at the window), with `origin` `""` and `source` null.

**Algorithm.**

1. `new Worker(url, options)`: the checks of today (URL, origin, cap). Then make a slot of kind `Worker`. The object is an `EventTarget`, as now. No `error` task.
2. The driver loads the script as it loads a frame document, with the script rules: `Sec-Fetch-Dest: worker`, a JavaScript MIME type, the size cap. A failure fires `error` at the object (the behaviour of today).
3. `child_browser` for a worker makes the empty document, then changes the global: it deletes `window`, `document`, `parent`, `top`, `frames`, `opener`, `frameElement`, `localStorage`, `sessionStorage`, `alert`, `confirm`, `prompt`, `print` and the window `postMessage`. It adds `importScripts`, `postMessage(message, transfer)`, `onmessage`, `onmessageerror`, `close()`, `name`, and the interface names `WorkerGlobalScope` and `DedicatedWorkerGlobalScope`. It sets the prototype of the global to a new `DedicatedWorkerGlobalScope.prototype`, whose chain is `WorkerGlobalScope.prototype`, then `EventTarget.prototype` (decision 7.1). `self` stays the global. `location` and `navigator` stay (a worker has both).
4. The driver runs the script as a classic script (or a module when `type` is `module`), then gives the worker turns as it gives a frame turns: only when the parent has no task due.
5. `importScripts(urls…)`: parse each URL against the worker URL, then for each one a blocking download through the page client (the same rules as step 2), then run it in the global. An error throws `NetworkError` (a `DOMException`). This blocks only the worker turn. A blocking call is legal here: the spec makes `importScripts` synchronous.
6. `worker.postMessage(v, transfer)` in the parent: clone with `clone_state_with`, ports become hub ends, push an `Out` for `Side::Child(id)`. The driver moves it before the turn of the worker and queues it at the worker global, as `queue_window` does.
7. `postMessage(v, transfer)` in the worker: the same, with `Side::Parent`. After the turn, the parent queues it at the `Worker` object (a port with no peer, as the window is: `queue_port` from #1002).
8. `terminate()` in the parent, `close()` in the worker, or the GC of a `Worker` object that has no listener and no queued message: the driver drops the child `Browser` and its hub ends.
9. Settle: a live worker blocks settle only while it has a task due or a message in flight, as a frame does (`Kids::blocks`).

**Complexity.** A message: O(size of the value) to write and to read, as for frames. A worker turn: one slot more in `kids_turn`. `importScripts`: O(bytes) to download and to parse, the same as a `<script>`.

**Memory.** An empty Vex page costs 2.4 M instructions and 160 KiB of heap (7.1). The empty document of a worker adds about nothing. The Teams worker adds its 19.6 MB of source and its heap. The source text is kept only while it parses, if Vex keeps no copy (to verify).

**Expected gain.** Teams can start its data layer. Then the next blockers become visible: probably the sign-in redirect, `caches`, `navigator.serviceWorker`. Outlook also makes a worker (seen on 2026-11-24). WhatsApp bundles name `Worker` and `SharedWorker` (hypothesis: it needs them). Also the pages of decisions.md that need a worker (OpenStreetMap, svelte.dev). Instructions go **up** for each page that makes a worker: that is work that Chrome does and bare skips today. A page with no worker pays nothing: no slot exists.

**Risk.** Medium.

- The global is a `Window` object with names removed and a worker prototype chain. `self instanceof WorkerGlobalScope` is `true` and `typeof window` is `"undefined"` (Teams tests it in 24 places). `Object.prototype.toString.call(self)` stays `[object Window]` unless the global's own `Symbol.toStringTag` is changed too.
- DOM interfaces (`HTMLElement`, `Node` and so on) stay visible in the worker. A script that tests `typeof HTMLElement` to find a worker takes the window path (hypothesis: rare).
- 19.6 MB of JS through Vex on the page thread. The worker runs only when the parent is idle, so the page is not late, but the journey takes longer.

**Work.** Medium, in steps that each work alone:

1. Worker slot, script load, run, `self.postMessage` and `onmessage` both ways, `terminate`, `close`. Test: a `data:` worker and a same-origin worker answer a message.
2. `importScripts` with a blocking download.
3. Worker global shape: remove the window names, add the worker names.
4. Teams run; next blockers.
5. Later: `SharedWorker` (one child page for each origin and name, connected by ports), a module worker, a worker thread.

### W2. A separate worker host with no DOM

A new host type next to `Dom<E>`: a `WorkerScope<E>` that installs only what a worker has (timers, `fetch`, `XMLHttpRequest`, `WebSocket`, IndexedDB, encoding, crypto, `Blob`, ports, console).

- Gain: the exact global (`self instanceof WorkerGlobalScope`, no DOM interfaces). Less memory for each worker: no DOM wrappers, no document.
- Cost: every host module that a worker needs takes a `&Dom<E>` today (fetch, scheduler, shims, console). Each needs a split between a part that needs a document and a part that does not. That is a refactor of a large part of `crates/bare`, and every touched hot path must pass the cost gate.
- Risk: high. Work: very large. The gain over W1 is only the exact global shape, which no measured page needs yet.

### W3. A worker on its own thread

W1 or W2, but the worker `Browser` runs on its own OS thread.

- Gain: true parallel work; the parent page does not wait for a worker turn.
- Cost: `Dom` and the Vex heap are not `Send`. The child driver, the port hub and `Cloned` assume one thread (design-host-clone.md, question 9: "Should `Cloned` be `Send`? Proposed: no"). The virtual clock and the replay order must stay the same; with threads they need a lock step.
- Risk: high. A later step after W1, if a journey shows that worker time on the page thread matters.

### W0. Keep the stub (rejected)

Teams, Outlook and WhatsApp stay on their loading screens. A fake that answers Teams's first messages would be a patch for one symptom, which the project rules forbid.

## 6. Checks before and after a build

Before:

1. Done for instructions and heap (7.2: 3,813 M, +62 MiB). Still to do: wall time on an idle machine.
2. Whether Vex keeps the source text of a script after the parse.
3. `Atomics` and `SharedArrayBuffer`: what bare gives today, and what Teams needs from them when no cross-origin isolation is set (Chrome then gives no `SharedArrayBuffer`).

After each step:

1. The new tests of the step, on Vex and on the old engine.
2. Teams signed out: the console lines of section 2, 3 runs.
3. The `cost_rows.txt` rows of the touched subsystem (`cost_matrix.sh -g`). A page with no worker must not change.

## 7. Decisions on the open questions

The owner asked (2026-11-24) to decide these by performance, efficiency and build time. Measurements: archMachine, release build of main `62ad2894`, scratch program (not committed), 3 runs each, load average 5 to 9. Instruction counts are exact to 0.01% over the 3 runs. Wall times are under load: upper bounds.

### 7.1 The global of a worker: decided, prototype swap

| Option | Build | Cost | Exact? |
| --- | --- | --- | --- |
| A. Window chain, window names deleted | about 15 lines | 0 | `self instanceof WorkerGlobalScope` is `false` |
| B. A and the prototype of the global set to `DedicatedWorkerGlobalScope.prototype` | about 30 lines | 0 (see below) | `instanceof` exact |
| W2. A host with no DOM | very large refactor | saves at most the empty page: 2.4 M instructions, 160 KiB | exact |

- `Object.setPrototypeOf(globalThis, …)` works on Vex and on the old engine. `delete window` and `delete document` work on both.
- A loop of 3 M iterations with 3 global lookups each: 8,232 M instructions on the window chain, 7,497 M after the swap (−9%; the chain is shorter). The swap costs nothing.
- An empty page costs 2.4 M instructions and 160 KiB of heap (10 pages: 25.2 M, 1,598 KiB). The Teams worker code costs 1,600 times more (7.2). So a worker host with no DOM (W2) saves under 0.1% of the cost of a real worker. It is not worth its build.

Decision: B.

### 7.2 `importScripts`: decided, a blocking download, no prefetch

- How Teams loads: the worker script runs `Promise.all([s.e(a), …5])`. Each `s.e` calls `importScripts` with one URL. All 5 calls run one after another in the same task. A flag (`shouldFetchWorkerChunksBeforeImportScripts`) would first start them all with `import()`, but the production config sets it to `false` (only `local` builds set it). So there is no list of URLs before the calls, and a prefetch of the arguments of one call gains nothing for Teams.
- Download of the 6 chunks (curl, gzip): one connection after another 2.7 to 3.2 s; in parallel 0.7 to 0.9 s; one after another on one kept-alive connection 0.9 to 1.0 s (2.4 s for the first run, with a cold connection). bare keeps connections alive (6 for each server), so a blocking download costs about the same as a parallel one here.
- Parse and run of the 6 chunks (19.6 MB) in one page:

| Engine | Instructions | CPU | Memory |
| --- | --- | --- | --- |
| bare (Vex) | 3,813 M | 0.39 to 0.43 s | +62 MiB heap, 74 MiB peak of the allocator |
| node 22 (V8) | 2,020 M | 0.25 s | 80 MiB RSS (whole process) |
| QuickJS 2026-06-04 | 62,060 M | 4.0 s | not measured |

bare is 1.9 times V8 and 16 times faster than QuickJS on this code. QuickJS compiles every function when it reads a script; Vex and V8 compile a function when it first runs (hypothesis for the gap: to verify with a profile).

Decision: a blocking download through the page client, as the spec says. A prefetch adds code for no measured gain. An HTTP cache would remove the download on a second load (as in Chrome); that is a separate design.

### 7.3 `SharedWorker`: decided, keep the stub

- Teams chooses by its server config, not by a test of the API: `experienceLoaderSettings.preInitWorker` is `true` and `preInitSharedWorker` is not set; `telemetry.enableSharedWorker` is `false` in production.
- A real `SharedWorker` needs one child page for each origin and name, shared by all pages that connect, and a port for each connection: a few hundred lines more than the dedicated worker, and no gain for Teams.

Decision: keep the stub (`browser_worker.rs`). Build it when a page that we need uses it (WhatsApp names it: to verify when WhatsApp is the target).

## 8. Not measured

- The cost of the Teams worker in a real worker page (7.2 measures the parse and the module setup in a window page; the worker start code did not run).
- Chrome on the same journey.
- Whether Teams needs more than the worker to reach the sign-in page.
- The `false` rejection.
