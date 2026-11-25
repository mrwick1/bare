# 2026-11-25: read an IndexedDB value with a platform object twice

Branch `idb-reread-host-objects`.

## Change

- `browser_idb.rs` `read`: a clone record gives its platform objects (a `Blob`, a `CryptoKey`) to the first read. A record that
  holds such objects is written again from the new copy, before any script runs. A record without them is read as before.
- The byte count of the store uses saturating arithmetic: a record written again can differ in size.
- Cause: WhatsApp link attempt 6. The second read of a stored value stopped the page with `damaged clone record`.

## Tests

- New `crates/bare/src/browser_idb_reread_tests.rs`: a value with a `Blob`, read by two `get` calls and one `getAll`. Before the
  fix: `Unsupported("damaged clone record")`. After: pass.
- `t` on `browser_idb.rs`: all pass.

## Cost

Not measured. A record without platform objects takes one more check (`moved().is_empty()`). `idb_load` stores no platform
objects.

## Not tested

- The WhatsApp journey with this fix.
- `history.state` reads a record the same way (`browser_navigation_state.rs` `materialize`). Hypothesis: a state with a `Blob`
  read twice fails the same way. Not checked.
- Windows.
