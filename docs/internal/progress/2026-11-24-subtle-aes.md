# SubtleCrypto: AES-GCM, AES-CBC and HKDF keys

Date: 2026-11-24. Base: main 56536335. Plan approved by the owner on 2026-11-24 (RSA comes later, with sign-in).

## Why

- Teams (signed out, `teams.steps`) logged `Unhandled error/rejection false`.
- Cause: the MSAL wrapper of Teams (`msal-service`, `handleCallback`) calls `reject(false)` when MSAL `initialize()` fails. MSAL failed with `NotSupportedError: This SubtleCrypto method is not supported yet`. Bare had `digest` only.
- MSAL encrypts its cache: AES-GCM `generateKey` and `exportKey`, HKDF `importKey` and `deriveKey`, AES-GCM `encrypt` and `decrypt`. Teams encrypts its tokens with AES-CBC 256.

## Change

- `bare_tls::aes` (new): AES with 128, 192 and 256-bit keys in both directions (table S-box, not constant-time), GCM with a 96-bit IV and a tag of 4 to 16 bytes, and CBC with PKCS #7 padding. TLS still uses `aes_gcm.rs`.
- `browser_subtle.rs` (new): `generateKey`, `importKey` and `exportKey` ("raw"), `encrypt`, `decrypt`, `deriveBits` and `deriveKey` for AES-GCM, AES-CBC and HKDF-SHA256. A `CryptoKey` has its data in `WebHost::keys` and is an `Owner::Web` object, so `browser_web::forget` drops its entry.
- `CryptoKey.prototype` has `type`, `extractable`, `algorithm` and `usages`. `algorithm` and `usages` give a new object on each read. There is no `CryptoKey` global (Teams does not use it), and no structured clone of a key.
- Still `NotSupportedError`: `sign`, `verify`, `wrapKey`, `unwrapKey`, RSA, EC, HMAC, the "jwk", "spki" and "pkcs8" formats, and AES-GCM with an IV that is not 12 bytes.

## Teams (signed out, 1 run)

- Before: console has `Unhandled error/rejection false`. A logger in the MSAL wrapper (tape copy) showed the `NotSupportedError`.
- After: no `false` rejection and no MSAL error. 0 panics. The page still stays on the spinner and makes the same 56 requests. Left: `getRegistration` of `undefined` (`navigator.serviceWorker`) and "user missing" (signed out).

## Tests

- `crates/tls/src/aes_tests.rs` (3 tests): FIPS 197 blocks for the three key sizes; CBC and GCM against Python `cryptography` 50 (OpenSSL); bad IV, bad padding, a changed AAD and a short tag.
- `browser_subtle_tests.rs` (2 tests): one script against Node 22.18 (`crypto.subtle`) for GCM, a 96-bit tag, a changed ciphertext, CBC, export, HKDF bits, an HKDF-derived key, the MSAL flow, `generateKey` and 14 error cases. Two differences from Node follow the spec: `usages` in `KeyUsage` order, and `InvalidAccessError` (Node: `InvalidAccessException`). The second test checks the methods and the formats that still reject.
- `browser_web_tests::subtle_has_every_method_and_an_illegal_constructor`: the built methods with no arguments now reject with `TypeError`.
- `t`: all passed after that update.

## Cost

- No row of `tools/js/cost_rows.txt` covers `browser_web.rs`. The `crypto` objects are made on the first use, so a page that does not read `crypto` or `navigator` pays nothing. The first use now also makes `CryptoKey.prototype` and 4 getters.

## Not tested

- The old engine.
- A collection that frees a key (the `forget` path is the one of Blob).
- Chrome itself: the expected values come from Node and from the spec.
