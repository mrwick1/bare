# IndexedDB transaction survives a microtask chain between requests

## Bug

The `indexedDB` shim (`crates/bare/src/shims/idb.js`) finished a transaction the moment its request
queue first drained. Each request's `success` schedules the next `step` as a microtask; when the
queue was momentarily empty, `step` set `t.dead = true` and dispatched `complete` at once.

The `idb` promise wrapper and `async`/`await` enqueue the next request several microtask hops after
the previous request's `success`, inside the same task. So the transaction died before the next
request was pushed, and that request threw `TransactionInactiveError: The transaction has finished.`

Authenticated Microsoft Teams hit this: the live run logged 20 `TransactionInactiveError`, the
dominant blocker to rendering the app shell.

## Fix

Commit the transaction at the next task boundary, not on the first empty microtask. When the queue
drains, `step` now releases the loop (`t.on = false`) and schedules one `setTimeout(_, 0)` grace
callback (guarded by `t.graceOn`). The callback commits only if the transaction is still alive, the
loop is not running and the queue is still empty; otherwise new work resumes the loop. This matches
the spec rule that a transaction commits when control returns to the event loop with no request
pending, and lets any number of microtask/`await` hops in one task enqueue more requests first.

## Tests

- New file `crates/bare/src/browser_idb_tx_tests.rs`, test
  `transaction_survives_a_microtask_chain_between_requests`: a write enqueued three `Promise.then`
  hops after a read's `success` lands on the same transaction. Fails before the fix
  (`complete,n1,throw:TransactionInactiveError`), passes after (`put2ok,complete,n2`).
- All 9 existing `indexeddb*` tests in `browser_gaps_tests.rs` still pass.

## Cost

JS-shim-only change, no Rust subsystem touched. The transaction now completes one macrotask later
(a `setTimeout(_, 0)`), not one microtask; no extra per-request work.

## Not tested

Not re-run against live Teams yet (next step). Other Teams blockers remain: telemetry worker
`RangeError`, missing `caches`/CacheStorage, `WorkerGlobalScope` undefined in workers.
