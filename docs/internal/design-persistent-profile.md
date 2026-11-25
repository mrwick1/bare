# Design: a persistent profile

Status: approved (owner, 2026-11-25): build option A in steps 1-5, one PR each.

## 1. Problem

The data of a session lives in memory only. When the process ends, the login is gone. WhatsApp link attempt 7 linked the
device, then a new build needed a new process, so it needed a new QR scan. The owner asked for a persistent profile: a
session that saves its data to disk and opens again with the same login.

## 2. Current design

- `WebStorage` (`browser_navigation_storage.rs`) holds the cookie jar, `localStorage`, `sessionStorage` and, since #1091,
  the IndexedDB stores (`browser_idb.rs`). All in memory.
- `save_state` (bare-mcp) and `save-state` (bare-drive) write a JSON file (Playwright `storageState`): cookies, `localStorage`
  and `sessionStorage`. Only on request. IndexedDB is not saved (`crates/drive/src/state.rs`). The file is mode 0600.
- `open state=NAME file=FILE` reads that file into a new session.
- The HTTP disk cache (`crates/net/src/disk_cache.rs`) is the only automatic disk data. It holds public replies only.

What WhatsApp needs to survive a restart: its cookies, `localStorage` (`WAWebPermanentStorage` mirrors there), and
IndexedDB (`wawc`, the signal keys, the chats). Some IndexedDB values hold `CryptoKey` and `Blob` objects.

## 3. Requirements

1. A named profile on disk. A session that opens it gets the cookies, `localStorage` and IndexedDB of the last session.
2. Saved without a request: a crash or a kill loses at most the last second of changes, and never leaves a damaged profile.
3. Low cost: a write costs the size of the change, not the size of the profile. Opening a profile reads an origin's
   IndexedDB only when a page of that origin opens `indexedDB`.
4. One process per profile at a time.
5. The files hold live credentials: directory mode 0700, files 0600, as `state.json` today.

## 4. How real engines do it

- Chrome: one profile directory. Cookies in SQLite (`Cookies`), `localStorage` in LevelDB, IndexedDB in LevelDB per origin
  (moving to SQLite; to verify). LevelDB is a log of changes plus sorted tables that it compacts. On Linux the cookie values
  are encrypted with a key from the desktop keyring; with no keyring the key is a fixed string.
- Firefox: one profile directory. Cookies, `localStorage` and IndexedDB in SQLite files, in WAL mode.
- WebKit: SQLite for IndexedDB and `localStorage`.
- A `profile.lock` file (Chrome `SingletonLock`, Firefox `parent.lock`) keeps a second process out.

All of them write a change when it commits, not a full copy. All of them keep `sessionStorage` in memory only.

## 5. Options

### A. A log of changes per origin, and a snapshot (recommended)

- Files: `<profile>/cookies.bin`, `<profile>/local/<origin hash>.bin` and `<profile>/idb/<origin hash>.log`.
- IndexedDB: each commit appends one record to the origin's log: the transaction's changes (put, delete, clear, schema,
  version). Record frame: length, CRC-32, payload. A replay at first open stops at the first bad frame, so a torn write at the
  end loses only that commit. When the log is more than twice the size of the live data, the store writes a new snapshot
  file and renames it over the log (temp file, `fsync`, `rename`).
- Cookies and `localStorage` are small. Each is written as a whole file (temp and rename) at most once per second after a
  change, and at close.
- The data stays in memory as now (`IdbStore`). The disk is a copy of it. A commit costs one append (O(change)). A load costs one
  read of the log (O(size)), once per origin.
- `fsync` at close and after each snapshot, not after each append. A power loss can lose the last appends; it cannot damage
  older ones.
- Work: medium. The new parts are the record coder for platform objects, the log and the snapshot.

### B. SQLite, as Firefox and WebKit do

- Robust and well tested. But it adds a C dependency to every build (Linux and Windows), about 1.5 MiB of code, and a second
  copy of the data (SQLite pages plus the memory store), or a redesign of `IdbStore` to read from SQLite.
- Revisit if a profile holds more data than memory allows.

### C. Write the whole profile again after each change

- The simplest. But WhatsApp writes the store many times a second during its download, and the store is tens of MB.
  O(size) per commit fails requirement 3. Rejected.

### D. Extend `save_state` with IndexedDB and call it from the agent

- Small. But it saves only when asked, so a crash loses the login. It fails requirement 2. It stays as the export format
  for sharing a session. Step 5 adds IndexedDB to it.

## 6. Choice

Option A. It writes only what changed, it survives a crash, and it keeps the store design of #1091.

## 7. Platform objects in a record

A clone record holds its `Blob`, `File` and `CryptoKey` objects as `Moved::Data` (`WebClone`), outside the bytes. On disk
each one gets a tag and its fields:

- `Blob`: type, bytes. `File`: also name and last modified time.
- `CryptoKey`: algorithm, usages, extractable flag, key bytes. A non-extractable key is written too: it must survive a
  restart, as in Chrome. The file mode protects it, not the flag.
- `WebAssembly.Module`: not written. A put of a value with a module into a persistent store throws `DataCloneError`, as in
  Chrome today.

## 8. Profile location and use

- `bare-mcp open url profile=NAME`, `bare-drive --profile NAME` and `bare --profile NAME`.
- Directory: `<data dir>/bare/profiles/NAME` (`$XDG_DATA_HOME` on Linux, `%LOCALAPPDATA%` on Windows; a new
  `bare_os::data_dir`). A path is also accepted.
- `profile.lock` with the process id. A second open of the profile fails with a clear error.
- Not saved: `sessionStorage` (as in all browsers), the HTTP cache (it has its own disk tier).

## 9. Steps, one PR each

1. Record coder for platform objects (`Blob`, `File`, `CryptoKey`). Test: a round trip of each.
2. Profile directory, lock, and cookies and `localStorage` files. `profile=` in bare-mcp and bare-drive. Test: log in on a
   test server, close, open, still logged in.
3. IndexedDB log: append at commit, replay at first open of the origin, snapshot. Tests: restart keeps data; a cut last frame
   loses only the last commit; an abort writes nothing.
4. IndexedDB for thread workers (design-indexeddb-store step 3), so WhatsApp's backend worker sees the saved keys.
5. `save_state` and `open state=` carry IndexedDB too (design-indexeddb-store step 5).

The WhatsApp check after step 3: link once, close bare, open the profile, see the chat list with no scan.

## 10. Benchmark

- `idb_load 1` and `10` with a profile on disk and without one. Goal: under +10% with a profile.
- Open time of a profile with 50 MB of IndexedDB, and peak RSS.
- The WhatsApp journey: time from open to chat list with a saved profile.

## 11. Owner answers (2026-11-25)

1. Encryption: no. File modes only (0700 and 0600), as `state.json` today.
2. Default profile: `open` with no `profile` uses the profile `default`, as a normal browser does. Only the entry points
   (bare-mcp, bare-drive, bare) do this; `Browser::new` in tests stays in memory. When `default` is locked by another process
   (two agents at once), the open goes on in memory and says so in its receipt, so a second agent is not blocked.
