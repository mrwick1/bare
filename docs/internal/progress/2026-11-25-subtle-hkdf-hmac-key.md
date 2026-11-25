# SubtleCrypto deriveKey from HKDF to HMAC (2026-11-25)

## Finding

WhatsApp Web workers log `NotSupportedError: Unrecognized algorithm name.` at start. The search worker bundle
(`WAWebFtsWorkerResource`, module `WAWebDbEncryptionKey`) makes its search index HMAC key with this call:

```js
crypto.subtle.deriveKey(
  { name: "HKDF", hash: "SHA-256", salt, info },
  key,
  { name: "HMAC", hash: "SHA-256" },
  false,
  ["sign"],
);
```

`deriveKey` accepted only AES-GCM and AES-CBC as the derived key type. HMAC gave the error.

The SubtleCrypto calls in the six worker bundles of the logged-out page:

| Call                 | Algorithm                | Before  |
| -------------------- | ------------------------ | ------- |
| `digest`             | SHA-1, SHA-256, SHA-512  | works   |
| `importKey` raw      | HMAC SHA-1/256/512, HKDF | works   |
| `sign`               | HMAC SHA-1/256/512       | works   |
| `deriveBits`         | HKDF SHA-256, 256 bits   | works   |
| `deriveKey`          | HKDF to AES-CBC 128      | works   |
| `deriveKey`          | HKDF to HMAC SHA-256     | missing |
| `encrypt`, `decrypt` | AES-CBC                  | works   |

The v4 init script URL gives 404 to a plain fetch. Its bundle is not checked. Hypothesis: it loads the same search worker
bundle, because the error column (4049) is the same.

## Change

`deriveKey` accepts an HMAC derived key type. Without `length`, the key is one block of its hash long, as the spec says
("get key length" of HMAC). The usages must be `sign` or `verify`.

## Tests

- New: `browser_subtle_hkdf_hmac_tests.rs`. The derived bytes are the RFC 5869 test case 1 OKM. It fails before the fix
  with the WhatsApp error and passes after it.
- `t` on the changed files.

## Cost

No row in `tools/js/cost_rows.txt` covers SubtleCrypto. No cost run.

## test262

No change to Vex. No run.

## Not tested

- WhatsApp Web with a login. The worker error is not checked live.
- An HMAC `length` that is not a whole number of bytes. It rejects with `NotSupportedError`, as for `importKey`.
