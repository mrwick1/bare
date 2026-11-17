# Agent browser integration

The integration combines action outcomes and guidance with observations, typed recovery, session workers and task evaluation.

## Shared contracts

- A completed action with a failed postcondition keeps `action_status: completed`, `verification: failed` and typed recovery evidence.
- Failure evidence uses the current session redactor, including passwords learned by the action.
- Transport refusals report `not_started` and `input_dispatched: no`. Errors after dispatch remain `unknown`.
- Cancellation between step lines returns an error. It cannot turn skipped login or verification steps into success. State saving and snapshot entry points check cancellation before continuing.
- A parent waiting for a new tab observes cancellation. The tab remains a browser session; cancellation does not undo an opened tab. Session listing skips its initial load.
- Session diagnostics preserve their text. Only actual URLs pass through URL sanitization.
- Independent sessions can progress concurrently. One session retains FIFO order across close and reopen.

## Verification

The integration tests exercise completed actions with failed postconditions, secret redaction and pre-dispatch errors. Responsiveness tests cover cancellation during postconditions and tab creation, queue rejection and close/reopen ordering. Unit tests cover cancellation between step lines, diagnostics and post-dispatch error metadata.

The Python task corpus has eight seeded tasks. Scripted smoke results check protocol and fixture correctness. They do not establish real-agent success.

Initial combined checks passed 44 targeted Rust tests and 62 Python tests. All eight scripted tasks passed three runs. Review then reproduced four regressions before correction: skipped steps reported as success, a blocked tab-open parent, missing queue-rejection metadata and truncated diagnostic text. The corrected focused suites pass. The final command results are recorded in the integration PR.

## Limits

Cancellation remains cooperative. It does not interrupt arbitrary scripts or native blocking operations. Windowed cancellation, Windows, release builds and real-agent browsing remain unverified. Driver failures from `act` and `step` are typed. Other untyped tool errors retain the documented `internal` fallback.

## Final validation (2026-11-17)

The combined source at `83c0da645d75b4f10b9af32cd1b10a64a1bad5e9` passed all MCP unit and integration suites in the full workspace run. This includes five MCP unit tests, 46 focused agent tests and 101 server tests; one window test was ignored. All 62 Python evaluation tests passed. A frozen debug browser from this source completed all eight scripted tasks in each of three runs with no duplicate actions. `cargo check --workspace` passed.

The full workspace command completed with 4,202 passed, eight failed and 16 ignored tests. The failures were in `bare-drive/tests/survive_limit.rs` (one), `bare-net/tests/https_openssl.rs` (two), `bare-tls/tests/client_openssl.rs` (four) and `bare-tls/tests/tls12_openssl.rs` (one). All eight failures reproduced in an independent source export of the unchanged base `f4e1fcd8`. The runtime-budget failure had the same driver kind and message. The TLS tests rejected the peer but failed their expected error classification. No expectations or engine budgets were weakened.

An earlier full MCP run had a cookie-banner timing failure. The test passed all three isolated reruns and the final workspace run. Its cause was not established. No performance improvement is claimed.
