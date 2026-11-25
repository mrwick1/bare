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

Live effect, same journey, before vs after PR 1 (archMachine, 1 run each):

| fact                       | before   | after                                 |
| -------------------------- | -------- | ------------------------------------- |
| requests                   | 11       | 22                                    |
| page idle after            | 1.1 s    | not idle at 90 s                      |
| first page error           | none     | `WAWeb-curve25519-donna` (see below)  |

## Open blocker: `Function.prototype.toString` gives no source text

`WAWeb-curve25519-donna` is old Emscripten output. Its `cwrap` helper reads the source of small functions:
`t.toString().match(d).slice(1)`. Vex gives `function NAME() { [native code] }` for a script function, so `match` is
`null` and the module throws `Cannot read properties of null (reading 'slice')`. The Teams record names the same gap
(`design-teams-bringup.md`, "Remaining gap").
