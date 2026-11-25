# Fetch: the `integrity` option (Subresource Integrity)

## Problem

The Teams WebRTC worker calls `fetch` with `integrity`. bare stopped the worker with `Unsupported("fetch option integrity")`.

## Change

- `fetch` and `new Request` read `integrity`. `request.integrity` gives it back. `new Request(request)` and `clone()` keep it.
- After the last hop, `sri_match` checks the body against the metadata:
  - known algorithms: `sha256`, `sha384`, `sha512` (from `bare_tls`);
  - only the strongest algorithm in the list counts, and one of its digests must match;
  - a list with no known algorithm passes;
  - base64 and base64url digests match, with or without padding;
  - options after `?` are ignored.
- On a mismatch, or on an opaque (`no-cors`) reply, the fetch is a network error (`TypeError`). The console diagnostics get a line.
- New `browser_encoding::base64` encodes bytes for the compare.

## Tests

- New: `crates/bare/src/browser_fetch_integrity_tests.rs` (2 tests: the metadata rules, and `fetch` with a match, a mismatch and `request.integrity`). The second test failed before the change with the gate.
- `browser_fetch_tests.rs`: the `integrity` gate line is removed. `browser_fetch_tests::` passes (55).
- `t`: pass.

## Live result (archMachine, 2026-11-25, 1 run, saved session)

The WebRTC worker no longer stops. Its scripts load (200), and no integrity mismatch shows. The telemetry worker still stops on a synchronous XHR.

## Cost

No cost rows ran. A fetch without `integrity` pays one `Option` check. A fetch with it hashes the body once.

## Not tested

- `<script integrity>` and `<link integrity>`: the loader does not check them (as before).
- A streamed fetch body: bare reads the whole body before it resolves, so the check sees all bytes.
