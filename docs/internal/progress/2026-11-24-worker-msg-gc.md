# Queued worker messages and the collector

Date: 2026-11-24. Base: main 64d36cf7. Follows `2026-11-24-worker-import.md`.

## Bug

- A message from a worker waits in the inbox of its `Worker` object until its delivery task runs.
- The trace of an event target traced the inbox of `Owner::Port` only, not of `Owner::Worker`. A collection before the delivery freed the data of the message.
- The `data` getter then read a dead cell. Teams (signed out, `teams.steps`) panicked in every run: `cell.rs:59` (bad header) through `msg_data`, or `bi_json.rs:339` ("flat") in `JSON.parse` of the data.

## Change

- `browser.rs`: the trace of `Owner::Worker` also traces its port entry (`sched_trace_port`): the queued data, ports and senders.

## Tests

- New: `browser_worker_gc_tests.rs` (1 test). A worker posts 20 messages at once. The first handler makes much garbage, so a collection runs while 19 messages wait. Before the fix it panicked at `cell.rs:59`. After the fix all 20 messages have their data.
- `t`: all passed.

## Teams

- Before: 8 of 8 runs panicked.
- After: 2 of 2 runs end with no panic. The page still stays on the spinner.

## Not tested

- The old engine.
- Messages from the page to the worker (they wait in the worker global port, which `trace_roots` already traces).
