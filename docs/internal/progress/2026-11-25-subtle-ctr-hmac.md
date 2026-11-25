# AES-CTR and HMAC in SubtleCrypto (WhatsApp bring-up step 3)

Record: `docs/internal/design-whatsapp-bringup.md`.

## What changed

- `crypto.subtle` takes `AES-CTR` in `generateKey`, `importKey`, `exportKey`, `encrypt` and `decrypt`
  (`browser_subtle.rs`). The counter is 16 bytes, `length` is 1 to 128. Data that needs more than `2^length` blocks rejects
  with `OperationError`, as the standard says. The block cipher is the AES of `bare-tls`.
- `crypto.subtle` takes `HMAC` with SHA-256 in `generateKey`, `importKey`, `exportKey`, `sign` and `verify`. `verify`
  compares in constant time. Another hash rejects with `NotSupportedError`.
- `sign` and `verify` are no longer stubs (`browser_web.rs`). Only `wrapKey` and `unwrapKey` stay unsupported.

Before, WhatsApp's AES-CTR key and HMAC calls rejected with `NotSupportedError`.

## Tests

- `crates/bare/src/browser_subtle_ctr_hmac_tests.rs`. The expected bytes come from Node 22 `webcrypto` with the same keys
  and inputs.
- `browser_subtle_tests.rs`: the not-supported test uses `ECDSA` now. `browser_web_tests.rs`: `sign` and `verify` with no
  arguments throw `TypeError`.
- `t`: all covering tests pass.

## Cost rows

`cost_matrix.sh -g 'web_apis_load|browser_load'`, base 16c1e3c3, archMachine, glibc. 20 rows measured, none over +0.5%,
mean +0.008%. `web_apis_load segmenter 1` and `segmenter 10` fail to run on the base and on this branch the same way
(a script error in the row itself): not caused by this change.

## Not tested

- HMAC with SHA-1, SHA-384 or SHA-512 (rejected).
- The effect on WhatsApp alone. With PR 2 (`Function.prototype.toString` source text), the page gets to the QR canvas
  (48 requests).
- Windows.
