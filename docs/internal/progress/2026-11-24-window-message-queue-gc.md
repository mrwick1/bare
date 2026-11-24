# Window message queue: data survives a collection

## Bug

A `message` queued at the window (`window.postMessage`, `queue_window` in `browser_scheduler.rs`) lost its data
through a garbage collection while it waited in the queue. The data came back empty, or, when the freed memory was
reused, bare read it as a live value and crashed. Microsoft Teams, signed in, hit the crash: `SIGSEGV` in
`Realm::a_push` at `crates/vex/src/api.rs:251` (`s.is_rope()`) reading a freed rope header. The faulting value was
`MessageEvent.data`, read by the `message` handler through `msg_data` during delivery.

## Cause

The window is a port, but its wrapper has no `Owner::Port`. So `Dom::trace` never dispatches to `sched_trace_port`
for it, and the window's queue (`Port.inbox`) was never traced. `sched_roots` marked the window object as reachable
(it is the global anyway) but did not trace the queued messages' `data`, `ports` or senders. A `BroadcastChannel`
and a `MessagePort` do have `Owner::Port`, so their queues were traced; only the window's was not.

String data of a same-page `postMessage` is the sender's own value, kept as is by `clone_payload` (no flatten). A
value built by concatenation is a rope, so the lost value was a rope. A cross-page message is flattened by the
receiver (`make_units`), which is why the crash needed a same-page post.

## Fix

`sched_roots` now traces the window port's queue directly: for each queued message it roots `data`, the transferred
`ports` and the sender's `source`. This matches what `sched_trace_port` does for a wrapper-backed port.

## Tests

New file `crates/bare/src/browser_message_queue_gc_tests.rs`:
- `queued_window_messages_keep_their_rope_data_through_a_collection`: 20 `window.postMessage` calls with rope data;
  the first handler churns garbage to force a collection while 19 wait. Fails before the fix (data comes back empty),
  passes after.
- `queued_same_page_broadcast_messages_keep_their_rope_data_through_a_collection`: the same for a same-page
  `BroadcastChannel`. Passes before and after (that path already had `Owner::Port`), kept as a guard.

Ran `t` (covering tests of the changed files). Not run: the full suite, and the live Teams sign-in (verified
separately).

## Cost

Not a Vex change; `sched_roots` runs per collection. Added work is one pass over the window's queue (usually empty).
Cost rows of the scheduler: to run before merge.

## test262

Unchanged (no engine change).
