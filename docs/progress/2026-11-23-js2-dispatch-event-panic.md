# 2026-11-23 — js2: the react-docs replay no longer panics ("event is registered")

## Cause

- `Dom::dispatch` (`crates/bare/src/browser_events.rs`) held the event only in a Rust local. Nothing rooted it for js2's precise GC.
- The host creates some events itself (for example the `message` event of `Run::Deliver`). It passes them to the listeners as arguments, but only while a listener runs.
- Between two listeners the host runs a microtask checkpoint. A microtask that allocates can start a GC (`gate!` in `crates/js2/src/interp.rs`). The event was then dead.
- `Dom::prune` (`browser.rs`, `Owner::Event`) removed it from `st.events`, with its `dispatching` flag still true. Verified with a temporary print: one removal had `dispatching=true`, 20 had `false`.
- Then `dispatch` hit `expect("event is registered")` (`browser_events.rs:104`). The event type at the panic was `message`, on a non-window target.
- Hypothesis A of the brief was right. The GC core of js2 is not at fault.
- OLD does not panic. Not verified why (hypothesis: a different GC trigger).

## Fix

- `Hd::running` (`browser_handlers.rs`): a stack of the events that `dispatch` runs. `dispatch` pushes and pops. `browser_handlers::roots` makes them roots. A nested dispatch keeps its outer events.

## Test

- `browser_events_gc_tests.rs`: a `MessageChannel` port with two `message` listeners. The first queues a microtask that allocates. Before the fix it panicked at `browser_events.rs:206` (`no entry found for key`, `st.events[&ev]` in the same dispatch). After the fix it passes. The panic site differs from the replay (104 against 206) because the freed event is hit by the next listener before the end of the dispatch. The cause is the same.
- `t`: all pass (1765 lib tests). `cargo check --locked -p bare --all-targets`: pass.

## Replay of react-docs

Machine archMachine, shared. Release `bare-drive`, no symbols. Env `BARE_NET_REPLAY=<tape> BARE_CLOCK=virtual BARE_SEED=1 LC_ALL=C GLIBC_TUNABLES=glibc.malloc.tcache_count=65535`, `perf stat -x, -e instructions:u`, 3 runs each. Reply count is `len(network)` in the report.

| Tape | Engine | Exit | Replies | Instructions (M), 3 runs |
|---|---|---|---|---|
| react-docs-new | js2 | 0 | 12 | 3776.6, 3775.9, 3776.8 |
| react-docs-new | OLD | 0 | 12 | 5051.4, 5030.8, 5030.3 |
| react-docs-old | js2 | 0 | 12 | 3776.8, 3777.3, 3777.9 |
| react-docs-old | OLD | 0 | 12 | 5030.5, 5052.7, 5030.5 |

All 12 runs have `ok: true`. Before the fix, js2 exited 1 on both tapes (the old tape was measured by the earlier entry; this session reproduced the panic on the new tape only).

## Not tested

- The panic on the old tape before the fix (taken from the earlier entry).
- Other event paths that create an event in Rust and dispatch it (the fix covers all of them, but only the `message` path has a test).
- Wall time, test262, the cost matrix.
- Seen and not changed: 13 requests miss the tape on replay (`bare-net: replay: no recorded reply`), as in the earlier entry.
