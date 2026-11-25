//! The data of `indexedDB` (`shims/idb.js`, design `docs/internal/design-indexeddb-store.md`). One `IdbStore` holds the databases of
//! an origin for the session. It lives in the session `WebStorage`, outside any realm, so a navigation keeps the data. `idb.js` keeps
//! the API objects, the events and the order of requests, and calls `idb(op, ...)` here for every read and write.
//!
//! Keys are encoded into bytes that sort in IndexedDB key order: a type tag (number < date < string < binary < array), then the
//! value. Values are structured clone records (`Cloned`). An object store is a `BTreeMap` from key bytes to its row; an index is a
//! set of (index key bytes, primary key bytes). A transaction keeps an undo list in the store; `abort` plays it backward.

use crate::browser::Dom;
use crate::browser_scoped::*;
use crate::eng::Eng;
use bare_js_api::clone::Cloned;
use bare_js_api::{Args, Local, Str, Type};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::ops::Bound;
use std::sync::{Arc, Mutex};

#[path = "browser_idb_log.rs"]
pub(crate) mod log;

/// The most bytes of records that one origin keeps. A write past it is a `QuotaExceededError`.
const MAX_BYTES: usize = 512 << 20;
/// The deepest array key. A deeper one (or a cycle) is not a valid key.
const MAX_DEPTH: usize = 64;

const NUMBER: u8 = 0x10;
const DATE: u8 = 0x20;
const STRING: u8 = 0x30;
const BINARY: u8 = 0x40;
const ARRAY: u8 = 0x50;

type Key = Box<[u8]>;

/// The databases of one origin.
#[derive(Default)]
pub(crate) struct IdbStore {
    dbs: HashMap<String, Db>,
    /// The undo list of each live transaction.
    txs: HashMap<u32, Vec<Undo>>,
    next_tx: u32,
    bytes: usize,
    /// The disk log of a profile (`browser_idb_log.rs`). `None`: memory only.
    log: Option<Box<log::Log>>,
}

/// A store shared by the realms of an origin.
pub(crate) type Shared = Arc<Mutex<IdbStore>>;

struct Db {
    version: f64,
    stores: BTreeMap<String, Store>,
}

/// A key path as the page gave it: none, one path, or a list.
#[derive(Clone)]
enum Kp {
    None,
    One(String),
    Many(Vec<String>),
}

struct Store {
    kp: Kp,
    auto: bool,
    next: f64,
    rows: BTreeMap<Key, Row>,
    indexes: BTreeMap<String, Index>,
    next_ix: u32,
}

struct Index {
    id: u32,
    kp: Kp,
    unique: bool,
    multi: bool,
    entries: BTreeSet<(Key, Key)>,
}

struct Row {
    value: Cloned,
    /// The index entries of the row, by index id. An entry of a deleted index stays until the row changes: ids are not reused.
    ix: Vec<(u32, Key)>,
}

enum Undo {
    Row { db: String, store: String, key: Key, old: Option<Row> },
    Next { db: String, store: String, next: f64 },
    Version { db: String, old: f64 },
    StoreMade { db: String, store: String },
    StoreGone { db: String, store: String, st: Store },
    IndexMade { db: String, store: String, ix: String },
    IndexGone { db: String, store: String, ix: String, index: Index },
}

impl IdbStore {
    fn store(&mut self, db: &str, store: &str) -> Option<&mut Store> {
        self.dbs.get_mut(db)?.stores.get_mut(store)
    }

    /// Put `row` at `key` (or remove the row), with its index entries. Gives the old row.
    fn set_row(&mut self, db: &str, store: &str, key: &[u8], row: Option<Row>) -> Option<Row> {
        let st = self.store(db, store)?;
        let old = st.rows.remove(key);
        let mut freed = 0;
        if let Some(o) = &old {
            freed = o.value.bytes().len();
            for (id, k) in &o.ix {
                if let Some(ix) = st.indexes.values_mut().find(|i| i.id == *id) {
                    ix.entries.remove(&(k.clone(), key.into()));
                }
            }
        }
        let mut added = 0;
        if let Some(r) = row {
            added = r.value.bytes().len();
            for (id, k) in &r.ix {
                if let Some(ix) = st.indexes.values_mut().find(|i| i.id == *id) {
                    ix.entries.insert((k.clone(), key.into()));
                }
            }
            st.rows.insert(key.into(), r);
        }
        self.bytes = self.bytes.saturating_sub(freed) + added;
        old
    }

    /// Put `row` at `key` as a put does: an `autoIncrement` store moves its key generator past a number key. Gives the old row.
    fn put_row(&mut self, db: &str, store: &str, k: Key, row: Row) -> Option<Row> {
        let so = self.store(db, store)?;
        if so.auto && matches!(kv_number(&k), Some(n) if n >= so.next) {
            so.next = (kv_number(&k).expect("a number").floor() + 1.0).min(9007199254740992.0);
        }
        self.set_row(db, store, &k, Some(row))
    }

    /// Remove database `name`. Gives its version, 0 when it did not exist.
    fn drop_db(&mut self, name: &str) -> f64 {
        self.dbs.remove(name).map_or(0.0, |db| {
            self.bytes = self.bytes.saturating_sub(db.stores.values().flat_map(|s| s.rows.values()).map(|r| r.value.bytes().len()).sum::<usize>());
            db.version
        })
    }

    fn drop_store(&mut self, db: &str, store: &str) -> Option<Store> {
        let gone = self.dbs.get_mut(db)?.stores.remove(store)?;
        self.bytes = self.bytes.saturating_sub(gone.rows.values().map(|r| r.value.bytes().len()).sum::<usize>());
        Some(gone)
    }

    /// Add the ops that `f` writes to the disk log of transaction `t` (a profile only).
    fn rec(&mut self, t: u32, f: impl FnOnce(&mut log::W)) {
        if let Some(l) = &mut self.log {
            l.rec(self.txs.contains_key(&t).then_some(t), f);
        }
    }

    /// Write the log of this origin (a profile only). `sync`: `fsync` too.
    pub(crate) fn flush(&mut self, sync: bool) {
        if let Some(l) = &mut self.log {
            l.flush(&self.dbs, !self.txs.is_empty(), sync);
        }
    }

    fn log(&mut self, t: u32, u: Undo) {
        if let Some(l) = self.txs.get_mut(&t) {
            l.push(u);
        }
    }

    fn undo(&mut self, u: Undo) {
        match u {
            Undo::Row { db, store, key, old } => {
                self.set_row(&db, &store, &key, old);
            }
            Undo::Next { db, store, next } => {
                if let Some(st) = self.store(&db, &store) {
                    st.next = next;
                }
            }
            Undo::Version { db, old } => {
                if old == 0.0 {
                    if let Some(gone) = self.dbs.remove(&db) {
                        self.bytes = self.bytes.saturating_sub(gone.stores.values().flat_map(|s| s.rows.values()).map(|r| r.value.bytes().len()).sum::<usize>());
                    }
                } else if let Some(d) = self.dbs.get_mut(&db) {
                    d.version = old;
                }
            }
            Undo::StoreMade { db, store } => {
                if let Some(st) = self.dbs.get_mut(&db).and_then(|d| d.stores.remove(&store)) {
                    self.bytes = self.bytes.saturating_sub(st.rows.values().map(|r| r.value.bytes().len()).sum::<usize>());
                }
            }
            Undo::StoreGone { db, store, st } => {
                self.bytes += st.rows.values().map(|r| r.value.bytes().len()).sum::<usize>();
                if let Some(d) = self.dbs.get_mut(&db) {
                    d.stores.insert(store, st);
                }
            }
            Undo::IndexMade { db, store, ix } => {
                if let Some(st) = self.store(&db, &store) {
                    st.indexes.remove(&ix);
                }
            }
            Undo::IndexGone { db, store, ix, index } => {
                if let Some(st) = self.store(&db, &store) {
                    st.indexes.insert(ix, index);
                }
            }
        }
    }
}

/// The store of the origin of `d`'s document.
fn shared<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>) -> Res<'s, Shared> {
    let n = d.nav.s.borrow();
    // A document without a URL (a test, a tool) has one store of its own: "".
    let origin = n.entries.get(n.cur).map_or(String::new(), |e| e.url.origin());
    match origin {
        o if o != "null" => Ok(n.store.idb(&o)),
        _ => {
            drop(n);
            Err(d.dom_err_s(s, "SecurityError", "Access to the IndexedDB API is denied in this context."))
        }
    }
}

// ------------------------------------------------------------------------------------------ keys

/// Append the bytes of key `v` to `out`. Gives false when `v` is not a valid key. `idb.js` gives a `Date` as `{ t: time }`.
fn enc<'s, E: Eng>(s: &mut Sc<'s, E>, v: Local<'s>, out: &mut Vec<u8>, depth: usize) -> Res<'s, bool> {
    match s.type_of(v) {
        Type::Number => {
            let n = s.as_number(v).unwrap_or(f64::NAN);
            if n.is_nan() {
                return Ok(false);
            }
            out.push(NUMBER);
            out.extend(sortable(n));
        }
        Type::String => {
            out.push(STRING);
            match s.as_str(v) {
                Some(Str::Latin1(b)) => b.iter().for_each(|&c| out.extend([1, 0, c])),
                Some(Str::Utf16(u)) => u.iter().for_each(|&c| out.extend([1, (c >> 8) as u8, c as u8])),
                None => return Ok(false),
            }
            out.push(0);
        }
        Type::Object if depth < MAX_DEPTH => {
            if s.is_array(v)? {
                out.push(ARRAY);
                let n = s.array_len(v)?.unwrap_or(0);
                for i in 0..n {
                    let k = s.number(f64::from(i));
                    let e = s.get(v, k)?;
                    if !enc::<E>(s, e, out, depth + 1)? {
                        return Ok(false);
                    }
                }
                out.push(0);
            } else if let Some(b) = s.bytes(v) {
                out.push(BINARY);
                b.iter().for_each(|&c| out.extend([1, c]));
                out.push(0);
            } else {
                let t = s.get_str(v, "t")?;
                let n = s.as_number(t).unwrap_or(f64::NAN);
                if n.is_nan() {
                    return Ok(false);
                }
                out.push(DATE);
                out.extend(sortable(n));
            }
        }
        _ => return Ok(false),
    }
    Ok(true)
}

/// The bytes of `n` that sort as the numbers do. -0 is 0.
fn sortable(n: f64) -> [u8; 8] {
    let b = (if n == 0.0 { 0.0 } else { n }).to_bits();
    (if b >> 63 == 1 { !b } else { b | 1 << 63 }).to_be_bytes()
}

fn unsortable(b: &[u8]) -> f64 {
    let b = u64::from_be_bytes(b[..8].try_into().expect("8 bytes"));
    f64::from_bits(if b >> 63 == 1 { b & !(1 << 63) } else { !b })
}

/// The key of `v`, or a `DataError`.
fn key<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, v: Local<'s>) -> Res<'s, Key> {
    let mut out = Vec::new();
    if enc::<E>(s, v, &mut out, 0)? { Ok(out.into()) } else { Err(d.dom_err_s(s, "DataError", "The parameter is not a valid key.")) }
}

/// The JS value of the key at the start of `b`, and the bytes after it.
fn dec<'s, 'b, E: Eng>(s: &mut Sc<'s, E>, b: &'b [u8]) -> Res<'s, (Local<'s>, &'b [u8])> {
    let (tag, rest) = (b[0], &b[1..]);
    match tag {
        NUMBER => Ok((s.number(unsortable(rest)), &rest[8..])),
        DATE => {
            let g = s.global();
            let c = s.get_str(g, "Date")?;
            let t = s.number(unsortable(rest));
            Ok((s.construct(c, &[t], None)?, &rest[8..]))
        }
        STRING => {
            let mut units = Vec::new();
            let mut i = 0;
            while rest[i] == 1 {
                units.push(u16::from_be_bytes([rest[i + 1], rest[i + 2]]));
                i += 3;
            }
            Ok((s.string_utf16(&units)?, &rest[i + 1..]))
        }
        BINARY => {
            let mut bytes = Vec::new();
            let mut i = 0;
            while rest[i] == 1 {
                bytes.push(rest[i + 1]);
                i += 2;
            }
            Ok((s.new_array_buffer(bytes)?, &rest[i + 1..]))
        }
        _ => {
            let mut items = Vec::new();
            let mut rest = rest;
            while rest[0] != 0 {
                let (v, r) = dec::<E>(s, rest)?;
                items.push(v);
                rest = r;
            }
            Ok((s.new_array(&items)?, &rest[1..]))
        }
    }
}

fn key_value<'s, E: Eng>(s: &mut Sc<'s, E>, b: &[u8]) -> Res<'s, Local<'s>> {
    dec::<E>(s, b).map(|(v, _)| v)
}

// ------------------------------------------------------------------------------------------ arguments

fn kp_of<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, v: Local<'s>) -> Res<'s, Kp> {
    match s.type_of(v) {
        Type::String => Ok(Kp::One(d.string_arg_s(s, v)?)),
        Type::Object => {
            let n = s.array_len(v)?.unwrap_or(0);
            let mut out = Vec::with_capacity(n as usize);
            for i in 0..n {
                let k = s.number(f64::from(i));
                let e = s.get(v, k)?;
                out.push(d.string_arg_s(s, e)?);
            }
            Ok(Kp::Many(out))
        }
        _ => Ok(Kp::None),
    }
}

fn kp_value<'s, E: Eng>(s: &mut Sc<'s, E>, kp: &Kp) -> Res<'s, Local<'s>> {
    match kp {
        Kp::None => Ok(s.null()),
        Kp::One(p) => s.string(p),
        Kp::Many(ps) => {
            let items = ps.iter().map(|p| s.string(p)).collect::<Result<Vec<_>, _>>()?;
            s.new_array(&items)
        }
    }
}

/// A key range from the arguments `i..i+4`: lower, upper (undefined: open end), lowerOpen, upperOpen.
struct Range {
    lo: Bound<Pair>,
    up: Bound<Pair>,
}

/// An index entry: (index key, primary key). A store row is (key, key).
type Pair = (Key, Key);

/// `k` followed by 0xFF. Key bytes are prefix-free (each key ends itself), so this sorts after `k` and before every key after `k`.
fn after(k: &[u8]) -> Key {
    [k, &[0xFF]].concat().into()
}

/// The first entry with key `k`, or past all entries of `k` (`past`).
fn edge(k: &[u8], past: bool) -> Pair {
    (if past { after(k) } else { k.into() }, Key::default())
}

/// The bound of a row key from the bound of an entry: a row has its key as its primary key.
fn row_bound(b: &Bound<Pair>) -> Bound<&[u8]> {
    match b {
        Bound::Included(p) => Bound::Included(&p.0),
        Bound::Excluded(p) => Bound::Excluded(&p.0),
        Bound::Unbounded => Bound::Unbounded,
    }
}

/// Whether `lo..up` holds no entry. `BTreeMap::range` panics on such a range.
fn empty(lo: &Bound<Pair>, up: &Bound<Pair>) -> bool {
    match (lo, up) {
        (Bound::Included(a), Bound::Included(b)) => a > b,
        (Bound::Included(a) | Bound::Excluded(a), Bound::Included(b) | Bound::Excluded(b)) => a >= b,
        _ => false,
    }
}

fn range<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, a: Args<'s>, i: usize) -> Res<'s, Range> {
    let bound = |s: &mut Sc<'s, E>, j: usize| {
        let v = arg_s::<E>(s, a, j);
        if matches!(s.type_of(v), Type::Undefined) { Ok(None) } else { key(d, s, v).map(Some) }
    };
    let (lo, up) = (bound(s, i)?, bound(s, i + 1)?);
    let flag = |s: &mut Sc<'s, E>, j: usize| {
        let v = arg_s::<E>(s, a, j);
        s.as_bool(v).unwrap_or(false)
    };
    let (lo_open, up_open) = (flag(s, i + 2), flag(s, i + 3));
    Ok(Range {
        lo: lo.map_or(Bound::Unbounded, |k| Bound::Included(edge(&k, lo_open))),
        up: up.map_or(Bound::Unbounded, |k| Bound::Excluded(edge(&k, !up_open))),
    })
}

/// The first entry in `lo..up` from the front (or from the back), of the store rows or of an index.
fn first(so: &Store, ix: Option<&Index>, lo: &Bound<Pair>, up: &Bound<Pair>, back: bool) -> Option<Pair> {
    if empty(lo, up) {
        return None;
    }
    match ix {
        None => {
            let mut it = so.rows.range::<[u8], _>((row_bound(lo), row_bound(up))).map(|(k, _)| (k.clone(), k.clone()));
            if back { it.next_back() } else { it.next() }
        }
        Some(ix) => {
            let mut it = ix.entries.range::<Pair, _>((lo.as_ref(), up.as_ref())).cloned();
            if back { it.next_back() } else { it.next() }
        }
    }
}

fn num<'s, E: Eng>(s: &mut Sc<'s, E>, a: Args<'s>, i: usize) -> f64 {
    let v = arg_s::<E>(s, a, i);
    s.as_number(v).unwrap_or(0.0)
}

fn not_found<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>) -> bare_js_api::Error<'s> {
    d.dom_err_s(s, "NotFoundError", "The specified object store was not found.")
}

// ------------------------------------------------------------------------------------------ the host function

/// `idb(op, ...)` of `shims/idb.js`. The ops are numbered as `OP` in the shim.
pub(crate) fn idb_host<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, _: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let op = num::<E>(s, a, 0) as u8;
    let sh = shared(d, s)?;
    let mut g = sh.lock().unwrap_or_else(|e| e.into_inner());
    let st = &mut *g;
    let text = |s: &mut Sc<'s, E>, i: usize| d.arg_str_s(s, a, i);
    match op {
        // open(name) -> the version, 0 when the database does not exist
        0 => {
            let name = text(s, 1)?;
            Ok(s.number(st.dbs.get(&name).map_or(0.0, |db| db.version)))
        }
        // databases() -> [[name, version]]
        1 => {
            let list: Vec<(String, f64)> = st.dbs.iter().map(|(n, db)| (n.clone(), db.version)).collect();
            let mut out = Vec::with_capacity(list.len());
            for (n, v) in list {
                let pair = [s.string(&n)?, s.number(v)];
                out.push(s.new_array(&pair)?);
            }
            s.new_array(&out)
        }
        // deleteDatabase(name) -> the old version
        2 => {
            let name = text(s, 1)?;
            let old = st.drop_db(&name);
            if old != 0.0 {
                st.rec(0, |w| {
                    w.op(log::DROP_DB, &name, "");
                });
            }
            Ok(s.number(old))
        }
        // begin() -> the transaction id
        3 => {
            st.next_tx = st.next_tx.wrapping_add(1);
            let t = st.next_tx;
            st.txs.insert(t, Vec::new());
            Ok(s.number(f64::from(t)))
        }
        // commit(t), abort(t)
        4 | 5 => {
            let t = num::<E>(s, a, 1) as u32;
            if let Some(l) = &mut st.log {
                l.end(t, op == 4);
            }
            if let Some(list) = st.txs.remove(&t) {
                if op == 5 {
                    for u in list.into_iter().rev() {
                        st.undo(u);
                    }
                }
            }
            Ok(s.undefined())
        }
        // setVersion(t, name, version): makes the database when it is new
        6 => {
            let t = num::<E>(s, a, 1) as u32;
            let name = text(s, 2)?;
            let v = num::<E>(s, a, 3);
            let old = st.dbs.get(&name).map_or(0.0, |db| db.version);
            st.dbs.entry(name.clone()).or_insert_with(|| Db { version: 0.0, stores: BTreeMap::new() }).version = v;
            st.rec(t, |w| {
                w.op(log::VERSION, &name, "").f64(v);
            });
            st.log(t, Undo::Version { db: name, old });
            Ok(s.undefined())
        }
        // schema(name) -> [[store, keyPath, autoIncrement, [[index, keyPath, unique, multiEntry]]]]
        7 => {
            let name = text(s, 1)?;
            let Some(db) = st.dbs.get(&name) else { return s.new_array(&[]) };
            let mut out = Vec::new();
            for (sn, so) in &db.stores {
                let mut ixs = Vec::new();
                for (iname, ix) in &so.indexes {
                    let row = [s.string(iname)?, kp_value::<E>(s, &ix.kp)?, s.boolean(ix.unique), s.boolean(ix.multi)];
                    ixs.push(s.new_array(&row)?);
                }
                let row = [s.string(sn)?, kp_value::<E>(s, &so.kp)?, s.boolean(so.auto), s.new_array(&ixs)?];
                out.push(s.new_array(&row)?);
            }
            s.new_array(&out)
        }
        // createStore(t, db, store, keyPath, autoIncrement)
        8 => {
            let t = num::<E>(s, a, 1) as u32;
            let (db, store) = (text(s, 2)?, text(s, 3)?);
            let kp_v = arg_s::<E>(s, a, 4);
            let kp = kp_of(d, s, kp_v)?;
            let auto_v = arg_s::<E>(s, a, 5);
            let auto = s.as_bool(auto_v).unwrap_or(false);
            if !st.dbs.contains_key(&db) {
                return Err(not_found(d, s));
            }
            st.rec(t, |w| {
                w.op(log::MAKE_STORE, &db, &store).kp(&kp).u8(auto as u8);
            });
            st.dbs.get_mut(&db).expect("checked").stores.insert(store.clone(), Store { kp, auto, next: 1.0, rows: BTreeMap::new(), indexes: BTreeMap::new(), next_ix: 0 });
            st.log(t, Undo::StoreMade { db, store });
            Ok(s.undefined())
        }
        // deleteStore(t, db, store)
        9 => {
            let t = num::<E>(s, a, 1) as u32;
            let (db, store) = (text(s, 2)?, text(s, 3)?);
            let Some(gone) = st.drop_store(&db, &store) else { return Err(not_found(d, s)) };
            st.rec(t, |w| {
                w.op(log::DROP_STORE, &db, &store);
            });
            st.log(t, Undo::StoreGone { db, store, st: gone });
            Ok(s.undefined())
        }
        // createIndex(t, db, store, index, keyPath, unique, multiEntry): an empty index; `ixFill` adds the rows
        10 => {
            let t = num::<E>(s, a, 1) as u32;
            let (db, store, ix) = (text(s, 2)?, text(s, 3)?, text(s, 4)?);
            let kp_v = arg_s::<E>(s, a, 5);
            let kp = kp_of(d, s, kp_v)?;
            let (u, m) = (arg_s::<E>(s, a, 6), arg_s::<E>(s, a, 7));
            let (unique, multi) = (s.as_bool(u).unwrap_or(false), s.as_bool(m).unwrap_or(false));
            let Some(so) = st.store(&db, &store) else { return Err(not_found(d, s)) };
            let id = so.next_ix;
            so.next_ix += 1;
            st.rec(t, |w| {
                w.op(log::MAKE_INDEX, &db, &store).str(&ix).kp(&kp).u8(unique as u8).u8(multi as u8);
            });
            let so = st.store(&db, &store).expect("checked");
            so.indexes.insert(ix.clone(), Index { id, kp, unique, multi, entries: BTreeSet::new() });
            st.log(t, Undo::IndexMade { db, store, ix });
            Ok(s.undefined())
        }
        // deleteIndex(t, db, store, index)
        11 => {
            let t = num::<E>(s, a, 1) as u32;
            let (db, store, ix) = (text(s, 2)?, text(s, 3)?, text(s, 4)?);
            let Some(index) = st.store(&db, &store).and_then(|so| so.indexes.remove(&ix)) else { return Err(not_found(d, s)) };
            st.rec(t, |w| {
                w.op(log::DROP_INDEX, &db, &store).str(&ix);
            });
            st.log(t, Undo::IndexGone { db, store, ix, index });
            Ok(s.undefined())
        }
        // nextKey(db, store) -> the next key of the generator
        12 => {
            let (db, store) = (text(s, 1)?, text(s, 2)?);
            let Some(so) = st.store(&db, &store) else { return Err(not_found(d, s)) };
            Ok(s.number(so.next))
        }
        // put(t, db, store, key, value, add, [[index, [keys]]])
        13 => put(d, s, a, st),
        // query(db, store, index|null, lower, upper, lowerOpen, upperOpen, what, count): what 0 value, 1 key, 2 values, 3 keys, 4 count
        14 => query(d, s, a, st),
        // delete(t, db, store, lower, upper, lowerOpen, upperOpen), clear(t, db, store)
        15 | 16 => {
            let t = num::<E>(s, a, 1) as u32;
            let (db, store) = (text(s, 2)?, text(s, 3)?);
            let r = if op == 15 { range(d, s, a, 4)? } else { Range { lo: Bound::Unbounded, up: Bound::Unbounded } };
            let Some(so) = st.store(&db, &store) else { return Err(not_found(d, s)) };
            let keys: Vec<Key> = if empty(&r.lo, &r.up) { Vec::new() } else { so.rows.range::<[u8], _>((row_bound(&r.lo), row_bound(&r.up))).map(|(k, _)| k.clone()).collect() };
            st.rec(t, |w| match op {
                16 => {
                    w.op(log::CLEAR, &db, &store);
                }
                _ => keys.iter().for_each(|k| {
                    w.op(log::DELETE, &db, &store).bytes(k);
                }),
            });
            for k in keys {
                let old = st.set_row(&db, &store, &k, None);
                st.log(t, Undo::Row { db: db.clone(), store: store.clone(), key: k, old });
            }
            Ok(s.undefined())
        }
        // cursor(db, store, index|null, lower, upper, lowerOpen, upperOpen, dir, posKey, posPrimary, steps, keysOnly)
        //   -> null, or [key, primaryKey, value, posKey, posPrimary]
        17 => cursor(d, s, a, st),
        // values(db, store) -> [[primaryKey, value]] (to fill a new index)
        18 => {
            let (db, store) = (text(s, 1)?, text(s, 2)?);
            let Some(so) = st.store(&db, &store) else { return Err(not_found(d, s)) };
            let mut out = Vec::with_capacity(so.rows.len());
            for (k, row) in so.rows.iter_mut() {
                let pair = [key_value::<E>(s, k)?, read(d, s, &mut row.value)?];
                out.push(s.new_array(&pair)?);
            }
            s.new_array(&out)
        }
        // ixFill(t, db, store, index, [[primaryKey, [keys]]])
        19 => {
            let t = num::<E>(s, a, 1) as u32;
            let (db, store, ix) = (text(s, 2)?, text(s, 3)?, text(s, 4)?);
            let list = arg_s::<E>(s, a, 5);
            let n = s.array_len(list)?.unwrap_or(0);
            let mut adds = Vec::new();
            for i in 0..n {
                let k = s.number(f64::from(i));
                let pair = s.get(list, k)?;
                let (z, o) = (s.number(0.0), s.number(1.0));
                let pk = s.get(pair, z)?;
                let pk = key(d, s, pk)?;
                let ks = s.get(pair, o)?;
                for k in keys_of(d, s, ks)? {
                    adds.push((k, pk.clone()));
                }
            }
            let Some(so) = st.store(&db, &store) else { return Err(not_found(d, s)) };
            let Some(index) = so.indexes.get_mut(&ix) else { return Err(not_found(d, s)) };
            let id = index.id;
            for (k, pk) in &adds {
                if index.unique && index.entries.range::<Pair, _>(edge(k, false)..edge(k, true)).any(|(_, ep)| ep != pk) {
                    return Err(d.dom_err_s(s, "ConstraintError", "An index key already exists."));
                }
                index.entries.insert((k.clone(), pk.clone()));
            }
            st.rec(t, |w| {
                w.op(log::FILL, &db, &store).str(&ix).u32(adds.len() as u32);
                adds.iter().for_each(|(k, pk)| {
                    w.bytes(k).bytes(pk);
                });
            });
            let so = st.store(&db, &store).expect("checked");
            for (k, pk) in adds {
                if let Some(row) = so.rows.get_mut(&pk) {
                    row.ix.push((id, k));
                }
            }
            Ok(s.undefined())
        }
        // check(): throws `SecurityError` for an opaque origin (`shared` above)
        20 => Ok(s.undefined()),
        _ => Err(s.type_error("Illegal invocation")),
    }
}

/// The keys of the JS array `v`.
fn keys_of<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, v: Local<'s>) -> Res<'s, Vec<Key>> {
    let n = s.array_len(v)?.unwrap_or(0);
    let mut out = Vec::with_capacity(n as usize);
    for i in 0..n {
        let k = s.number(f64::from(i));
        let e = s.get(v, k)?;
        out.push(key(d, s, e)?);
    }
    Ok(out)
}

fn put<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, a: Args<'s>, st: &mut IdbStore) -> Res<'s, Local<'s>> {
    let t = num::<E>(s, a, 1) as u32;
    let (db, store) = (d.arg_str_s(s, a, 2)?, d.arg_str_s(s, a, 3)?);
    let kv = arg_s::<E>(s, a, 4);
    let k = key(d, s, kv)?;
    let v = arg_s::<E>(s, a, 5);
    let add_v = arg_s::<E>(s, a, 6);
    let add = s.as_bool(add_v).unwrap_or(false);
    let ixv = arg_s::<E>(s, a, 7);
    let n = s.array_len(ixv)?.unwrap_or(0);
    let mut ixk: Vec<(String, Vec<Key>)> = Vec::with_capacity(n as usize);
    for i in 0..n {
        let j = s.number(f64::from(i));
        let pair = s.get(ixv, j)?;
        let (z, o) = (s.number(0.0), s.number(1.0));
        let name = s.get(pair, z)?;
        let name = d.string_arg_s(s, name)?;
        let ks = s.get(pair, o)?;
        ixk.push((name, keys_of(d, s, ks)?));
    }
    let value = crate::browser_navigation_state::serialize_storage(d, s, v)?;
    if st.log.is_some() && !crate::profile_record::storable(&value) {
        return Err(d.dom_err_s(s, "DataCloneError", "The value cannot be saved to disk."));
    }
    let Some(so) = st.store(&db, &store) else { return Err(not_found(d, s)) };
    if add && so.rows.contains_key(&k) {
        return Err(d.dom_err_s(s, "ConstraintError", "Unable to add key to index. A record with this key already exists."));
    }
    let mut ix = Vec::new();
    for (name, keys) in ixk {
        let Some(index) = so.indexes.get(&name) else { continue };
        for ik in keys {
            if index.unique && index.entries.range::<Pair, _>(edge(&ik, false)..edge(&ik, true)).any(|(_, ep)| *ep != k) {
                return Err(d.dom_err_s(s, "ConstraintError", "An index key already exists."));
            }
            ix.push((index.id, ik));
        }
    }
    let old_size = so.rows.get(&k).map_or(0, |r| r.value.bytes().len());
    if st.bytes.saturating_sub(old_size) + value.bytes().len() > MAX_BYTES {
        return Err(d.dom_err_s(s, "QuotaExceededError", "The quota has been exceeded."));
    }
    let so = st.store(&db, &store).expect("checked");
    if so.auto && matches!(kv_number(&k), Some(n) if n >= so.next) {
        let next = so.next;
        so.next = (kv_number(&k).expect("a number").floor() + 1.0).min(9007199254740992.0);
        st.log(t, Undo::Next { db: db.clone(), store: store.clone(), next });
    }
    if let Some(l) = &mut st.log {
        // The index entries go by name: a replay gives new index ids.
        let so = &st.dbs[&db].stores[&store];
        l.rec(st.txs.contains_key(&t).then_some(t), |w| {
            w.op(log::PUT, &db, &store).bytes(&k);
            crate::profile_record::encode(&value, w.0);
            w.u32(ix.len() as u32);
            for (id, ik) in &ix {
                let name = so.indexes.iter().find(|(_, i)| i.id == *id).map_or("", |(n, _)| n);
                w.str(name).bytes(ik);
            }
        });
    }
    let old = st.set_row(&db, &store, &k, Some(Row { value, ix }));
    st.log(t, Undo::Row { db, store, key: k, old });
    Ok(s.undefined())
}

/// The number of a number key.
fn kv_number(k: &[u8]) -> Option<f64> {
    (k[0] == NUMBER).then(|| unsortable(&k[1..]))
}

fn index<'s, 'a, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, a: Args<'s>, i: usize, so: &'a Store) -> Res<'s, Option<&'a Index>> {
    let v = arg_s::<E>(s, a, i);
    if !matches!(s.type_of(v), Type::String) {
        return Ok(None);
    }
    let name = d.string_arg_s(s, v)?;
    match so.indexes.get(&name) {
        Some(ix) => Ok(Some(ix)),
        None => Err(d.dom_err_s(s, "NotFoundError", "The index was not found.")),
    }
}

/// The value of the row at `pk`: a new copy in this realm.
fn value<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, so: &mut Store, pk: &[u8]) -> Res<'s, Local<'s>> {
    let row = so.rows.get_mut(pk).expect("a row of the index");
    read(d, s, &mut row.value)
}

/// A new copy of a stored record. Reading a record takes its platform objects (a `Blob`, a `CryptoKey`) out of it, so a record
/// that has them is written again from the copy, before any script runs. A record without them is read as it is.
fn read<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, c: &mut Cloned) -> Res<'s, Local<'s>> {
    let objects = !c.moved().is_empty();
    let v = crate::browser_navigation_state::materialize(d, s, c)?;
    if objects {
        *c = crate::browser_navigation_state::serialize_storage(d, s, v)?;
    }
    Ok(v)
}

fn query<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, a: Args<'s>, st: &mut IdbStore) -> Res<'s, Local<'s>> {
    let (db, store) = (d.arg_str_s(s, a, 1)?, d.arg_str_s(s, a, 2)?);
    let r = range(d, s, a, 4)?;
    let what = num::<E>(s, a, 8) as u8;
    let max = match num::<E>(s, a, 9) {
        n if n >= 1.0 && what < 4 => n as usize,
        _ => usize::MAX,
    };
    let max = if what < 2 { 1 } else { max };
    let Some(so) = st.store(&db, &store) else { return Err(not_found(d, s)) };
    // The (key, primary key) of the hits, in order.
    let hits: Vec<Pair> = match index(d, s, a, 3, so)? {
        _ if empty(&r.lo, &r.up) => Vec::new(),
        None if what == 4 => return Ok(s.number(so.rows.range::<[u8], _>((row_bound(&r.lo), row_bound(&r.up))).count() as f64)),
        Some(ix) if what == 4 => return Ok(s.number(ix.entries.range::<Pair, _>((r.lo.as_ref(), r.up.as_ref())).count() as f64)),
        None => so.rows.range::<[u8], _>((row_bound(&r.lo), row_bound(&r.up))).take(max).map(|(k, _)| (k.clone(), k.clone())).collect(),
        Some(ix) => ix.entries.range::<Pair, _>((r.lo.as_ref(), r.up.as_ref())).take(max).cloned().collect(),
    };
    match what {
        4 => Ok(s.number(0.0)),
        0 => match hits.first() {
            None => Ok(s.undefined()),
            Some((_, pk)) => value(d, s, so, pk),
        },
        1 => match hits.first() {
            None => Ok(s.undefined()),
            Some((_, pk)) => key_value::<E>(s, pk),
        },
        _ => {
            let mut out = Vec::with_capacity(hits.len());
            for (_, pk) in &hits {
                out.push(if what == 2 { value(d, s, so, pk)? } else { key_value::<E>(s, pk)? });
            }
            s.new_array(&out)
        }
    }
}

fn cursor<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, a: Args<'s>, st: &mut IdbStore) -> Res<'s, Local<'s>> {
    let (db, store) = (d.arg_str_s(s, a, 1)?, d.arg_str_s(s, a, 2)?);
    let r = range(d, s, a, 4)?;
    let dir = num::<E>(s, a, 8) as u8; // 0 next, 1 nextunique, 2 prev, 3 prevunique
    let pos = |s: &mut Sc<'s, E>, i: usize| {
        let v = arg_s::<E>(s, a, i);
        s.bytes(v).map(Key::from)
    };
    let mut at = pos(s, 9).map(|k| (k, pos(s, 10).unwrap_or_default()));
    let steps = (num::<E>(s, a, 11) as usize).max(1);
    let keys_v = arg_s::<E>(s, a, 12);
    let keys_only = s.as_bool(keys_v).unwrap_or(false);
    let Some(so) = st.store(&db, &store) else { return Err(not_found(d, s)) };
    let ix = index(d, s, a, 3, so)?;
    for _ in 0..steps {
        let found = match (dir, at.take()) {
            (0 | 1, None) => first(so, ix, &r.lo, &r.up, false),
            (0, Some(p)) => first(so, ix, &Bound::Excluded(p), &r.up, false),
            (1, Some(p)) => first(so, ix, &Bound::Included(edge(&p.0, true)), &r.up, false),
            (_, p) => {
                let up = match p {
                    None => r.up.clone(),
                    Some(p) if dir == 2 => Bound::Excluded(p),
                    Some(p) => Bound::Excluded(edge(&p.0, false)),
                };
                // `prevunique` stops at the first entry of a key.
                first(so, ix, &r.lo, &up, true).and_then(|e| if dir == 3 { first(so, ix, &Bound::Included(edge(&e.0, false)), &r.up, false) } else { Some(e) })
            }
        };
        let Some(e) = found else { return Ok(s.null()) };
        at = Some(e);
    }
    let (k, p) = at.expect("one step");
    let v = if keys_only { s.undefined() } else { value(d, s, so, &p)? };
    let out = [key_value::<E>(s, &k)?, key_value::<E>(s, &p)?, v, s.new_uint8_array(k.to_vec())?, s.new_uint8_array(p.to_vec())?];
    s.new_array(&out)
}
