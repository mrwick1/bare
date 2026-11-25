# Events: no cap on the listeners of one target

## Problem

The Teams data worker stopped with `Limit("dom-listeners-per-target")`. Its global had 1021 listeners: 510 `message`, 509 `beforeinstallprompt` and 2 others. The cap was 1024. The chat list stayed empty.

A browser has no cap for each target.

## Change

`BrowserLimits::max_listeners_per_target` is removed with its 6 checks (`addEventListener`, the event handler setters in `browser_handlers.rs`, `browser_loader.rs`, `browser_platform.rs` and `browser_scheduler.rs`). The total cap `max_listeners` (65536) stays.

The per-target cap kept the duplicate check of `addEventListener` short: it reads the list of the target (O(n) for each add). Worst case now: 65536 listeners on one target. Owner decision (2026-11-25): accept this, add no hash index.

## Tests

- New: `crates/bare/src/browser_listener_per_target_tests.rs`. It adds 2000 listeners to `window` and dispatches one event. It failed before the change with the limit.
- `caps_return_distinct_limit_errors` and `the_listener_caps_are_checked_before_a_handler_slot_is_added` no longer test the removed cap.
- `t`: pass.

## Live result (archMachine, 2026-11-25, 1 run, saved session, with #1093)

| | Before | After |
|---|---|---|
| Data worker | stopped at the cap | runs to the end |
| `definitionsv2` | 0 | 7 × 200 |
| Chat message requests | 8 | 64 |
| Screen | header, presence | chat list, messages, unread count |

## Cost

No cost rows ran. The change removes one compare from each listener add.

## Not tested

- A page with tens of thousands of listeners on one target (the time of the duplicate check).
- Still open in the same run: the telemetry worker stops on a synchronous XHR, and the WebRTC worker stops on the `fetch` option `integrity`.
