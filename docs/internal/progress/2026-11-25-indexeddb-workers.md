# Persistent profile step 4: IndexedDB in thread workers (2026-11-25)

Designs: `../design-persistent-profile.md` step 4 (section 8.3), `../design-indexeddb-store.md` step 3 (section 7) and the
concurrency model (section 10). Steps before: `2026-11-25-indexeddb-host-store.md`, `2026-11-25-profile-indexeddb-log.md`.

## What was there

- Bare has two kinds of dedicated worker. The default one is a child page on the page thread (`browser_child.rs`). It runs on a
  thread of its own only when the page has an isolated agent (`Cross-Origin-Embedder-Policy: require-corp`,
  `browser_child_thread.rs`).
- A worker on the page thread already used the store and the profile log of its page: `child_browser` gives it the
  `WebStorage` of the page. The new test of this case passed before the change.
- A worker on its own thread made its `Browser` with a new `WebStorage`. So its `indexedDB` was a separate, empty store, and its
  writes went to no profile.
- `SharedWorker` does not run in bare (`browser_worker.rs`), so it has no store to share.

## Change

- `WebStorage::idb_share` gives an `IdbShare`: the `Arc<Mutex<..>>` of the stores of the session and the storage limits.
  `IdbShare` is `Send`; `WebStorage` is not (its `localStorage` areas are `Rc`).
- `ThreadWorker::start` takes the share of the page and sends it to the worker thread. `run_worker` attaches
  `IdbShare::storage()`: the stores of the session, with an empty `localStorage` of its own, as before.
- The worker opens the store of its origin through the same `WebStorage::idb`. So the first open of an origin reads the log of the
  profile, and a commit of the worker goes to the same log frames. The `Saver` of the page writes them.
- No change in `shims/idb.js`, in the caps or in the page path.

## Concurrency

- Each host op locks the store of the origin, so a row and its index entries change as one step on any thread.
- Transactions are not isolated from each other (no lock table yet, step 4 of the store design). Two write transactions on
  different keys give the correct result. On the same key an update can be lost, and the log can then differ from memory.
  Chrome serializes overlapping `readwrite` transactions; bare does not yet. Details: section 10 of the store design.
- An upgrade does not wait for the connections of other realms and fires no `versionchange` or `blocked`. So there is no
  deadlock, but these events are not built.

## Tests

New file `browser_idb_worker_tests.rs`, 5 tests:

- a worker on the page thread puts a row with a `CryptoKey` and a `Blob`, and the page reads it; and the reverse (passed before
  the change too);
- the same with a worker on its own thread (failed before: `NotFoundError`, the page saw an empty database);
- with a profile, the write of a worker (both kinds) survives a restart: a new page on the profile reads it (failed before for
  the thread worker);
- a page and a worker thread each run 40 write transactions of 10 rows at once, each fifth one aborts: 640 rows, 320 index
  entries per side in key order, the same after a restart (failed before: the page reached no result in 20 s). A temporary
  probe showed 57 `begin` ops while a transaction of the other thread was live, so the transactions overlapped;
- an upgrade in the page while a worker holds a connection completes (`upgrade`, `success`) with no `versionchange` and no
  `blocked`, for both kinds of worker.

The new tests ran 5 times in a row: all pass. `t` on the changed files (`browser_idb_worker_tests`, `browser_child_tests`,
`browser_iframe_tests`, `profile_tests`, the tests in `browser_navigation_storage.rs`), and by hand `browser_threaded_worker_tests`,
`browser_idb_log_tests` and `browser_idb_store_tests`: all pass.

test262: no change to Vex or to the JS shims. Not run.

## Cost

Machine: archMachine, shared (load average 8 to 23 during the builds, 8 during the runs). Base: main `64fdbe00`. New: this branch
on `64fdbe00`. Release builds, one per tree, glibc. `tools/js/instr.sh` (minimum of 3 runs of `instructions:u`), 3 times per cell,
base and new one after the other.

| Row           | Base (min of 9) | New (min of 9) | Change |
| ------------- | --------------: | -------------: | -----: |
| `idb_load 1`  |     329,447,461 |    329,424,318 | -0.01% |
| `idb_load 10` |   3,136,984,231 |  3,136,878,738 | -0.00% |

Spread of the 3 `instr.sh` results per cell: 0.01% for `idb_load 1`. For `idb_load 10`, base 3,136.98 to 3,146.46 M and new
3,136.88 to 3,146.26 M (0.3%; the row is bimodal on both trees, as step 3 recorded). `tools/js/cost_rows.txt` has no worker row.
The change adds one `Arc` clone per start of a thread worker and nothing to the page path. No row is over the rule.

## Not tested

- The WhatsApp journey (link, close, open the profile, no new scan). WhatsApp sets no isolated agent as far as the bring-up notes
  show, so its worker runs on the page thread and already had the store. Hypothesis: the restart needs only steps 3 and 4.
- Two write transactions on the same key from two threads (known lost update, section 10 of the store design).
- A worker thread that ends inside a write transaction: its undo list and log buffer stay, and they block snapshots of the origin
  in this process (the same as a page that leaves in a transaction).
- Windows. jemalloc and `codegen-units=1` cost runs (no row moved under glibc).
