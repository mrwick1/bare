# 2026-11-25: worker global scope

Branch `whatsapp-worker-scope`. Record: `docs/internal/design-whatsapp-bringup.md`, "Link attempt 1".

## Change

A worker global gets `WorkerGlobalScope` and `DedicatedWorkerGlobalScope` and their prototypes in its chain. WhatsApp's
worker asserts `self instanceof DedicatedWorkerGlobalScope` (invariant 56318) and stopped the message download.

## Tests

- New: `crates/bare/src/browser_worker_scope_tests.rs` (2 tests). Both failed before the change.
- `cargo test -p bare --lib browser_worker`: 32 passed.

## Cost

The change runs only in a worker page, at its start: two functions, two objects and one prototype change. No cost row
starts a worker. Rows not run.

## Not tested

- The live WhatsApp journey after the fix (needs a new scan by the owner).
- `ErrorEvent` for an uncaught worker error. Bare still fires a plain `Event`.
- Windows.
