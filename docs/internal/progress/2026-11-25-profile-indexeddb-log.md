# Persistent profile step 3: the IndexedDB log (2026-11-25)

Design: `../design-persistent-profile.md`, option A, step 3 (section 8.2 has the details as built). Steps 1 and 2: `2026-11-25-profile-record-coder.md`, `2026-11-25-profile-storage.md`.

## Change

- A profile now saves IndexedDB. Each origin has a log, `<profile>/idb/<FNV-1a 64 of the origin>.log`. The file starts with a magic number and the origin. Frames follow: u32 length, CRC-32, payload.
- A payload holds the host ops of one committed write transaction: version, store, index, put, delete, clear, index fill. `deleteDatabase` writes a frame of its own. Values use the record coder of step 1 (`profile_record.rs`).
- A write transaction collects its ops in a buffer from the call of each op. The commit moves the buffer into the pending frames. An abort drops it. A read-only transaction has no buffer (id 0 in the shim).
- Why not read the rows at the commit: there are no locks between transactions yet. At a commit the store can hold a write of another live transaction. When that one aborted, the disk would keep its write.
- Replay: at the first open of an origin in the process (`WebStorage::idb`). It stops at the first bad frame and cuts the file there. A torn write loses only the last commit.
- Snapshot: when the log is more than twice the live data (and at least 64 KiB), and no write transaction is live. The snapshot goes to a temporary file, then `fsync`, then a rename over the log.
- Timing: the pending frames go to the file with the step-2 writes (`Saver::tick`, at most once a second). `Saver::flush` and the `Saver` drop also `fsync`. After the drop the logs write nothing more, because the lock is free.
- A put of a `WebAssembly.Module` into a store of a profile fails with `DataCloneError` (`profile_record::storable`).
- No profile: no file work. The cost is one `Option` test per host op.
- `examples/idb_load`: `BARE_IDB_LOAD_PROFILE=DIR` runs the load with a new profile in DIR. `Saver` and its `new` and `tick` are public for that.

## Tests

New file `browser_idb_log_tests.rs`, 6 tests. All failed before the change and pass now:

- a new process reads the saved rows and index (string, number, Date, binary and array keys; Blob, Uint8Array and CryptoKey values; file modes 0700 and 0600);
- a cut last frame loses only the last commit, and a later commit is read after the cut;
- an abort and a read-only transaction write nothing (the file is the same, byte for byte);
- a long log becomes a snapshot (300 commits of 1 KiB to one key: the file is under 8 KiB after close);
- a deleted database and an upgrade survive a restart (store deleted, new `autoIncrement` store, unique index);
- a `WebAssembly.Module` does not go into a saved store.

`t` (the tests of the changed files: `browser_idb_log_tests`, `browser_idb_store_tests`, `profile_tests`, `profile_record_tests`, the tests in `browser_navigation_storage.rs`), and by hand `browser_idb_tx_tests`, `browser_idb_cursor_tests`, `browser_idb_many_requests_tests` and `browser_idb_reread_tests`: all pass.

test262: no change to Vex or to the JS shims. Not run.

## Cost

Machine: archMachine, shared (load average 6 to 11). Base: main `7e256648`. New: this branch merged with `7e256648`. Release builds, one per tree. `tools/js/instr.sh` (minimum of 3 runs of `instructions:u`), 3 times per cell, so 9 runs per cell. Glibc only.

| Row           | Base (min of 9) | New, no profile | Change |  New, profile | Change |
| ------------- | --------------: | --------------: | -----: | ------------: | -----: |
| `idb_load 1`  |     329,152,024 |     329,175,319 | +0.01% |   330,364,706 | +0.37% |
| `idb_load 10` |   3,133,891,564 |   3,133,843,876 | -0.00% | 3,145,978,058 | +0.39% |

Spread of the 3 `instr.sh` results per cell: at most 0.01% for `idb_load 1`, and 0.3% for `idb_load 10` (base 3,133.9 to 3,143.6 M; the row is bimodal on both trees).

- Without a profile: within the +0.5% rule.
- With a profile: +0.4%, well under the goal of +10%. User-mode instructions only: the `write` and `fsync` time in the kernel is not in the count.
- Peak RSS of `idb_load 10`, 3 runs: base 23,908 to 24,088 kB, new 24,024 to 24,112 kB, new with a profile 25,340 to 25,392 kB. The extra 1.3 MiB is the pending buffer: the run takes less than one second, so the 1.1 MB log goes to the file at the end only. Wall time is not given: the machine was loaded.
- The log of `idb_load 10` is 1,171,600 bytes for 10,000 rows.

## Gates and limits

- The `WebAssembly.Module` check runs in the host op, after `put` returns. So the page gets `DataCloneError` as a request error, and the transaction aborts. The standard throws at the call. `shims/idb.js` is not changed in this PR (another change edits its request scheduling).
- A write transaction that never ends (a page leaves during the transaction) keeps its buffer, and it blocks snapshots for that origin in the process. Hypothesis: rare; the undo list of such a transaction stays in the store too, as before.
- Two write transactions on the same rows can interleave (no locks, design-indexeddb-store step 4). Then the order of the frames is the commit order, not the write order, and the disk can differ from memory. Step 4 of that design removes this.
- A tab that a page opens shares the `WebStorage`, so it shares the logs too.

## Not tested

- The WhatsApp journey (link, close, open with no scan). The worker of WhatsApp needs step 4 first.
- Open time and peak RSS of a profile with 50 MB of IndexedDB (design section 10).
- A kill during a snapshot (the rename keeps the old file or the new one; not tested by a real kill).
- A full disk or a write error (the log prints one message and keeps the origin in memory).
- Windows (the file modes are Unix only, as in step 2).
- jemalloc and `codegen-units=1` runs of the cost rows (both rows are under the rule under glibc).
