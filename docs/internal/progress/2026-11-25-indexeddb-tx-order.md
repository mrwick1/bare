# IndexedDB transaction order (2026-11-25)

## Change

The shim (`shims/idb.js`) orders transactions as the standard says. A transaction starts when no earlier, unfinished
transaction of its database overlaps its scope with a write on either side. Two readonly transactions run together. A
`versionchange` transaction overlaps all stores. A finished or aborted transaction starts the waiting ones that nothing blocks
now, in the order they were made.

## Why

WhatsApp link attempt 12: `message.bulkAdd` failed 17 times with `ConstraintError`, and Dexie transactions then failed with
`TransactionInactiveError`. WhatsApp reads a row and adds it when it is missing. The shim ran two such transactions at the same
time, so both added the same key. Chrome runs the second one after the first one commits.

## Tests

- New: `browser_idb_tx_order_tests.rs`. Two read-then-add transactions on one store: the second sees the first one's row. A
  readonly transaction on another store does not wait. It failed before the change with the same `ConstraintError`.
- All `browser_idb_*` and `profile_*` test files pass.

## Not built

The order is per realm. A worker and a page of one origin do not wait for each other yet (persistent profile step 4).

## Cost

Not measured: `idb_load` runs one transaction at a time, so no transaction waits. The new work per transaction is one scan of
the unfinished transactions at start and at end. Hypothesis: no change on the `idb_load` rows.
