//! The IndexedDB log of a profile (`docs/internal/design-persistent-profile.md`, option A, step 3). The data stays in memory
//! (`IdbStore`); the log is a copy of it on disk, so a new process opens the data of the last one.
//!
//! ```text
//! <profile>/idb/<FNV-1a 64 of the origin, hex>.log
//! file    = 8 bytes magic, str origin, frame*
//! frame   = u32 payload length, u32 CRC-32 of the payload, op*
//! op      = 1 drop database: str db
//!         | 2 version: str db, f64 version (makes the database when it is new)
//!         | 3 make store: str db, str store, kp, u8 autoIncrement
//!         | 4 drop store: str db, str store
//!         | 5 make index: str db, str store, str index, kp, u8 unique, u8 multiEntry
//!         | 6 drop index: str db, str store, str index
//!         | 7 put: str db, str store, key, record, u32 n, (str index, key)*
//!         | 8 delete: str db, str store, key
//!         | 9 clear: str db, str store
//!         | 10 fill index: str db, str store, str index, u32 n, (key, primary key)*
//!         | 11 next key: str db, str store, f64
//! kp      = u8 0 none | 1, str | 2, u32 n, str*
//! str/key = u32 length, bytes
//! record  = `profile_record`
//! ```
//!
//! Numbers are little endian. The ops are the host ops of `idb(op, ...)`, so a replay runs the same steps on the store. Index ids
//! are not written: a replay gives new ones, and a put names its indexes.
//!
//! A write transaction collects its ops in a buffer of its own, from the call of each op. The commit moves the buffer into the
//! pending frames; an abort drops it. A read-only transaction has no buffer. The ops are not read from the store at the commit:
//! there are no locks between transactions yet (design-indexeddb-store step 4), so the store can hold a write of another live
//! transaction.
//!
//! The pending frames go to the file with the profile writes (`Saver`): at most once a second, and at close. `fsync` runs at close
//! and after a snapshot only. A replay at the first open of the origin in the process stops at the first bad frame (a torn write
//! of the last commit) and cuts the file there. When the log has more than twice the bytes of the live data (and at least
//! `FLOOR`), and no write transaction is live, a snapshot (the ops that make the store) goes to a temporary file, `fsync`, and a
//! rename over the log. The files hold live credentials: mode 0600 in a 0700 directory, and no message holds a value.

use super::{Db, IdbStore, Index, Key, Kp, Row, Store};
use crate::profile::Profile;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs::File;
use std::io::{self, Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};

const MAGIC: &[u8; 8] = b"bareidb1";
/// The smallest log that becomes a snapshot. A smaller one costs less to read than a snapshot costs to write.
const FLOOR: u64 = 64 << 10;
/// The payload of a snapshot frame grows to about this size before the frame is written.
const CHUNK: usize = 64 << 10;

pub(super) const DROP_DB: u8 = 1;
pub(super) const VERSION: u8 = 2;
pub(super) const MAKE_STORE: u8 = 3;
pub(super) const DROP_STORE: u8 = 4;
pub(super) const MAKE_INDEX: u8 = 5;
pub(super) const DROP_INDEX: u8 = 6;
pub(super) const PUT: u8 = 7;
pub(super) const DELETE: u8 = 8;
pub(super) const CLEAR: u8 = 9;
pub(super) const FILL: u8 = 10;
const NEXT: u8 = 11;

/// The IndexedDB part of a profile, shared by the logs of its origins and by the `Saver`. The `Saver` holds the lock of the profile;
/// when it ends, it flushes the logs and closes the disk, so no log writes after the lock is free.
pub(crate) struct Disk {
    dir: PathBuf,
    /// Moves at each commit with ops. The `Saver` flushes the logs when it moved.
    pub(crate) rev: AtomicU64,
    pub(crate) closed: AtomicBool,
}

impl Disk {
    pub(crate) fn new(profile: &Profile) -> Arc<Disk> {
        Arc::new(Disk { dir: profile.dir().join("idb"), rev: AtomicU64::new(0), closed: AtomicBool::new(false) })
    }
}

/// The log of one origin.
pub(super) struct Log {
    disk: Arc<Disk>,
    origin: String,
    path: PathBuf,
    /// The ops of each live write transaction.
    txs: HashMap<u32, Vec<u8>>,
    /// Frames that are not in the file yet.
    pending: Vec<u8>,
    /// The bytes in the file.
    len: u64,
    file: Option<File>,
    /// Bytes were written since the last `fsync`.
    unsynced: bool,
    /// The log length at which the live data is measured again (a snapshot check).
    check: u64,
    /// A write failed: the message went to stderr once, and the log writes nothing more.
    failed: bool,
}

/// The ops of one change, written into a buffer.
pub(super) struct W<'a>(pub(super) &'a mut Vec<u8>);

impl W<'_> {
    pub(super) fn op(&mut self, op: u8, db: &str, store: &str) -> &mut Self {
        self.0.push(op);
        self.str(db);
        if op != DROP_DB && op != VERSION {
            self.str(store);
        }
        self
    }

    pub(super) fn str(&mut self, s: &str) -> &mut Self {
        self.bytes(s.as_bytes())
    }

    pub(super) fn bytes(&mut self, b: &[u8]) -> &mut Self {
        self.u32(b.len() as u32);
        self.0.extend_from_slice(b);
        self
    }

    pub(super) fn u32(&mut self, n: u32) -> &mut Self {
        self.0.extend(n.to_le_bytes());
        self
    }

    pub(super) fn u8(&mut self, n: u8) -> &mut Self {
        self.0.push(n);
        self
    }

    pub(super) fn f64(&mut self, n: f64) -> &mut Self {
        self.0.extend(n.to_le_bytes());
        self
    }

    pub(super) fn kp(&mut self, kp: &Kp) -> &mut Self {
        match kp {
            Kp::None => self.u8(0),
            Kp::One(p) => self.u8(1).str(p),
            Kp::Many(ps) => {
                self.u8(2).u32(ps.len() as u32);
                ps.iter().for_each(|p| {
                    self.str(p);
                });
                self
            }
        }
    }
}

impl Log {
    /// Add the ops that `f` writes to transaction `t` (`None`: a change outside a transaction, a frame of its own).
    pub(super) fn rec(&mut self, t: Option<u32>, f: impl FnOnce(&mut W)) {
        match t {
            Some(t) => f(&mut W(self.txs.entry(t).or_default())),
            None => {
                let mut b = Vec::new();
                f(&mut W(&mut b));
                self.frame(&b);
            }
        }
    }

    /// The end of transaction `t`: its ops become a frame (a commit), or go (an abort).
    pub(super) fn end(&mut self, t: u32, commit: bool) {
        if let Some(b) = self.txs.remove(&t).filter(|b| commit && !b.is_empty()) {
            self.frame(&b);
        }
    }

    fn frame(&mut self, payload: &[u8]) {
        frame(&mut self.pending, payload);
        self.disk.rev.fetch_add(1, Relaxed);
    }

    /// Write the pending frames, or a snapshot when the log is long and no write transaction is live (`dbs`, `busy`). `sync`:
    /// `fsync` the file too (a close).
    pub(super) fn flush(&mut self, dbs: &HashMap<String, Db>, busy: bool, sync: bool) {
        if self.failed || self.disk.closed.load(Relaxed) {
            return;
        }
        let end = self.len + self.pending.len() as u64;
        let mut done = Ok(());
        if end > self.check && !busy {
            let live = live(dbs);
            self.check = 2 * live.max(FLOOR);
            if end > self.check {
                done = self.snapshot(dbs);
            }
        }
        if done.is_ok() && !self.pending.is_empty() {
            done = self.append();
        }
        if done.is_ok() && sync && self.unsynced {
            done = self.file.as_ref().map_or(Ok(()), |f| f.sync_data());
            self.unsynced = done.is_err();
        }
        if let Err(e) = done {
            self.failed = true;
            eprintln!("bare: {}: cannot write the IndexedDB log ({}); the changes of this origin stay in memory", self.path.display(), e.kind());
        }
    }

    fn append(&mut self) -> io::Result<()> {
        let mut f = match self.file.take() {
            Some(f) => f,
            None => {
                bare_os::fs::create_private_dir(self.path.parent().expect("a directory"))?;
                bare_os::fs::open_private(&self.path)?
            }
        };
        if self.len == 0 {
            let mut head = MAGIC.to_vec();
            W(&mut head).str(&self.origin);
            self.pending.splice(0..0, head);
        }
        f.seek(SeekFrom::Start(self.len))?;
        f.write_all(&self.pending)?;
        self.len += self.pending.len() as u64;
        self.pending.clear();
        self.unsynced = true;
        self.file = Some(f);
        Ok(())
    }

    /// Write the ops that make `dbs` to a temporary file, `fsync`, and rename it over the log. The pending frames are in `dbs` already.
    fn snapshot(&mut self, dbs: &HashMap<String, Db>) -> io::Result<()> {
        let tmp = self.path.with_extension("tmp");
        bare_os::fs::create_private_dir(self.path.parent().expect("a directory"))?;
        let _ = std::fs::remove_file(&tmp);
        let mut out = io::BufWriter::new(bare_os::fs::create_private(&tmp)?);
        let mut head = MAGIC.to_vec();
        W(&mut head).str(&self.origin);
        out.write_all(&head)?;
        let mut len = head.len() as u64;
        let (mut b, mut fr) = (Vec::new(), Vec::new());
        let mut emit = |b: &mut Vec<u8>, out: &mut io::BufWriter<File>, last: bool| -> io::Result<()> {
            if b.len() >= CHUNK || (last && !b.is_empty()) {
                fr.clear();
                frame(&mut fr, b);
                out.write_all(&fr)?;
                len += fr.len() as u64;
                b.clear();
            }
            Ok(())
        };
        for (name, db) in dbs {
            W(&mut b).op(VERSION, name, "").f64(db.version);
            for (sn, so) in &db.stores {
                W(&mut b).op(MAKE_STORE, name, sn).kp(&so.kp).u8(so.auto as u8);
                W(&mut b).op(NEXT, name, sn).f64(so.next);
                let names: HashMap<u32, &str> = so.indexes.iter().map(|(n, ix)| (ix.id, n.as_str())).collect();
                for (iname, ix) in &so.indexes {
                    W(&mut b).op(MAKE_INDEX, name, sn).str(iname).kp(&ix.kp).u8(ix.unique as u8).u8(ix.multi as u8);
                }
                for (k, row) in &so.rows {
                    put(&mut b, name, sn, k, row, &names);
                    emit(&mut b, &mut out, false)?;
                }
            }
        }
        emit(&mut b, &mut out, true)?;
        let f = out.into_inner().map_err(|e| e.into_error())?;
        f.sync_all()?;
        drop(f);
        std::fs::rename(&tmp, &self.path)?;
        // The pending frames are in the snapshot, and the old file handle is of the old file.
        (self.len, self.file, self.unsynced) = (len, None, false);
        self.pending.clear();
        self.check = 2 * len.max(FLOOR);
        Ok(())
    }
}

/// A put of `row` in the snapshot. Each index entry names its index; an entry of a deleted index is left out.
fn put(b: &mut Vec<u8>, db: &str, store: &str, k: &[u8], row: &Row, names: &HashMap<u32, &str>) {
    let mut w = W(b);
    w.op(PUT, db, store).bytes(k);
    let ok = crate::profile_record::encode(&row.value, w.0);
    debug_assert!(ok, "a put checks that the record has a disk form");
    let ix: Vec<(&str, &Key)> = row.ix.iter().filter_map(|(id, k)| Some((*names.get(id)?, k))).collect();
    w.u32(ix.len() as u32);
    for (n, k) in ix {
        w.str(n).bytes(k);
    }
}

fn frame(out: &mut Vec<u8>, payload: &[u8]) {
    out.extend((payload.len() as u32).to_le_bytes());
    out.extend(crc32fast::hash(payload).to_le_bytes());
    out.extend_from_slice(payload);
}

/// About the bytes of a snapshot of `dbs`: keys, records, index keys. It walks the rows; it copies nothing.
fn live(dbs: &HashMap<String, Db>) -> u64 {
    let mut n = 0;
    for (dn, st) in dbs.iter().flat_map(|(dn, d)| d.stores.iter().map(move |(sn, st)| (dn.len() + sn.len(), st))) {
        for (k, r) in &st.rows {
            n += 33 + dn + k.len() + r.value.bytes().len() + r.ix.iter().map(|(_, k)| 8 + k.len()).sum::<usize>();
            for m in r.value.moved() {
                if let bare_js_api::clone::Moved::Data(d) = m {
                    n += match d.downcast_ref::<crate::browser_web::WebClone>() {
                        Some(crate::browser_web::WebClone::Blob { bytes, .. }) => bytes.len(),
                        _ => 64,
                    };
                }
            }
        }
    }
    n as u64
}

/// The file name of `origin`: FNV-1a 64. The file starts with the origin, so a collision is found, not read.
fn file_name(origin: &str) -> String {
    let h = origin.bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ u64::from(b)).wrapping_mul(0x100_0000_01b3));
    format!("{h:016x}.log")
}

/// The store of `origin` from its log in the profile of `disk`. A file that cannot be read or is not of this origin gives an empty
/// store in memory only, and a message.
pub(crate) fn load(disk: &Arc<Disk>, origin: &str) -> IdbStore {
    let path = disk.dir.join(file_name(origin));
    let mut st = IdbStore::default();
    let mut head = MAGIC.to_vec();
    W(&mut head).str(origin);
    let b = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) if e.kind() == io::ErrorKind::NotFound => Vec::new(),
        Err(e) => {
            eprintln!("bare: {}: cannot read the IndexedDB log ({}); this origin is in memory", path.display(), e.kind());
            return st;
        }
    };
    // A file shorter than its head is a torn first write: it starts again.
    let mut at = if b.len() < head.len() && head.starts_with(&b) { 0 } else { head.len() };
    if at > 0 && !b.starts_with(&head) {
        eprintln!("bare: {}: not the IndexedDB log of this origin; this origin is in memory", path.display());
        return st;
    }
    while at > 0 {
        let Some(h) = b.get(at..at + 8) else { break };
        let n = u32::from_le_bytes(h[..4].try_into().expect("4 bytes")) as usize;
        let Some(p) = b.get(at + 8..at + 8 + n) else { break };
        if crc32fast::hash(p).to_le_bytes() != h[4..] || apply(&mut st, p).is_none() {
            break;
        }
        at += 8 + n;
    }
    let mut log = Log { disk: disk.clone(), origin: origin.into(), path, txs: HashMap::new(), pending: Vec::new(), len: at as u64, file: None, unsynced: false, check: 0, failed: false };
    if at < b.len() {
        // A torn or damaged tail: cut it, so the next frame follows the last good one.
        let cut = bare_os::fs::open_private(&log.path).and_then(|f| f.set_len(at as u64).map(|_| f));
        match cut {
            Ok(f) => log.file = Some(f),
            Err(e) => {
                eprintln!("bare: {}: cannot cut the damaged end of the IndexedDB log ({}); this origin is in memory", log.path.display(), e.kind());
                return st;
            }
        }
    }
    log.check = 2 * live(&st.dbs).max(FLOOR);
    st.log = Some(Box::new(log));
    st
}

struct Reader<'b>(&'b [u8]);

impl<'b> Reader<'b> {
    fn take(&mut self, n: usize) -> Option<&'b [u8]> {
        let (a, b) = self.0.split_at_checked(n)?;
        self.0 = b;
        Some(a)
    }

    fn u8(&mut self) -> Option<u8> {
        Some(self.take(1)?[0])
    }

    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }

    fn f64(&mut self) -> Option<f64> {
        Some(f64::from_le_bytes(self.take(8)?.try_into().ok()?))
    }

    fn bytes(&mut self) -> Option<&'b [u8]> {
        let n = self.u32()? as usize;
        self.take(n)
    }

    fn key(&mut self) -> Option<Key> {
        self.bytes().map(Key::from)
    }

    fn str(&mut self) -> Option<String> {
        String::from_utf8(self.bytes()?.to_vec()).ok()
    }

    fn kp(&mut self) -> Option<Kp> {
        Some(match self.u8()? {
            0 => Kp::None,
            1 => Kp::One(self.str()?),
            2 => Kp::Many((0..self.u32()?).map(|_| self.str()).collect::<Option<_>>()?),
            _ => return None,
        })
    }
}

/// Run the ops of one frame on `st`. `None`: the frame is damaged (the ops before the damage are done).
fn apply(st: &mut IdbStore, p: &[u8]) -> Option<()> {
    let mut r = Reader(p);
    while !r.0.is_empty() {
        let op = r.u8()?;
        let db = r.str()?;
        if op == DROP_DB {
            st.drop_db(&db);
            continue;
        }
        if op == VERSION {
            let v = r.f64()?;
            st.dbs.entry(db).or_insert_with(|| Db { version: 0.0, stores: BTreeMap::new() }).version = v;
            continue;
        }
        let store = r.str()?;
        match op {
            MAKE_STORE => {
                let (kp, auto) = (r.kp()?, r.u8()? != 0);
                let so = Store { kp, auto, next: 1.0, rows: BTreeMap::new(), indexes: BTreeMap::new(), next_ix: 0 };
                st.drop_store(&db, &store);
                st.dbs.get_mut(&db)?.stores.insert(store, so);
            }
            DROP_STORE => {
                st.drop_store(&db, &store);
            }
            MAKE_INDEX => {
                let (name, kp, unique, multi) = (r.str()?, r.kp()?, r.u8()? != 0, r.u8()? != 0);
                let so = st.store(&db, &store)?;
                let id = so.next_ix;
                so.next_ix += 1;
                so.indexes.insert(name, Index { id, kp, unique, multi, entries: BTreeSet::new() });
            }
            DROP_INDEX => {
                let name = r.str()?;
                st.store(&db, &store)?.indexes.remove(&name);
            }
            PUT => {
                let k = r.key()?;
                let (value, rest) = crate::profile_record::decode(r.0)?;
                r.0 = rest;
                let so = st.store(&db, &store)?;
                let mut ix = Vec::new();
                for _ in 0..r.u32()? {
                    let (name, ik) = (r.str()?, r.key()?);
                    if let Some(i) = so.indexes.get(&name) {
                        ix.push((i.id, ik));
                    }
                }
                st.put_row(&db, &store, k, Row { value, ix });
            }
            DELETE => {
                let k = r.key()?;
                st.set_row(&db, &store, &k, None);
            }
            CLEAR => {
                let keys: Vec<Key> = st.store(&db, &store)?.rows.keys().cloned().collect();
                for k in keys {
                    st.set_row(&db, &store, &k, None);
                }
            }
            FILL => {
                let name = r.str()?;
                let so = st.store(&db, &store)?;
                let index = so.indexes.get_mut(&name)?;
                for _ in 0..r.u32()? {
                    let (k, pk) = (r.key()?, r.key()?);
                    index.entries.insert((k.clone(), pk.clone()));
                    if let Some(row) = so.rows.get_mut(&pk) {
                        row.ix.push((index.id, k));
                    }
                }
            }
            NEXT => st.store(&db, &store)?.next = r.f64()?,
            _ => return None,
        }
    }
    Some(())
}
