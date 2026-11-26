# WhatsApp Web bring-up: status and the open blocker

Goal: run WhatsApp Web (`https://web.whatsapp.com/`) in `bare`. Link `bare` as a new device by QR. Then read the chat list,
read a conversation and send a text message. This file is the running record. Update it as the work moves.

Last updated: 2026-11-26. Work is paused (owner, 2026-11-26).

## How to run the live journey

The journey `docs/internal/whatsapp/whatsapp-qr.steps` opens the page and waits for the QR code. It needs no login.

```sh
cd ~/coding/personal/projects/bare
cargo build --release -p bare-drive
OUT=/tmp/wa-out            # any writable dir
./target/release/bare-drive --out "$OUT" docs/internal/whatsapp/whatsapp-qr.steps
jq -r '.failure' "$OUT/result.json"
jq -r '.console[] | select(.level=="error" or .level=="exception") | .text' "$OUT/result.json"
```

To read page state, run `bare-mcp` (same build) and call `open`, then `eval`. WhatsApp uses the Facebook module system:
`require('ErrorPubSub').history` lists the errors that the page caught and did not log.

## Done

- **Parsed `data:` scripts** (PR 1). WhatsApp runs its boot steps from `<script src="data:text/javascript;base64,...">`
  elements. `requireLazy(["ServerJSPayloadListener"], m => m.process())` is one of them. Bare skipped every parsed `data:`
  script with no message (`Skip::BadUrl`), so the 28 server payloads (`script[data-sjs]`) were never read. The page stayed on
  the splash screen, idle, with no console error. Fix: a classic script with a `data:` `src` is `Source::Data`
  (`crates/bare/src/resources.rs`). The loader decodes it when its turn comes and runs it with the timing of an external
  script, with `load` and `error` events (`crates/bare/src/browser_loader.rs`, `run_parsed`). Tests:
  `crates/bare/src/browser_loader_data_tests.rs`.
- **`nonce` on `HTMLElement` and `SVGElement`** (PR 1). It was `undefined`. WhatsApp reads `script.nonce` first, then
  `getAttribute('nonce')`, so this was not the blocker. It is a gap that other pages hit. Tests:
  `crates/bare/src/browser_element_nonce_tests.rs`.
- **AES-CTR and HMAC in `crypto.subtle`** (PR 3). WhatsApp makes an AES-CTR key at boot and signs with HMAC SHA-256.
  Both rejected with `NotSupportedError`. Fix: `crates/bare/src/browser_subtle.rs`. Tests:
  `crates/bare/src/browser_subtle_ctr_hmac_tests.rs`.
- **Source text in `Function.prototype.toString`** (PR 2). `WAWeb-curve25519-donna` reads the source of small functions
  with `t.toString().match(d)`. Vex gave `[native code]`, the match was `null`, and the boot threw. Fix: `Code.span` and
  `fn_to_string` (`crates/vex/src/builtins.rs`). Tests: `crates/vex/src/fn_source_tests.rs`.
- **Canvas size and pixels** (PR 4). The QR code is a `<canvas>` that a script makes and sizes with `c.width = 228`. Bare
  had no `width` and `height` members on a canvas and no size for its box, so the QR box was 0x0. After that, the canvas was
  blank: WhatsApp draws the code with `fillRect` and `strokeRect` (a `QRCode` port, `p.prototype.draw`), and bare drew
  nothing. Fix: `width` and `height` reflect their attributes; the box takes them as a hint (`crates/css/src/hints.rs`);
  `fillRect`, `strokeRect` and `clearRect` draw into a bitmap per canvas, and paint shows it like an `<img>`
  (`crates/bare/src/browser_canvas.rs`, `crates/paint/src/display.rs`). Tests: `browser_canvas_size_tests.rs`,
  `browser_canvas_raster_tests.rs`.

Live effect, same journey, before vs after PR 1 (archMachine, 1 run each):

| fact                       | before   | after                                 |
| -------------------------- | -------- | ------------------------------------- |
| requests                   | 11       | 22                                    |
| page idle after            | 1.1 s    | not idle at 90 s                      |
| first page error           | none     | `WAWeb-curve25519-donna` (see below)  |

## State after PR 4

With PRs 1 to 4, `whatsapp-qr.steps` passes: the login page shows a QR code in 15 s (archMachine, 1 run). `zbarimg` reads
the screenshot as a `https://wa.me/settings/li...` link. Next: link a device, then the chat list.

## Link attempt 1 (2026-11-25)

The owner scanned the QR code. The phone and the page did the link handshake. The page showed "Your messages are
downloading", then an error. The phone then showed no linked device for bare.

- The page logged `[backend-worker] worker script failed to load during init`. `WAWebBackendWorkerInitErrorWatcher` reads
  an `error` event that is not an `ErrorEvent` as a script that failed to load. Bare fires a plain `Event`.
- The cause was in the worker: an uncaught `Invariant Violation #56318` in its `message` listener. The full-text-search
  worker bundle (`WAWebWorkerConnect`) asserts `self instanceof DedicatedWorkerGlobalScope`. In a bare worker, that name
  was `undefined` and `Object.prototype.toString.call(self)` gave `[object Window]`.
- The 400 responses on the `init_script` requests did not reproduce with `curl`. Hypothesis: a side effect of the failed
  worker, not a cause.
- Not the cause: `WebAssembly`. WhatsApp uses it (liboqs, media worker), and a design for it is open for owner review.

**Worker global scope** (PR 5, #1076). The worker global gets the interfaces `WorkerGlobalScope` and
`DedicatedWorkerGlobalScope` (illegal constructors) and the chain `self -> DedicatedWorkerGlobalScope.prototype ->
WorkerGlobalScope.prototype -> Window.prototype -> .. -> EventTarget.prototype`. Its tag is `DedicatedWorkerGlobalScope`.
`Window.prototype` stays in the chain, because the functions of the global live there; the standard has no window names in
a worker (`design-workers.md`, step 3). Fix: `crates/bare/src/browser_worker.rs` (`install_scope`). Tests:
`crates/bare/src/browser_worker_scope_tests.rs`.

## Link attempt 2 (2026-11-25)

Build: main with #1076 and WebAssembly PR 1 (#1079). The link handshake worked. The page stayed on "Your messages are
downloading", and the phone failed.

- The backend worker failed to start again. This time the server answered 400 with an empty body to the `init_script`
  requests of `webworker_v1` and `webworker_v4`. Some minutes later, `curl` and an in-page `fetch` got 200 for the same URL.
  On a fresh page, the same requests got 200 again. Hypothesis: a short block on the server side after a failed init.
- `suspicious-host-checks-init` failed: a WebAssembly module exports its memory, and PR 1 refused that. Fix: WebAssembly
  PR 2 (#1084), `Memory` and `Table`.

## Link attempt 3 (2026-11-25)

Build: main with #1084. The link handshake worked, and the backend worker script loaded (no 400). The page then logged two
errors and showed "Logging out".

- `suspicious-host-checks-init-failed`: "The argument must be a WebAssembly.Module". The page compiles the host check module
  (`WAGetUrlSpoofChecksWasm`) and posts it to the backend worker in `{wasmModule}` (`WAWebBackendWorkerInitState`,
  `setSuspiciousHostChecksModule`). The worker calls `new WebAssembly.Instance(t)`. Bare cloned the module as a plain object.
  Fix: structured clone of a `WebAssembly.Module` (WebAssembly step 6).
- `backend-worker-init-fails`: "globals: PN-less setup requires a LID device jid". Not analysed yet. Hypothesis: a
  consequence of the first error.

## Link attempt 4 (2026-11-25)

Build: main with #1085 (structured clone of a `WebAssembly.Module`). The link handshake worked. The page stayed on "Your
messages are downloading". The backend worker failed to load again: the `init_script` requests of `webworker_v1` and
`webworker_v4` got 400, as in attempt 2.

Cause, found with `curl` on new URLs (a new `__spin_t` value for each try, 2 tries for each header):

- The first request for a URL decides the reply, and the CDN keeps it for that URL. Later requests get the same status.
- A first request with only `User-Agent` gets 400. A first request with `Sec-Fetch-Site: same-origin` gets 200. Each other
  header alone (`Accept`, `Sec-Fetch-Dest`, `Sec-Fetch-Mode`, `Referer`, `Accept-Language`, `Accept-Encoding`) gets 400.
- Bare sent the Fetch Metadata only from `fetch()` and on navigations. A `<script src>`, a stylesheet and a worker script
  went out without it. The URL holds a value per page load (`__spin_t`), so each new page load is a new first request.
- Attempt 3 got 200. Hypothesis: another request with the metadata (a `fetch` preload) reached that URL first.

Fix: Fetch Metadata on subresource requests (`resources::fetch`).

## Link attempt 5 (2026-11-25)

Build: main with #1087 (Fetch Metadata on subresource requests). The backend worker loaded. After the link, the page went from
`/login/` to `/` and showed "Logging out". The worker failed its `setup` step again and again with "globals: PN-less setup
requires a LID device jid". Hypothesis: the new document found empty IndexedDB databases, because bare kept the data in the
realm.

Fix: one IndexedDB store per origin in the session (#1091, `design-indexeddb-store.md` steps 1 and 2).

## Link attempt 6 (2026-11-25)

Build: main with #1091. The link held: the page went to `/` and showed "Don't close this window. Your messages are
downloading." The device JID was set (probe: set or null only). No "PN-less setup" error.

Then the page stopped. `browser_diagnose`: `Unsupported("damaged clone record")`. Timers and `MessageChannel` messages did not
run after that, so the download screen stayed.

Cause: a read of a clone record takes the platform objects (a `Blob`, a `CryptoKey`) out of the record. The IndexedDB store
read the same record twice, and the second read found it damaged. The error is fatal, so the page stopped.

Fix: the store writes such a record again from the new copy after each read (`browser_idb.rs` `read`).

## Link attempt 7 (2026-11-25)

Build: main with #1092. The link held, and the phone showed the new device. During the message download the page stopped with
`Limit(Host("dom-events"))`. Two worker scripts also threw `NotSupportedError: Unrecognized algorithm name` from
`crypto.subtle` (not yet traced).

Cause of the stop: every request of an IndexedDB transaction fires a `success` event, and the shim ran all requests of a
transaction in one microtask chain. The host frees events only in a collect, and it collects only between tasks. The download
queues thousands of requests, so the live event cap (16,384) filled.

Fix: the shim yields to a task after each 64 requests (`MessageChannel`).

## Link attempts 8 and 9 (2026-11-25)

Attempt 8. Build: main with #1094. The link held. The page stopped again with `Limit(Host("dom-events"))` during the
download. Cause: not yet known. Attempt 9 runs a build that prints the event types at the cap.

Attempt 9. Build: main with #1097 and the event print. The link held. After 7 minutes the page still showed "Your messages
are downloading", with little CPU use. The event cap was not hit. WhatsApp's error log (`ErrorPubSub`) showed:

- First: `DataError: The parameter is not a valid key` on `get` in table `sync-keys`.
- Then about 40 `TransactionInactiveError` from Dexie, on `bulkCreate` in `syncd-logs` and on reads of `contact`, `chat` and
  `message`.

Cause: the shim checked keys in JS and took only numbers, dates, strings and arrays. A binary key (`ArrayBuffer` or a view)
gave `DataError`. The host coder takes binary keys. `sync-keys` uses binary key ids.

Fix: the shim takes binary keys and compares them byte by byte. To check: whether the Dexie errors stop with this fix.

## Link attempts 10 and 11 (2026-11-25)

Attempt 10. Build: #1099 and #1097 with the event print. The page stopped with `Limit(Host("dom-events"))`. The print showed
16,383 IndexedDB `success` events, all made in one task: the shim yields after 64 requests per transaction, and WhatsApp runs
hundreds of transactions at once. The owner asked to remove caps that make pages fail. Fix: no count cap on events
(branch `no-event-cap`).

Attempt 11. Build: `no-event-cap`. A start with the `default` profile showed "Logging out": the profile held cookies and
`localStorage` but no IndexedDB (step 3 of the profile design is not built). A fresh profile `wa` linked. No `DataError`
and no Dexie error. The download ran for minutes. Then syncd failed 40 times with "HMAC takes SHA-256 only", and WhatsApp
logged the device out.

Fix: HMAC takes SHA-1, SHA-384 and SHA-512.

## Link attempt 12 (2026-11-25)

Build: main with #1101, #1102 and #1104, profile `whatsapp`. The link held. No HMAC error. During the download,
`message.bulkAdd` failed 17 times with `ConstraintError`. Then Dexie transactions failed with `TransactionInactiveError`, and
syncd logged "Calling runInTransaction recursively".

Cause: the shim started every transaction at once. WhatsApp reads a row and adds it when it is missing, so two transactions at
the same time added the same key. Chrome runs overlapping write transactions one after the other.

Fix: the shim orders transactions as the standard says.

## Link attempt 13 (2026-11-25)

Build: main with #1106, profile `wa3`. The chat list loaded. A search for a number found the chat, and the chat opened.
Send cleared the composer, but no message came. `crypto.subtle.digest` gave a promise that never settled. No promise job,
timer or message task ran after an agent `eval`.

Cause: the page had stopped for good. `diagnose` gave the reason: `Unsupported("an XMLHttpRequest response text in a
charset other than UTF-8")`. WhatsApp reads binary data with `overrideMimeType("text/plain; charset=x-user-defined")`.
The shim rejected that charset with a fault, and a fault stops the page.

Fix: `responseText` decodes x-user-defined as the Encoding Standard says.

## Link attempt 14 (2026-11-25)

Build: the x-user-defined fix, profile `wa3` from attempt 13. The page opened from the profile with no new QR scan. The chat
list loaded. The chat opened. A text message sent and got two ticks (`ack` 2, delivered).

Open: the worker error "Unrecognized algorithm name" from SubtleCrypto. Service workers are not supported (not fatal). A
layout panic came once after the page stopped: "layout only asks for the style of a box" (`crates/layout/src/boxes.rs:132`).

## Status on 2026-11-26 (paused)

Owner scope (2026-11-26): send and receive messages, listen to notifications, send media. For received media: view it in
the page. If that fails, download it to a temporary folder, view it and delete it, when that saves memory.

Live builds: main from `bec30b32` to `a7fa6b36`, profile `wa3`. The profile opens with no new QR scan.

| Feature | State | Evidence |
| --- | --- | --- |
| Send text | Works | `ack` 2 (delivered). |
| Receive text and picture live | Works | A text and a JPEG (250x375) came into the open chat with no reload. |
| Read messages | Works | Through the store and the rendered chat. |
| Download a received picture | Works | Viewer download, 11,698 bytes (#1121). |
| Send a document | Works | `bare-test.txt`, `ack` 2 (#1125). |
| Send a photo | Works | JPEG 4,123 bytes, 320x120, caption, `ack` 2 (#1129, #1130). |
| Notifications | Works | First notification in the bare-mcp `notifications` list with the page in background mode. |
| Send a video | Fails | The preview spinner does not end. Send stays off. |
| Send an audio file | Fails | Same spinner as video. |

### Fixes merged on 2026-11-25 and 2026-11-26

- #1113: XHR `responseText` decodes x-user-defined.
- #1115, #1116, #1117, #1118, #1120, #1121: crypto, task aborts, layout, `blob:` images, ResizeObserver, fixed boxes.
- #1124: an unsupported feature throws `NotSupportedError`. The page goes on.
- #1125: an `ArrayBuffer` or typed-array request body sends its bytes. Before, media upload got HTTP 415 (hash mismatch).
- #1126: `DataCloneError` names the type that failed.
- #1122, #1127, #1129, #1130: canvas 2D on bare's own painter, lazy raster, `drawImage`, `getImageData`, `toBlob`, PNG and
  JPEG (`jpeg-encoder`, IJG notice). Step 2 of the canvas plan is stopped (owner).
- #1131: `MessagePort` and `MessageChannel` have their `Symbol.toStringTag`.
- #1133, #1136 to #1139, #1141: notifications. Permission per origin in the profile. Default stays "denied". A bare-mcp
  tool `notifications` (grant, deny, reset, list, wait, click, close, background).
- #1134: an uploaded file gets its media or office type from its extension.
- #1140: stack frames show the script URL.
- #1143: `FileReaderSync` in workers.
- #1148, #1151, #1154, #1156, #1159, #1161, #1163, #1166, #1168: service workers (design S2), notification actions,
  Cache API, `fetch()` and navigation interception.

### Notifications: the cause

WhatsApp's own setting "Messages: Show notifications" was off on this device (`getGlobalNotificationsEnabled()` false).
Hypothesis: WhatsApp saved it off when the device linked, because `bare` answered "denied" then. The fix was the setting in
WhatsApp: Settings (the "You" button), Notifications, Messages, Show notifications. Message notifications use
`new Notification`. Only test and call notifications use the service worker.

Steps for a live check:

1. `notifications {"op":"grant","origin":"https://web.whatsapp.com"}`, then open the page again with the profile.
2. `notifications {"op":"background","on":true}`.
3. `notifications {"op":"wait","timeout_ms":900000}`. The wait blocks the other tool calls of the session.

### Open items

- Video and audio send: the preview waits on a `<video>` element from a `blob:` URL. `bare` has no media decoder, so the
  element fires `error` (code 4) as Chrome does for a source it cannot play. The cause of the endless spinner is not found.
- Worker errors, cause not found:
  - `MessagePort object could not be cloned.` at `iMur7JVH-Cm.js:25256:1554`. WhatsApp lists the port in the transfer
    list. Tests of that pattern pass in `bare`.
  - `Cannot read properties of undefined (reading 'operation')` at `Fz8l-_vrIk3.js:437:4283`. This is the search-index
    worker (`WAWebFtsWorkerDelegate`): a message with no `command` reaches `enqueue`.
- Groups and Status notifications are off in WhatsApp's settings. The owner said to turn them on. This is not done.
- Service workers: WhatsApp's `/sw.js` is not checked live. Scripts, images, XHR, workers and frames do not go to the
  service worker yet.
- The caption box of the photo editor has no label for the `label=` locator. Use `find role=textbox` and a ref.
- A paint bug: old search text showed over the "Messages" row in the notification settings.
- Not tested: a notification click, a message from a chat that is not open, voice notes recorded in the page.
