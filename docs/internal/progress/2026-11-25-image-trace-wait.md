# Net trace: the queue wait of a picture

## Problem

The trace showed a queue wait of 15 to 90 s for Teams avatars. The number was wrong. A picture request set no `wanted` time, so `trace::begin` counted its wait from the page start. All 74 avatars of one run had the same base (21 983 ms).

## Change

`images.rs` keeps the time when a picture enters `queue` or `css_queue`. The job calls `bare_net::trace::wanted` with that time before `load`. The trace wait of a picture is now its wait for a download slot (`INFLIGHT`, 4).

## Tests

- No new unit test. The trace is a process-global list, and a test would need the whole picture loop of a page. The live run below checks the change.
- `cargo check -p bare`: pass. `t`: pass.

## Live result (archMachine, 2026-11-25, 1 run, saved session)

| Teams `profilepicturev2` (68) | Before (ws22) | After (ws23) |
|---|---|---|
| Wait, median | 17 587 ms (from page start) | 1 148 ms |
| Wait, p90 | not measured | 3 009 ms |
| Wait, max | 91 846 ms | 3 890 ms |
| Time to first byte, median | 82 ms | 82 ms |

The real wait is 1 to 4 s for a reply of about 0.1 s. The cause is hypothesis: 4 slots, and a done job is seen only at the next `images_step`.

## Cost

No cost rows ran. The change adds one `Instant` per queued picture. `trace::wanted` returns at once when tracing is off.

## Not done

- The wait starts when the picture enters the queue, not when the page set `src`.
- `INFLIGHT` stays 4.
