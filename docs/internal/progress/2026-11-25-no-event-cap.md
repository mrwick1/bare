# No count cap on event objects (2026-11-25)

## Change

- `BrowserLimits::max_events` is gone. A page may make and keep any number of event objects, as in Chrome. The heap limits
  bound their memory.
- A collect is still due when the live events grow: when their count is more than twice the count after the last collect,
  and at least 8,192 (`Browser::collect_due`). The trigger grows with the live count, so many live events do not make a collect
  each round.

## Why

WhatsApp link attempt 10 stopped with `Limit(Host("dom-events"))` during the message download. A diagnostic build printed
the live events at the cap: 16,383 IndexedDB `success` events, all made in one task. The owner asked to remove caps that make
pages fail (2026-11-25).

## Tests

- New: `browser_events_count_tests.rs`. One task keeps 40,000 events; a collect keeps them while they are live and frees them
  after.
- The cap cases in `browser_focus_tests.rs`, `browser_form_tests.rs` and `browser_tests.rs` are removed.
- `t` on the touched test files: all pass.

## Cost

Not measured. The change removes one check per event and adds one borrow per `collect_due` call. Hypothesis: no row moves.
