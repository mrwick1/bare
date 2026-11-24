# In-memory `caches` (CacheStorage) shim

Date: 2026-11-24. Base: main a40994f6.

## Change

- New `crates/bare/src/shims/caches.js`. It defines `caches`, `CacheStorage` and `Cache`. It loads at the first read of one of these names (`browser_shims.rs`).
- Storage is in memory. Each `Cache` holds a `Map` keyed by the request URL. `caches` holds a `Map` of named caches.
- `CacheStorage`: `open`, `has`, `delete`, `keys`, `match`.
- `Cache`: `match`, `matchAll`, `add`, `addAll`, `put`, `delete`, `keys`.
- `put` stores `response.clone()`. It rejects with `TypeError` for status 206, for a URL scheme that is not http or https, and for a value that is not a `Response`. `match` returns a clone or `undefined`. `add` runs `fetch` then `put`. `keys` returns `Request` objects.
- Why: Teams calls `caches.open` at boot and logs the failure.

## Left out on purpose

- The options `ignoreSearch`, `ignoreVary` and `cacheName` are accepted. They have no effect. Matching is by exact URL.
- No persistence. A new page starts with no caches, like the `indexedDB` shim.
- Request method and `Vary` headers are not checked.

## Tests

- New: `browser_caches_tests.rs` (2 tests): global types, put and match round trip, a miss, a rejected scheme, `has`, `keys`, `delete`.
- `cargo test --locked -p bare --lib browser_caches_tests::`: 2 passed.
- `t`: all pass, exit 0.

## Not tested

- Teams was not run against this change.
- The cost gate rows were not run. The shim loads only on first use.
- test262 counts: not run.
