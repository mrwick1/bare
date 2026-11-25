# 2026-11-25: IndexedDB requests yield to a task

Branch `idb-request-tasks`.

## Change

- `shims/idb.js`: a transaction runs up to 64 requests in one microtask chain, then the next request waits for a task
  (a `MessageChannel` message; a `setTimeout` where there is no `MessageChannel`). The standard runs each request in its own task.
- Cause: WhatsApp link attempt 7. The message download queued thousands of requests. They all ran in one microtask chain, so
  the host could not collect, and the page stopped at the live event cap (`Limit(Host("dom-events"))`).

## Tests

- New `crates/bare/src/browser_idb_many_requests_tests.rs`: 5,000 puts in one transaction with an event cap of 2,048, pumped as the
  loader pumps (8 tasks, then a collect when due). Before the fix: `Limit(Host("dom-events"))`. After: all 5,000 succeed.
- `t` on `browser_idb.rs` and the IndexedDB tests of `browser_gaps_tests.rs`: all pass.

## Cost

archMachine. Base = main with #1092. Minimum of 3 runs (`instr.sh`).

| row | base | new | change |
| --- | ---: | ---: | ---: |
| idb_load 1 | 327,014,358 | 329,058,178 | +0.63% |
| idb_load 10 | 3,122,778,035 | 3,143,966,922 | +0.68% |

Over the +0.5% rule: one task and one `MessageEvent` per 64 requests. Rows for the debt list: `idb_load 1` +0.63%,
`idb_load 10` +0.68%. The row is still 33% below the shim before #1091.

## Not tested

- The WhatsApp journey with this fix.
- A request run that is 64 long in one task still has its events until the next collect. A page that makes many other events
  in the same task can still reach the cap.
- Windows.
