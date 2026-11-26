//! `localStorage` and `sessionStorage`: an in-memory store for each origin, and the JS `Storage` bindings.
//!
//! The data lives in a `WebStorage`, not in the browser. A test (or the future loader) can keep one `WebStorage` and
//! attach it to a new `Browser` (`Browser::attach_storage`), so a "reload" keeps the same session. The store is only
//! memory. It never writes a file. A profile (`profile.rs`) saves the cookies and `localStorage` to disk; it reads `revisions`
//! to see a change.
//!
//! Keys and values are UTF-16 (`Box<[u16]>`, lone surrogates included). Items keep insertion order, which is the order of
//! `key(n)`. The origin is `scheme://host[:port]` of the document URL. Two origins never see each other's items.
//!
//! Caps (`StorageLimits`), all checked before anything grows:
//! - one origin may hold `max_origin_units` UTF-16 units (keys plus values): `QuotaExceededError`, which a script can
//!   catch, as in a browser. The write is not done;
//! - `max_origins`, `max_entries` per origin and `max_total_units` over all origins: `Limit` (a sandbox bound, not a
//!   browser behavior).
//!
//! Named properties (WebIDL `getter`, `setter`, `deleter` of `Storage`; checked against Chrome 155 in
//! `tools/fixtures/ui/storage-cases.txt`): `localStorage.token` is `getItem('token')`, `localStorage.token = v` is `setItem`, `delete`
//! is `removeItem`, `Object.keys`, `in`, `for in`, `JSON.stringify` and `Object.getOwnPropertyDescriptor` see the items (a writable,
//! enumerable, configurable data property). The two objects are host named objects (`Heap::new_named_open`, `heap_named.rs`), and the
//! heap asks `Dom` (`NamedSource`) on every read, so nothing is cached. Each object is made on the first read of `window.localStorage`
//! or `window.sessionStorage` (`area_get`): a named object makes `Heap::has_exotics` true, and that costs every property write. A name does not hide the prototype chain (there is no
//! `[LegacyOverrideBuiltIns]`): `getItem`, `length` and the like stay the methods, and a write to such a name makes an ordinary
//! property on the object, as in Chrome. `Object.defineProperty` with a data descriptor is a `setItem` (the flags are ignored); an
//! accessor descriptor is a `TypeError`. `preventExtensions`, `seal` and `freeze` are refused.
//!
//! Gates: `key(n)` and the order of the names are the insertion order; Chrome uses the order of a hash table, which the Standard leaves
//! open. `Object.keys` and `for in` do not list a name that the prototype chain hides (Chrome lists it, except an accessor such as
//! `length`). There is no `storage` event, and no sharing of a live area between two `Browser`s except through a shared `WebStorage`.
//! Without a document URL a read finds nothing and a write is `Unsupported`.

use crate::eng::{Eng, Fault, Ops};
use crate::browser::*;
use crate::browser_scoped::*;
use crate::eng::NamedOp;
use bare_js_api::Pending;
use bare_js_api::{Args, Engine as _, Local, Str, Type};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

#[derive(Clone, Copy, Debug)]
pub struct StorageLimits {
    pub max_origins: usize,
    pub max_entries: usize,
    pub max_origin_units: usize,
    pub max_total_units: usize,
}

impl Default for StorageLimits {
    fn default() -> Self {
        // 5M UTF-16 units is about the 10 MB of a browser quota.
        StorageLimits { max_origins: 64, max_entries: 1024, max_origin_units: 5 << 20, max_total_units: 16 << 20 }
    }
}

/// Key and value, UTF-16.
type Item = (Box<[u16]>, Box<[u16]>);

#[derive(Default)]
struct Area {
    items: Vec<Item>,
    units: usize,
}

#[derive(Default)]
struct Areas {
    by_origin: HashMap<String, Area>,
    total: usize,
    /// Moves at each change of an item (`WebStorage::revisions`).
    rev: u64,
}

/// Why a write failed.
#[derive(Debug, PartialEq, Eq)]
pub enum StoreError {
    /// The origin is over its quota (`QuotaExceededError` in JS).
    Quota,
    /// A sandbox bound (origins, entries or total size).
    Limit(&'static str),
}

/// The items of one origin as text: `(origin, [(key, value)])`.
pub type OriginItems = Vec<(String, Vec<(String, String)>)>;

/// A copy of a whole session as plain text: cookies, `localStorage` and `sessionStorage`. It holds live credentials.
/// The text is lossy: a lone UTF-16 surrogate becomes U+FFFD.
#[derive(Clone, Debug, Default)]
pub struct StorageState {
    pub cookies: Vec<bare_net::cookie::Cookie>,
    pub local: OriginItems,
    pub session: OriginItems,
    /// The permission decisions: `(origin, permission name, decision)`, sorted.
    pub permissions: Vec<(String, String, Permission)>,
}

/// The `localStorage` and `sessionStorage` data of all origins. Clones share the data.
#[derive(Clone)]
pub struct WebStorage {
    local: Rc<RefCell<Areas>>,
    session: Rc<RefCell<Areas>>,
    /// The cookie jar: the one of the network client (`Client::jar`) and of `document.cookie` (`browser_cookie.rs`). A new tab shares it, as it
    /// shares `localStorage`. It is in memory only.
    cookies: Arc<Mutex<bare_net::cookie::Jar>>,
    /// The `indexedDB` data of each origin (`browser_idb.rs`). A new tab shares it, as it shares `localStorage`.
    idb: Arc<Mutex<Idbs>>,
    /// The permission decisions of each origin (`browser_notification.rs`). A new tab shares them.
    perms: Rc<RefCell<Perms>>,
    lim: StorageLimits,
}

/// A decision of the user for one permission of one origin. No decision is `None`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Permission {
    Granted,
    Denied,
}

/// The permission decisions: `(origin, permission name, decision)`. The list is short: a linear search is enough.
#[derive(Default)]
struct Perms {
    list: Vec<(String, String, Permission)>,
    /// Moves at each change (`WebStorage::revisions`).
    rev: u64,
}

/// The `indexedDB` stores of the origins that opened it, and the profile that saves them (`browser_idb_log.rs`).
#[derive(Default)]
struct Idbs {
    stores: HashMap<String, crate::browser_idb::Shared>,
    disk: Option<Arc<crate::browser_idb::log::Disk>>,
}

/// The `indexedDB` stores of a session, sent to a worker thread (`WebStorage::idb_share`).
pub(crate) struct IdbShare {
    idb: Arc<Mutex<Idbs>>,
    lim: StorageLimits,
}

impl IdbShare {
    /// The storage of a worker thread: the `indexedDB` stores of the session, and its own empty `localStorage` and cookie jar.
    pub(crate) fn storage(self) -> WebStorage {
        WebStorage { idb: self.idb, ..WebStorage::new(self.lim) }
    }
}

impl Default for WebStorage {
    fn default() -> Self {
        WebStorage::new(StorageLimits::default())
    }
}

impl WebStorage {
    pub fn new(lim: StorageLimits) -> WebStorage {
        WebStorage { local: Default::default(), session: Default::default(), cookies: Default::default(), idb: Default::default(), perms: Default::default(), lim }
    }

    /// The same `localStorage` data with a new, empty `sessionStorage` (a new tab).
    pub fn new_session(&self) -> WebStorage {
        WebStorage { local: self.local.clone(), session: Default::default(), cookies: self.cookies.clone(), idb: self.idb.clone(), perms: self.perms.clone(), lim: self.lim }
    }

    /// The `indexedDB` stores of this session, for a worker on another thread (`WebStorage` is not `Send`).
    pub(crate) fn idb_share(&self) -> IdbShare {
        IdbShare { idb: self.idb.clone(), lim: self.lim }
    }

    /// The `indexedDB` store of `origin`, made when it is new.
    /// With a profile, the first open of an origin in the process reads its log.
    pub(crate) fn idb(&self, origin: &str) -> crate::browser_idb::Shared {
        let mut g = self.idb.lock().unwrap_or_else(|e| e.into_inner());
        let Idbs { stores, disk } = &mut *g;
        stores
            .entry(origin.into())
            .or_insert_with(|| match disk {
                Some(disk) if !origin.is_empty() => Arc::new(Mutex::new(crate::browser_idb::log::load(disk, origin))),
                _ => Default::default(),
            })
            .clone()
    }

    /// Save the `indexedDB` stores of the origins opened from now on to `disk`.
    pub(crate) fn use_idb_disk(&self, disk: Arc<crate::browser_idb::log::Disk>) {
        self.idb.lock().unwrap_or_else(|e| e.into_inner()).disk = Some(disk);
    }

    /// Write the `indexedDB` logs (a profile only). `sync`: `fsync` too.
    pub(crate) fn flush_idb(&self, sync: bool) {
        let list: Vec<_> = self.idb.lock().unwrap_or_else(|e| e.into_inner()).stores.values().cloned().collect();
        for st in list {
            st.lock().unwrap_or_else(|e| e.into_inner()).flush(sync);
        }
    }

    /// The decision for permission `name` of `origin`.
    pub fn permission(&self, origin: &str, name: &str) -> Option<Permission> {
        self.perms.borrow().list.iter().find(|(o, n, _)| o == origin && n == name).map(|e| e.2)
    }

    /// Set (or with `None`, remove) the decision for permission `name` of `origin`. Gives `true` when it changed.
    pub fn set_permission(&self, origin: &str, name: &str, to: Option<Permission>) -> bool {
        let mut p = self.perms.borrow_mut();
        let at = p.list.iter().position(|(o, n, _)| o == origin && n == name);
        match (at, to) {
            (Some(i), Some(v)) if p.list[i].2 == v => return false,
            (Some(i), Some(v)) => p.list[i].2 = v,
            (Some(i), None) => drop(p.list.remove(i)),
            (None, Some(v)) => p.list.push((origin.into(), name.into(), v)),
            (None, None) => return false,
        }
        p.rev += 1;
        true
    }

    pub fn jar(&self) -> Arc<Mutex<bare_net::cookie::Jar>> {
        self.cookies.clone()
    }

    /// Use the jar of the network client from now on. Only this value changes: a clone made before keeps the old jar.
    pub fn use_jar(&mut self, jar: Arc<Mutex<bare_net::cookie::Jar>>) {
        self.cookies = jar;
    }

    fn areas(&self, session: bool) -> &RefCell<Areas> {
        if session { &self.session } else { &self.local }
    }

    pub(crate) fn get(&self, session: bool, origin: &str, key: &[u16]) -> Option<Box<[u16]>> {
        let a = self.areas(session).borrow();
        a.by_origin.get(origin)?.items.iter().find(|(k, _)| **k == *key).map(|(_, v)| v.clone())
    }

    pub(crate) fn len(&self, session: bool, origin: &str) -> usize {
        self.areas(session).borrow().by_origin.get(origin).map_or(0, |a| a.items.len())
    }

    pub(crate) fn keys(&self, session: bool, origin: &str) -> Vec<Box<[u16]>> {
        self.areas(session).borrow().by_origin.get(origin).map_or_else(Vec::new, |a| a.items.iter().map(|(k, _)| k.clone()).collect())
    }

    pub(crate) fn key_at(&self, session: bool, origin: &str, i: usize) -> Option<Box<[u16]>> {
        self.areas(session).borrow().by_origin.get(origin)?.items.get(i).map(|(k, _)| k.clone())
    }

    pub(crate) fn set(&self, session: bool, origin: &str, key: &[u16], value: &[u16]) -> Result<(), StoreError> {
        let mut areas = self.areas(session).borrow_mut();
        let areas = &mut *areas;
        let lim = self.lim;
        let (pos, old_units, area_units, n) = match areas.by_origin.get(origin) {
            Some(a) => {
                let pos = a.items.iter().position(|(k, _)| **k == *key);
                (pos, pos.map_or(0, |p| a.items[p].0.len() + a.items[p].1.len()), a.units, a.items.len())
            }
            None => {
                if areas.by_origin.len() >= lim.max_origins {
                    return Err(StoreError::Limit("storage-origins"));
                }
                (None, 0, 0, 0)
            }
        };
        if let Some(p) = pos {
            if *areas.by_origin[origin].items[p].1 == *value {
                return Ok(());
            }
        } else if n >= lim.max_entries {
            return Err(StoreError::Limit("storage-entries"));
        }
        let new_units = key.len() + value.len();
        if area_units - old_units + new_units > lim.max_origin_units {
            return Err(StoreError::Quota);
        }
        if areas.total - old_units + new_units > lim.max_total_units {
            return Err(StoreError::Limit("storage-total"));
        }
        let a = areas.by_origin.entry(origin.to_string()).or_default();
        match pos {
            Some(p) => a.items[p].1 = value.into(),
            None => a.items.push((key.into(), value.into())),
        }
        a.units = a.units - old_units + new_units;
        areas.total = areas.total - old_units + new_units;
        areas.rev += 1;
        Ok(())
    }

    pub(crate) fn remove(&self, session: bool, origin: &str, key: &[u16]) {
        let mut areas = self.areas(session).borrow_mut();
        let areas = &mut *areas;
        let Some(a) = areas.by_origin.get_mut(origin) else { return };
        let Some(p) = a.items.iter().position(|(k, _)| **k == *key) else { return };
        let (k, v) = a.items.remove(p);
        a.units -= k.len() + v.len();
        areas.total -= k.len() + v.len();
        areas.rev += 1;
        if a.items.is_empty() {
            areas.by_origin.remove(origin);
        }
    }

    pub(crate) fn clear(&self, session: bool, origin: &str) {
        let mut areas = self.areas(session).borrow_mut();
        if let Some(a) = areas.by_origin.remove(origin) {
            areas.total -= a.units;
            areas.rev += 1;
        }
    }

    // ----- for tests and drivers -----

    /// A copy of the cookies (an expired one is skipped) and of both storage areas. Origins are sorted.
    pub fn export(&self) -> StorageState {
        StorageState { cookies: self.export_cookies(), local: self.export_area(false), session: self.export_area(true), permissions: self.export_permissions() }
    }

    /// The cookies of `export`.
    pub(crate) fn export_cookies(&self) -> Vec<bare_net::cookie::Cookie> {
        let jar = self.cookies.lock().unwrap_or_else(|e| e.into_inner());
        let now = jar.now();
        jar.list().into_iter().filter(|c| c.expires.is_none_or(|e| e > now)).cloned().collect()
    }

    /// One storage area of `export`.
    pub(crate) fn export_area(&self, session: bool) -> OriginItems {
        let a = self.areas(session).borrow();
        let mut out: OriginItems = a
            .by_origin
            .iter()
            .map(|(o, a)| (o.clone(), a.items.iter().map(|(k, v)| (String::from_utf16_lossy(k), String::from_utf16_lossy(v))).collect()))
            .collect();
        out.sort_by(|x, y| x.0.cmp(&y.0));
        out
    }

    /// The permission decisions of `export`, sorted.
    pub(crate) fn export_permissions(&self) -> Vec<(String, String, Permission)> {
        let mut out = self.perms.borrow().list.clone();
        out.sort_by(|x, y| (&x.0, &x.1).cmp(&(&y.0, &y.1)));
        out
    }

    /// The revisions of the cookie jar, of `localStorage` and of the permissions: each moves at a change. A profile saves a part
    /// when its number moved.
    pub(crate) fn revisions(&self) -> (u64, u64, u64) {
        (self.cookies.lock().unwrap_or_else(|e| e.into_inner()).revision(), self.local.borrow().rev, self.perms.borrow().rev)
    }

    /// Add a saved state to this storage. Items go through `set`, so the limits hold; the first error stops the import (what was added stays).
    pub fn import(&self, st: &StorageState) -> Result<(), StoreError> {
        {
            let mut jar = self.cookies.lock().unwrap_or_else(|e| e.into_inner());
            let now = jar.now();
            for c in &st.cookies {
                jar.insert(c.clone(), now);
            }
        }
        for (origin, name, v) in &st.permissions {
            self.set_permission(origin, name, Some(*v));
        }
        for (session, list) in [(false, &st.local), (true, &st.session)] {
            for (origin, items) in list {
                for (k, v) in items {
                    self.set_item(session, origin, k, v)?;
                }
            }
        }
        Ok(())
    }

    /// `getItem` as text (lone surrogates become U+FFFD).
    pub fn get_item(&self, session: bool, origin: &str, key: &str) -> Option<String> {
        let k: Vec<u16> = key.encode_utf16().collect();
        self.get(session, origin, &k).map(|v| String::from_utf16_lossy(&v))
    }

    pub fn set_item(&self, session: bool, origin: &str, key: &str, value: &str) -> Result<(), StoreError> {
        let (k, v): (Vec<u16>, Vec<u16>) = (key.encode_utf16().collect(), value.encode_utf16().collect());
        self.set(session, origin, &k, &v)
    }

    pub fn item_count(&self, session: bool, origin: &str) -> usize {
        self.len(session, origin)
    }

    /// Origins that hold at least one item, and the UTF-16 units kept over all of them.
    pub fn origin_count(&self, session: bool) -> usize {
        self.areas(session).borrow().by_origin.len()
    }

    pub fn total_units(&self, session: bool) -> usize {
        self.areas(session).borrow().total
    }
}

// ------------------------------------------------------------------------------------------ JS bindings

/// Which area `this` is (`true`: `sessionStorage`). A method of another receiver throws "Illegal invocation".
#[inline(always)]
fn area<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>) -> Res<'s, bool> {
    match obj_id::<E>(s, this).and_then(|o| d.storage_area(o)) {
        Some(a) => Ok(a),
        None => Err(s.type_error("Illegal invocation")),
    }
}

/// The getter of `window.localStorage` and `window.sessionStorage`. It makes the object on the first read.
pub(crate) fn area_get<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, session: bool) -> Res<'s, Local<'s>> {
    let cell = &d.nav.areas[session as usize];
    let o = match cell.get() {
        Some(o) => o,
        None => old::<E, _>(s, |it| {
            let proto = d.nav.p.get().map(|p| p.storage_proto);
            let o = d.new_source(it, crate::browser_sources::Source::Storage, proto)?;
            cell.set(Some(o));
            d.nav.area_held.borrow_mut().push(it.persist_value(E::obj_raw(o)));
            Ok(o)
        })?,
    };
    Ok(d.obj_local(s, o))
}

/// `area`, and the origin of the document. `Unsupported` while the document URL is not configured.
fn target<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>) -> Res<'s, (bool, String)> {
    let a = area(d, s, this)?;
    Ok((a, lift::<E, _>(s, origin_of(d))?))
}

/// The origin of the document. `Unsupported` while the document URL is not configured.
fn origin_of<E: Eng>(d: &Dom<E>) -> R<E, String> {
    let origin = d.nav.s.borrow().entries.get(d.nav.s.borrow().cur).map(|e| e.url.origin());
    origin.ok_or_else(|| Fault::Unsupported(crate::browser_navigation::NOURL))
}

impl<E: Eng> Dom<E> {
    /// Which area `o` is (`true`: `sessionStorage`), or `None` when `o` is no `Storage` object.
    pub(crate) fn storage_area(&self, o: E::Obj) -> Option<bool> {
        if self.nav.areas[0].get() == Some(o) { Some(false) } else if self.nav.areas[1].get() == Some(o) { Some(true) } else { None }
    }

    /// `f` with the store and the origin of the document. `None` while the document URL is not configured.
    fn storage_with<T>(&self, f: impl FnOnce(&WebStorage, &str) -> T) -> Result<Option<T>, Pending> {
        let s = self.nav.s.try_borrow().map_err(|_| Pending::Busy)?;
        Ok(s.entries.get(s.cur).map(|e| f(&s.store, &e.url.origin())))
    }

    pub(crate) fn storage_get(&self, session: bool, key: &[u16]) -> Result<Option<Vec<u16>>, Pending> {
        Ok(self.storage_with(|st, o| st.get(session, o, key))?.flatten().map(Vec::from))
    }

    pub(crate) fn storage_len(&self, session: bool) -> Result<usize, Pending> {
        Ok(self.storage_with(|st, o| st.len(session, o))?.unwrap_or(0))
    }

    pub(crate) fn storage_keys(&self, session: bool) -> Result<Vec<Vec<u16>>, Pending> {
        Ok(self.storage_with(|st, o| st.keys(session, o))?.unwrap_or_default().into_iter().map(Vec::from).collect())
    }
}

/// The text of a key for a message (lone surrogates become U+FFFD).
fn lossy(u: &[u16]) -> String {
    String::from_utf16_lossy(u)
}

/// `Host::named_write` for `localStorage` and `sessionStorage`: the named setter, deleter and `[[DefineOwnProperty]]`.
/// `Ok(false)` when `req.owner` is no `Storage` object (`Dom::named_write` calls this for `Storage` objects only).
pub(crate) fn write<E: Eng>(d: &Dom<E>, it: &mut E::Vm, owner: E::Obj, key: &[u16], op: NamedOp<E>) -> R<E, bool> {
    let Some(session) = d.storage_area(owner) else { return Ok(false) };
    let named = format!("Failed to set a named property '{}' on 'Storage'", lossy(key));
    let value = match op {
        NamedOp::Set(v) => v,
        NamedOp::Define(desc) => {
            if desc.get.is_some() || desc.set.is_some() {
                return Err(it.type_error_o(&format!("{named}: Accessor properties are not allowed.")));
            }
            // WebIDL: only a data descriptor (a `value` or a `writable` field) runs the setter. Its flags are ignored.
            if desc.value.is_none() && desc.writable.is_none() {
                return Ok(false);
            }
            desc.value.unwrap_or(E::undef())
        }
        NamedOp::Delete => {
            let origin = origin_of(d)?;
            let store = d.nav.s.borrow().store.clone();
            store.remove(session, &origin, &key);
            return Ok(true);
        }
    };
    run::<E, _>(it, |s| {
        let v = s.engine().raw_local(&value);
        let Some(v) = units(d, s, v, &named)? else { return Err(over(d, s, &key, &named, "storage-item")) };
        let origin = lift::<E, _>(s, origin_of(d))?;
        store_set(d, s, session, &origin, &key, &v, &named)
    })
    .map(|()| true)
}

/// The `QuotaExceededError` of a write that does not fit. A sandbox bound of bare (`bound`) fails the one call in the same way: a browser
/// throws this error from a full store and never stops the page. The message names the bound, so an agent sees which one it was.
fn over<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, k: &[u16], who: &str, bound: &str) -> bare_js_api::Error<'s> {
    let k = lossy(k);
    let why = if bound.is_empty() { String::new() } else { format!(" (bare bound {bound})") };
    d.dom_err_s(s, "QuotaExceededError", &format!("{who}: Setting the value of '{k}' exceeded the quota{why}."))
}

/// The write of `setItem` and of the named setter. `who` starts the quota message.
fn store_set<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, session: bool, origin: &str, k: &[u16], v: &[u16], who: &str) -> Res<'s, ()> {
    let store = d.nav.s.borrow().store.clone();
    s.charge(1 + store.len(session, origin) as u64 / 16)?;
    match store.set(session, origin, k, v) {
        Ok(()) => Ok(()),
        Err(StoreError::Quota) => Err(over(d, s, k, who, "")),
        Err(StoreError::Limit(w)) => Err(over(d, s, k, who, w)),
    }
}

/// The argument `i` that `need` has checked.
fn at<'s>(a: Args<'s>, i: usize) -> Local<'s> {
    a.get(i).expect("checked by need")
}

fn need<'s, E: Eng>(s: &mut Sc<'s, E>, a: Args<'s>, n: usize, name: &str) -> Res<'s, ()> {
    if a.len() < n {
        return Err(s.type_error(&format!("Failed to execute '{name}' on 'Storage': {n} argument{} required, but only {} present.", if n == 1 { "" } else { "s" }, a.len())));
    }
    Ok(())
}

/// `ToString(v)` as UTF-16 units (lone surrogates are kept). `None` when the text is longer than one item may be: no store holds it.
/// (`Option` is a value or nothing, as `T | undefined` in TypeScript.)
fn units<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, v: Local<'s>, who: &str) -> Res<'s, Option<Vec<u16>>> {
    if s.type_of(v) == Type::Symbol {
        return Err(s.type_error(&format!("{who}: Cannot convert a Symbol value to a string")));
    }
    // A string is read in place: `to_string` pushes a local for another value only.
    let max = d.nav.lim.get().max_item_units;
    let read = |s: &Sc<'_, E>, t| {
        s.as_str(t).map(|t| {
            let len = t.len();
            let u = (len <= max).then(|| match t {
                Str::Latin1(b) => b.iter().map(|&c| c as u16).collect(),
                Str::Utf16(u) => u.to_vec(),
            });
            (len, u)
        })
    };
    let (len, u) = match read(s, v) {
        Some(r) => r,
        None => {
            let t = s.to_string(v)?;
            read(s, t).ok_or_else(|| s.type_error("not a string"))?
        }
    };
    let Some(u) = u else { return Ok(None) };
    s.charge((len / 32) as u64 + 1)?;
    Ok(Some(u))
}

pub(crate) fn get_item<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let session = area(d, s, this)?;
    need::<E>(s, a, 1, "getItem")?;
    let Some(k) = units(d, s, at(a, 0), "Failed to execute 'getItem' on 'Storage'")? else { return Ok(s.null()) };
    let origin = lift::<E, _>(s, origin_of(d))?;
    let store = d.nav.s.borrow().store.clone();
    s.charge(1 + store.len(session, &origin) as u64 / 16)?;
    match store.get(session, &origin, &k) {
        Some(v) => s.string_utf16(&v),
        None => Ok(s.null()),
    }
}

pub(crate) fn set_item<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let session = area(d, s, this)?;
    need::<E>(s, a, 2, "setItem")?;
    let who = "Failed to execute 'setItem' on 'Storage'";
    let Some(k) = units(d, s, at(a, 0), who)? else { return Err(over(d, s, &[0x2026], who, "storage-item")) };
    let Some(v) = units(d, s, at(a, 1), who)? else { return Err(over(d, s, &k, who, "storage-item")) };
    // A script may have changed the document URL while it coerced the arguments: read the origin now.
    let origin = lift::<E, _>(s, origin_of(d))?;
    store_set(d, s, session, &origin, &k, &v, "Failed to execute 'setItem' on 'Storage'")?;
    Ok(s.undefined())
}

pub(crate) fn remove_item<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let session = area(d, s, this)?;
    need::<E>(s, a, 1, "removeItem")?;
    let Some(k) = units(d, s, at(a, 0), "Failed to execute 'removeItem' on 'Storage'")? else { return Ok(s.undefined()) };
    let origin = lift::<E, _>(s, origin_of(d))?;
    let store = d.nav.s.borrow().store.clone();
    s.charge(1 + store.len(session, &origin) as u64 / 16)?;
    store.remove(session, &origin, &k);
    Ok(s.undefined())
}

pub(crate) fn clear<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let (session, origin) = target(d, s, this)?;
    let store = d.nav.s.borrow().store.clone();
    store.clear(session, &origin);
    Ok(s.undefined())
}

pub(crate) fn key<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let session = area(d, s, this)?;
    need::<E>(s, a, 1, "key")?;
    let n = s.to_number(at(a, 0))?;
    let origin = lift::<E, _>(s, origin_of(d))?;
    // WebIDL `unsigned long`: NaN is 0, the rest wraps modulo 2^32.
    let i = if n.is_finite() { n.trunc().rem_euclid(4294967296.0) as usize } else { 0 };
    let store = d.nav.s.borrow().store.clone();
    match store.key_at(session, &origin, i) {
        Some(k) => s.string_utf16(&k),
        None => Ok(s.null()),
    }
}

pub(crate) fn length<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let (session, origin) = target(d, s, this)?;
    let n = d.nav.s.borrow().store.len(session, &origin);
    Ok(s.number(n as f64))
}

#[cfg(test)]
mod state_tests {
    use super::*;
    use bare_net::cookie::Cookie;

    fn filled() -> WebStorage {
        let s = WebStorage::default();
        s.set_item(false, "https://a.test", "k", "v\u{e9}").unwrap();
        s.set_item(true, "http://b.test:8080", "t", "x").unwrap();
        let mut c = Cookie::new("sid", "1", "a.test", true, "/");
        c.http_only = true;
        s.jar().lock().unwrap().insert(c, 0.0);
        s
    }

    #[test]
    fn storage_state_round_trips_all_three_parts() {
        let st = filled().export();
        let t = WebStorage::default();
        t.import(&st).unwrap();
        let u = t.export();
        assert_eq!(u.local, vec![("https://a.test".to_string(), vec![("k".to_string(), "v\u{e9}".to_string())])]);
        assert_eq!(u.session, vec![("http://b.test:8080".to_string(), vec![("t".to_string(), "x".to_string())])]);
        assert_eq!(u.cookies.len(), 1);
        assert!(u.cookies[0].http_only && u.cookies[0].name == "sid");
    }

    #[test]
    fn storage_state_import_over_the_limit_is_an_error() {
        let t = WebStorage::new(StorageLimits { max_origins: 1, ..StorageLimits::default() });
        let f = filled();
        f.set_item(false, "https://c.test", "k", "v").unwrap();
        let st = f.export();
        assert_eq!(t.import(&st), Err(StoreError::Limit("storage-origins")));
    }
}
