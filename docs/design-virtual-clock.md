# Design: a virtual page clock for replay mode

Status: built on 2026-11-14 (branch `t3/deterministic-replay-clock`). Written against main `fe2d54cd`. Machine: archMachine.
Code: `crates/net/src/vclock.rs` and the call sites listed in section 4.

## 1. Problem

The journey benchmark (`docs/progress.md`, PR #486) serves recorded replies from a tape. A page still races the real clock. A reply is ready when a worker thread has read the tape and decoded the body. A step of the driver ends when its timers and sleeps say so. Which of the two comes first changes with the load of the machine.

Measured on the real clock, same binary, same corpus, `bare-drive` after #486:

- `rust-book-search`: the search index loads while the driver types 9 keys. If it arrives after key 3, the page searches 6 times. If it arrives early, it searches once. Runs gave 3.8 G to 19.7 G instructions.
- `python-docs-search`: 175 M in 5 of 6 runs and 1,432 M in the sixth. The debounced search ran or did not run before the journey ended.
- Same-binary spread over 3 runs (new binary, real clock): 15 of 47 journeys over 2%, 7 over 5%. The first measurement (#486, two binaries) had 28 over 2%.

Cause, by the discrete modes of the runs: the order of worker threads, page timers and the end of a step depends on real time. This is a race, not noise.

## 2. Options

| Option | How | Result |
| --- | --- | --- |
| A. Longer waits | Wait for every reply before the next step | Removes some races. The page timers still use real time. Slower. Not a design. |
| B. Chrome style virtual time budget (`Emulation.setVirtualTimePolicy`) | Time only moves when the page is idle. Chrome waits for pending fetches in real time. | Pending fetches still end at a real time, so the order of "reply" and "key" stays random. Does not fix `rust-book-search`. |
| C. Virtual clock with a time for each reply (chosen) | Every request has a due time on the clock. The page sees the reply only when the clock has reached it. | Order of replies, timers and driver steps is a function of the inputs only. |

Option C is B plus one change: the reply has a time of its own (`REPLY_MS`), and the real time of the worker does not matter.

## 3. Design

`BARE_CLOCK=virtual` and a tape that reads (`BARE_NET_REPLAY` or `BARE_NET_FILL`) turn the clock on for the process. Nothing else does. With no tape the variable does nothing.

- Time is a number of milliseconds that only `vclock` moves. `performance.now`, `Date.now`, timers, animation frames, idle callbacks and the slice deadlines of the loader read it. Nothing sleeps for it.
- A request that a worker runs (`parallel::Task`) gets a due time when it starts: now + `REPLY_MS` (5 ms). The page sees the result (`Task::done`) only when `now >= due`.
- The clock moves in three cases.
  1. The page or the driver waits: it jumps to the next event (a timer of the page, or the due time of a task). `Page::nap` is the one place for it. It always moves by at least 0.001 ms, so a loop with a limit reaches the limit.
  2. A unit of work runs: `TASK_MS` (0.1) for a task of the scheduler, `ACTION_MS` (1.0) for one try of a driver step.
  3. Page code reads the time: `READ_MS` (0.001), so that `while (Date.now() < t)` ends.
- A move to time `t` first waits, in real time, for every task due at `t` or earlier. This is the only place where real and virtual time meet. A task that is due always has its result.
- `Date.now()` is `BARE_CLOCK_EPOCH` (default 2026-11-13T00:00:00Z) plus the virtual time. Each document has a time origin, `vorigin`, for its `performance.now`.

Not virtual: the limits of a step, a script and a load (they stop a hang and do not make a result), and the clocks of TLS and cookies. One process runs one journey, so the clock is global.

Complexity: a `BTreeMap` of pending tasks keyed by (due time, start order). A start and a move cost O(log n) for n requests in flight. The lock is held only for the map, never while waiting.

## 4. What changed

- `crates/net/src/vclock.rs` (new): the clock. `crates/net/src/parallel.rs`: `Task` has a due time.
- `crates/js/src/eval_date.rs`: `WallClock::Host`, so `Date.now()` and `new Date()` read the host clock.
- `crates/bare/src/browser_scheduler.rs`: `now_ms`, `performance.now`, task cost, `move_clock`. `browser_session.rs`, `browser_loader.rs`, `browser_loader/scan.rs`: slice deadlines use a frozen virtual instant, so a parse slice is the same in every run.
- `crates/bare/src/page.rs`: `nap`, virtual instants in `tick`, `work`, `wait_load`, `hide`. `crates/bare/src/images.rs`: the picture waits.
- `crates/drive/src/run.rs`: every sleep of a step is `Page::nap`. A try costs `ACTION_MS`.
- `tools/bench/journeys.py`: `--clock real|virtual`. The recording for the virtual clock is a separate corpus, `corpus/virtual`.

## 5. Effects to know

- The clock decides when a reply arrives, so it decides which of the real modes a journey shows. `rust-book-search` always takes the 6-search mode (16.5 G). `python-docs-search` always runs its search (2.1 G). The real clock gave 3.8 G and 175 M most of the time. Total instructions of the corpus on the same binary: 66.6 G real, 96.3 G virtual. Compare a binary with another only on the same clock and the same corpus.
- `REPLY_MS` and `ACTION_MS` set the ratio of the reply time to the key time. A page that is slow to load on the real net is not modeled. The constants are in the source, not in the environment, so the numbers stay comparable between runs.
- A recording made on the real clock does not hold every reply that the virtual clock asks for. A page that never got to a chunk on the real clock asks for it now (`react-docs`: 14 replies). `journeys.sh fill --clock virtual` completes the corpus. A journey that fails on a miss is a finding, not a pass.
- Hypothesis: a rare beacon whose body holds a value that is not on the clock (`wikipedia-portal`: one POST to `/ins-*/v2/events`) still misses. The label hash changes from run to run. Its cause is not found.

## 6. Tests

- `vclock::tests`: moves by events, waits for due tasks, never goes back.
- `crates/drive/tests/replay.rs`: a page with an 8 s timer ends in under 4 s of real time on replay with the clock on, and the same step needs the 8 s when the tape does not answer (clock off).
