# 2026-11-25: the disk form of a clone record (persistent profile step 1)

Branch `persistent-profile-design`. Design: `docs/internal/design-persistent-profile.md`, approved by the owner on 2026-11-25
(option A; file modes only; `open` with no profile uses the profile `default`).

## Change

- New `crates/bare/src/profile_record.rs`: `encode` and `decode` of a clone record with its `Blob`, `File` and `CryptoKey`
  objects. The format is in the module header. A `WebAssembly.Module`, a transferred buffer or a port has no disk form.
- `browser_subtle.rs`: `Key::parts` and `Key::from_parts`, with a fixed disk order of the algorithms.
- No caller yet: the IndexedDB log (step 3) is the first.

## Tests

- New `crates/bare/src/profile_record_tests.rs`: a round trip of a `Blob`, a `File` and a `CryptoKey`; every cut of an
  encoded record fails to decode; an unknown algorithm index fails; a transferred buffer is refused and leaves the output as it was.
- `t` on the changed files: all pass.

## Cost

None: no code path calls the new functions yet.

## Not tested

- A record made by a real page (step 3 tests that end to end).
- Windows.
