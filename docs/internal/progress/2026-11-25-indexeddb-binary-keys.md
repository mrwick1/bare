# IndexedDB binary keys (2026-11-25)

## Change

The IndexedDB shim (`shims/idb.js`) takes `ArrayBuffer` and view keys. It compares them byte by byte. Order: number, date,
string, binary, array, as the standard says. The host coder (`browser_idb.rs`) already took binary keys.

## Why

WhatsApp link attempt 9: each `get` in table `sync-keys` gave `DataError`, and Dexie transactions failed after it.

## Tests

- New: `browser_idb_binary_key_tests.rs`. `indexedDB.cmp` order, put and get by a `Uint8Array` key path, a `DataView` key,
  a key range with a binary bound.
- `t` on the IndexedDB test files: all pass.

## Cost

Not measured. A number, date or string key leaves `rank` before the new test, so `idb_load` (number keys) runs the same code
as before. Hypothesis: no change on the `idb_load` rows.

## test262

Not run. The change is in a browser shim, not in Vex.
