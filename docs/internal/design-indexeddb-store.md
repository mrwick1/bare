# Design: one IndexedDB store per origin

Status: draft for owner review (2026-11-25). No code is built yet.

## 1. Problem

WhatsApp link attempt 5 linked the device, then showed "Logging out". The backend worker failed its `setup` step again and
again with "globals: PN-less setup requires a LID device jid".

Facts:

- After the link, the page goes from `/login/` to `/`. The new document boots again from storage.
- WhatsApp keeps its login in `WAWebPermanentStorage`. That module writes to memory first, then to `localStorage`, and it
  mirrors the data to the IndexedDB table `user` of the `wawc` database. The backend worker opens the same databases.
- Bare's `indexedDB` (`crates/bare/src/shims/idb.js`) keeps its data in a JS object of the realm (`dbs`). A navigation
  starts a new realm, so the data is gone. A worker has its own realm, so it sees an empty database.
- Two other errors of attempt 5 come from IndexedDB: `warmUpAllLidPnMappings` reads the contact table, and the push
  notification module failed to write the `local_storage` table.

Hypothesis: the lost IndexedDB data causes the failed `setup`. It is not proven. A probe in the next link attempt can
check it (section 8). The design is needed in any case: the chat list and the messages live in IndexedDB, and the backend
worker writes them.

## 2. Current design

- `shims/idb.js`, 541 lines, made at the first read of `indexedDB` (`browser_shims.rs`).
- Data: `dbs[name] = { version, stores: { name: { records, indexes, autoIncrement, keyPath } } }`, in the realm.
- Values: `structuredClone` on put and on get.
- Transactions: the requests of a transaction run one after another in microtasks. There is no lock between transactions
  and no `versionchange` or `blocked` event between connections, because one realm has no other connection to tell.
- Lifetime: the realm. No disk, no session export (`browser_save_state` keeps cookies and `localStorage` only).
- Cost: nothing until a page reads `indexedDB`.

## 3. Requirements

1. One store for each origin in a session. Every realm of the origin uses it: a page after a navigation, a frame, a
   dedicated worker on another thread.
2. Transactions follow the standard: read-only transactions on the same stores can overlap; a read-write transaction has
   its stores alone; a `versionchange` transaction waits for the other connections to close, after `versionchange` and
   `blocked` events.
3. A value is stored as a structured clone record, as in the standard. A get gives a new copy in the realm that asks.
4. Low memory: one copy of the data per origin, not one per realm.
5. Later: the store goes into the session export, so a saved WhatsApp login works after a restart without a new QR scan.

## 4. How real engines do it

- Firefox: one SQLite database per origin, in the parent process. Content processes send requests over IPC. Values are
  structured clone buffers.
- WebKit: an IndexedDB server in a separate process, with SQLite. Web processes send requests over IPC.
- Chrome: the backend is in the browser process, and renderers talk to it over Mojo. The values are V8 serializer bytes.
  To verify: the move of the backend from LevelDB to SQLite.
- Ladybird: LibWeb has its own IndexedDB code. To verify: if it keeps its data in memory only.

All of them keep the data outside the realm. A realm holds connections and request objects only. Bare does the opposite.

## 5. Options

### A. A store in the host, the API stays in JS (recommended)

- Data: `IdbStore` per origin, shared as `Arc<Mutex<..>>`. Each object store is a `BTreeMap<Box<[u8]>, Cloned>`. The key
  is encoded into bytes that sort in IndexedDB key order (type tag first: number, date, string, binary, array, as Chrome
  does). Each index is a `BTreeMap<(index key bytes, primary key bytes), ()>`.
- Values: the existing `Cloned` record (`bare_js_api::clone`). It is `Send`, so a worker thread can read it. Put clones
  once, get deserializes once, as the standard requires.
- Algorithms: get, put and delete are O(log n). A cursor is a range walk of the `BTreeMap`. A transaction keeps an undo
  list for abort, like the shim does now.
- Locks: a lock table per database. A transaction asks for its scope. If the scope is free, it runs. Otherwise it waits in a
  queue, and the releasing thread wakes the waiting realm (`PortHub` wake, as `postMessage` to a worker does).
- `idb.js` keeps the API objects, the events and the request order. Only the data access calls host functions.
- Sharing: the session `WebStorage` holds the stores by origin, so a navigation and a frame find them. A thread worker gets
  the `Arc` of its origin at start, like the wasm engine (`adopt_engine`).
- Memory: one copy per origin. Each record costs its clone bytes plus about 48 bytes of map overhead.
- Work: medium. The shim loses its data layer; the host gains a store, the key coder, the locks and about 10 host functions.

### B. All of IndexedDB in Rust

- The same store as A, and the API objects, events and request queue in Rust as well.
- Faster per request, no JS shim to parse. But it adds about 2,000 lines of host code, and the shim is already written and
  tested. A can move to B later, one interface at a time, if a profile shows the shim is the cost.

### C. Keep a store per realm and copy changes between realms

- Each realm keeps its own `dbs`. A committed transaction sends its changes to the other realms of the origin, like a
  `BroadcastChannel`.
- Rejected: N copies of the data, no real locks between realms, and a worker can read data that is not committed yet.

### D. SQLite on disk, as Firefox and WebKit do

- Durable and low in memory for big databases. But it adds a dependency and disk I/O to each transaction, and bare runs
  sessions that end in minutes. The store of A can write a snapshot into the session export instead. Revisit if a page
  keeps more data than memory allows.

## 6. Choice

Option A. It removes a whole kind of loss (data that dies with a realm) and keeps one copy of the data. It reuses
`Cloned`, the `PortHub` wake and the `WebStorage` sharing that bare already has.

## 7. Steps, one PR each

1. `IdbStore`, the key coder and the host functions. `idb.js` uses them in one realm. Tests: the existing IndexedDB tests
   pass, plus key order tests.
2. Share the store in a session: a navigation and a frame see the same data. Test: put, navigate, get.
3. Workers: a thread worker gets the store of its origin. Test: the page puts, a worker gets.
4. Locks between connections, `versionchange` and `blocked` events. Tests from the standard's examples.
5. Session export and import of the store (`browser_save_state`), so a saved login survives a restart.

## 8. Check before step 1

In the next WhatsApp link attempt, record whether `setup` sends a null `deviceJid` and `lidDeviceJid` (null or set only,
never the values). If the page sends set values and the worker sees them, the cause is elsewhere, and this design moves
behind the next blocker. The design is still needed for the chat list.

## 9. Benchmark

- There is no IndexedDB row in `cost_rows.txt`. Step 1 adds `web_apis_load idb 1` and `10`: open, 1,000 puts, 1,000 gets
  and a cursor walk. Compare instruction counts with the shim before step 1.
- Peak memory: a page that stores 10 MiB, with one and with two realms of the origin.
- The WhatsApp journey: link, chat list, read a conversation.
