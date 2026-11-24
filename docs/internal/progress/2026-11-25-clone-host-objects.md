# Structured clone of `Blob`, `File` and `CryptoKey`

Date: 2026-11-25. Base: main e621b666.

## Change

- `CloneHost` has two new methods with defaults: `write_host_object` and `read_host_object` (`crates/js-api/src/clone.rs`). This is design Q5 of `docs/internal/design-host-clone.md`.
- The record carries the data of such an object as `Moved::Data(Box<dyn Any>)`. Vex keeps it in the moved list after the transferred items (`crates/vex/src/api_clone.rs`).
- Vex asks the host only for an object that has a host owner. A plain object has none, so the cost is one hash lookup per object.
- bare writes a `Blob`, a `File` and a `CryptoKey` (`browser_web::clone_out`, `browser_web::clone_in`). Before, each was cloned as an empty plain object.
- `BlobData.bytes` is now `Rc<[u8]>`. A clone of a `Blob` shares the bytes and copies no data. A `Blob` never changes, so the share is safe.
- Design sources: V8 `ValueSerializer::Delegate::WriteHostObject`. Ladybird gives each serializable interface its own serialization steps (`Bindings::Serializable`).
- Why: Teams keeps its token encryption key (a `CryptoKey`) in IndexedDB. The IndexedDB shim stores a structured clone. The key came back as `{}`, and `crypto.subtle.encrypt` threw "The provided value is not of type 'CryptoKey'". Attachments pass `Blob` and `File` objects through `postMessage` and IndexedDB too.

## Cost

- `tools/js/cost_matrix.sh -J -g 'web_apis_load clone'`, base e621b666, on arjun-e16. Each count is the minimum of 3 runs of `instructions:u`.

| Row | glibc base | glibc new | change | jemalloc base | jemalloc new | change |
|---|---|---|---|---|---|---|
| `web_apis_load clone 1` | 140209614 | 140498739 | +0.206% | 141858464 | 142085552 | +0.160% |
| `web_apis_load clone 10` | 1358110349 | 1360869664 | +0.203% | 1363800065 | 1366389668 | +0.190% |

- Both rows are inside the +0.5% rule. Hypothesis: the growth is the owner lookup per plain object.
- A `Blob` clone does not copy the bytes. Before, the clone was wrong and had no bytes.

## Tests

- New: `browser_clone_host_tests.rs` (2 tests). Both fail on main and pass with the change.
- `t`: all pass. Also `bare-vex --lib clone`, and `bare` `browser_clone_tests`, `browser_broadcast_tests`, `browser_worker_run_tests`, `browser_iframe_tests`: all pass.

## Not tested

- Miri: not run. The change adds no unsafe code.
- test262 counts: not run.
- A live Teams run: not yet.
- The `CryptoKey` global does not exist in bare. This change does not add it.
