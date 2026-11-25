# WhatsApp Web bring-up: status and the open blocker

Goal: run WhatsApp Web (`https://web.whatsapp.com/`) in `bare`. Link `bare` as a new device by QR. Then read the chat list,
read a conversation and send a text message. This file is the running record. Update it as the work moves.

Last updated: 2026-11-25.

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
