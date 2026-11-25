# HMAC with SHA-1, SHA-384 and SHA-512 (2026-11-25)

## Change

`SubtleCrypto` HMAC takes SHA-1, SHA-256, SHA-384 and SHA-512 (`browser_subtle.rs`). The key keeps its hash
(`Alg::Hmac(Hash)`). `algorithm.hash` names it. A generated key with no `length` is one block long (64 or 128 bytes). SHA-256
still uses `bare_tls::hmac`. The other hashes use RFC 2104 over the `bare_tls` hash functions.

The disk order of the algorithms (`DISK`) appends the three new HMAC kinds. The old indexes do not change.

## Why

WhatsApp link attempt 11: the app state sync (syncd) failed 40 times with "HMAC takes SHA-256 only". Then WhatsApp logged the
device out.

## Tests

- New: `browser_subtle_hmac_hash_tests.rs`. RFC 2202 and RFC 4231 test case 2 for each hash, a key longer than the block, a
  generated SHA-512 key.
- `browser_subtle_ctr_hmac_tests.rs`, `browser_subtle_tests.rs`, `profile_record_tests.rs`: pass.

## Cost

Not measured. No cost row runs `SubtleCrypto`. The SHA-256 path is the same code as before.
