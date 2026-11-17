# Agent browser integration

The integration combines action outcomes and guidance with observations, typed recovery, session workers and task evaluation.

## Shared contracts

- A completed action with a failed postcondition keeps `action_status: completed`, `verification: failed` and typed recovery evidence.
- Failure evidence uses the current session redactor, including passwords learned by the action.
- Transport refusals report `not_started` and `input_dispatched: no`. Errors after dispatch remain `unknown`.
- Cancellation between step lines returns an error. It cannot turn skipped login or verification steps into success. Cancelled calls do not save session state or enter another snapshot wait.
- A parent waiting for a new tab observes cancellation. The tab remains a browser session; cancellation does not undo an opened tab. Session listing skips its initial load.
- Session diagnostics preserve their text. Only actual URLs pass through URL sanitization.
- Independent sessions can progress concurrently. One session retains FIFO order across close and reopen.

## Verification

The integration tests exercise completed actions with failed postconditions, secret redaction and pre-dispatch errors. Responsiveness tests cover cancellation during postconditions and tab creation, queue rejection and close/reopen ordering. Unit tests cover cancellation between step lines, diagnostics and post-dispatch error metadata.

The Python task corpus has eight seeded tasks. Scripted smoke results check protocol and fixture correctness. They do not establish real-agent success.

Initial combined checks passed 44 targeted Rust tests and 62 Python tests. All eight scripted tasks passed three runs. Review then reproduced four regressions before correction: skipped steps reported as success, a blocked tab-open parent, missing queue-rejection metadata and truncated diagnostic text. The corrected focused suites pass. The final command results are recorded in the integration PR.

## Limits

Cancellation remains cooperative. It does not interrupt arbitrary scripts or native blocking operations. Windowed cancellation, Windows, release builds and real-agent browsing remain unverified. Driver failures from `act` and `step` are typed. Other untyped tool errors retain the documented `internal` fallback.
