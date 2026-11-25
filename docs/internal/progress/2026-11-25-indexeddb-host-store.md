# 2026-11-25: one IndexedDB store per origin (design steps 1 and 2)

Branch `idb-shared-store-design`. Design: `docs/internal/design-indexeddb-store.md`, option A, approved by the owner.

## Change

- New `crates/bare/src/browser_idb.rs`: `IdbStore`, the data of the databases of one origin. An object store is a `BTreeMap`
  from key bytes to a row (a `Cloned` record and its index entries). An index is a `BTreeSet` of (index key, primary key).
  Key bytes sort in IndexedDB order: a type tag, then the value. A write transaction keeps an undo list; abort plays it back.
- The host function `idb(op, ...)` gives the shim 21 operations: open, schema, put, query, cursor, delete and the others.
  A cursor walks the tree from the bytes of its last hit, so each step costs O(log n).
- `WebStorage` holds the stores by origin. A navigation and a new tab of the session find the same data. A document without
  a URL has one store of its own.
- `shims/idb.js` keeps the API objects, the events and the request order. Its data layer (sorted arrays per realm) is gone.
- Cause: WhatsApp link attempt 5. WhatsApp writes its login to IndexedDB before it navigates from `/login/` to `/`. The
  data died with the realm. Hypothesis: this caused the failed `setup` of the backend worker.

## Tests

- New `crates/bare/src/browser_idb_store_tests.rs`: data after a new document of the origin and in a new tab; nothing for
  another origin or session; key order of all key types and ranges; abort of rows, a key generator and a new database;
  cursors after a delete and with `advance`.
- `t` on the changed files and the IndexedDB tests of `browser_gaps_tests.rs`: all pass.

## Cost

archMachine, idle. New example `crates/bare/examples/idb_load.rs` (new rows in `cost_rows.txt`): SCALE*1000 puts in
transactions of 100, a get of each, an index cursor walk and a count. Old = the same tree with the old shim.

| row | old instructions | new instructions | change |
| --- | ---: | ---: | ---: |
| idb_load 1 | 393,977,253 | 327,016,999 | -17.0% |
| idb_load 10 | 4,730,727,324 | 3,122,982,148 | -34.0% |

Minimum of 3 runs (`instr.sh`). Wall time at x10, 3 runs: old 406-439 ms, new 309-312 ms. Peak RSS at x10: old 27.1-27.8 MiB,
new 24.6-25.3 MiB. Allocations rise from 1.23 M to 1.58 M (key bytes and records).

## Not tested

- An opaque origin: bare has no document with an opaque URL origin, so the `SecurityError` path has no test.
- Workers (step 3), locks between connections (step 4) and session export (step 5).
- A realm that dies in a write transaction leaves the writes in the store. Two realms can write at once with no lock.
- `put` still clones the value twice (a JS `structuredClone`, then the record).
- The other cost rows of the touched files (`browser_navigation_storage.rs`, `browser_navigation_state.rs`): the change there
  adds one field and one function.
- Windows.
