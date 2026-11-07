//! `localStorage` and `sessionStorage`: an in-memory store for each origin, and the JS `Storage` bindings.
//!
//! The data lives in a `WebStorage`, not in the browser. A test (or the future loader) can keep one `WebStorage` and
//! attach it to a new `Browser` (`Browser::attach_storage`), so a "reload" keeps the same session. The store is only
//! memory. It never writes a file. Nothing here is browser-restart persistence.
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
//! enumerable, configurable data property). The two objects are host named objects (`Heap::new_named`, `heap_named.rs`), and the
//! heap asks `Dom` (`NamedSource`) on every read, so nothing is cached. A name does not hide the prototype chain (there is no
//! `[LegacyOverrideBuiltIns]`): `getItem`, `length` and the like stay the methods, and a write to such a name makes an ordinary
//! property on the object, as in Chrome. `Object.defineProperty` with a data descriptor is a `setItem` (the flags are ignored); an
//! accessor descriptor is a `TypeError`. `preventExtensions`, `seal` and `freeze` are refused.
//!
//! Gates: `key(n)` and the order of the names are the insertion order; Chrome uses the order of a hash table, which the Standard leaves
//! open. `Object.keys` and `for in` do not list a name that the prototype chain hides (Chrome lists it, except an accessor such as
//! `length`). There is no `storage` event, and no sharing of a live area between two `Browser`s except through a shared `WebStorage`.
//! Without a document URL a read finds nothing and a write is `Unsupported`.

use crate::browser::*;
use bare_js::eval::{Error, Interp};
use bare_js::heap::{HeapError, NamedOp, NamedRequest};
use bare_js::value::{ObjId, PropertyKey, Value};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

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
}

/// Why a write failed.
#[derive(Debug, PartialEq, Eq)]
pub enum StoreError {
    /// The origin is over its quota (`QuotaExceededError` in JS).
    Quota,
    /// A sandbox bound (origins, entries or total size).
    Limit(&'static str),
}

/// The `localStorage` and `sessionStorage` data of all origins. Clones share the data.
#[derive(Clone)]
pub struct WebStorage {
    local: Rc<RefCell<Areas>>,
    session: Rc<RefCell<Areas>>,
    /// The cookies that scripts set (`browser_cookie.rs`). A new tab shares them, as it shares `localStorage`.
    cookies: Rc<RefCell<crate::browser_cookie::Jar>>,
    lim: StorageLimits,
}

impl Default for WebStorage {
    fn default() -> Self {
        WebStorage::new(StorageLimits::default())
    }
}

impl WebStorage {
    pub fn new(lim: StorageLimits) -> WebStorage {
        WebStorage { local: Default::default(), session: Default::default(), cookies: Default::default(), lim }
    }

    /// The same `localStorage` data with a new, empty `sessionStorage` (a new tab).
    pub fn new_session(&self) -> WebStorage {
        WebStorage { local: self.local.clone(), session: Default::default(), cookies: self.cookies.clone(), lim: self.lim }
    }

    pub(crate) fn cookies(&self) -> Rc<RefCell<crate::browser_cookie::Jar>> {
        self.cookies.clone()
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
        if a.items.is_empty() {
            areas.by_origin.remove(origin);
        }
    }

    pub(crate) fn clear(&self, session: bool, origin: &str) {
        let mut areas = self.areas(session).borrow_mut();
        if let Some(a) = areas.by_origin.remove(origin) {
            areas.total -= a.units;
        }
    }

    // ----- for tests and drivers -----

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
fn area(d: &Dom, it: &mut Interp<'_>, this: Value) -> R<bool> {
    match this {
        Value::Object(o) => d.storage_area(o),
        _ => None,
    }
    .ok_or_else(|| it.type_error("Illegal invocation"))
}

/// The getter of `window.localStorage` and `window.sessionStorage`. It makes the object on the first read.
pub(crate) fn area_get(d: &Dom, it: &mut Interp<'_>, session: bool) -> R<Value> {
    let cell = &d.nav.areas[session as usize];
    let o = match cell.get() {
        Some(o) => o,
        None => {
            let proto = d.nav.p.get().map(|p| p.storage_proto);
            let o = it.heap.new_named_open(proto).map_err(|e| it.heap_error(e))?;
            cell.set(Some(o));
            d.fixed.borrow_mut().push(o);
            o
        }
    };
    Ok(Value::Object(o))
}

/// `area`, and the origin of the document. `Unsupported` while the document URL is not configured.
fn target(d: &Dom, it: &mut Interp<'_>, this: Value) -> R<(bool, String)> {
    Ok((area(d, it, this)?, origin_of(d)?))
}

/// The origin of the document. `Unsupported` while the document URL is not configured.
fn origin_of(d: &Dom) -> R<String> {
    let origin = d.nav.s.borrow().entries.get(d.nav.s.borrow().cur).map(|e| e.url.origin());
    origin.ok_or_else(|| Error::Unsupported(unsupported(crate::browser_navigation::NOURL)))
}

impl Dom {
    /// Which area `o` is (`true`: `sessionStorage`), or `None` when `o` is no `Storage` object.
    pub(crate) fn storage_area(&self, o: ObjId) -> Option<bool> {
        if self.nav.areas[0].get() == Some(o) { Some(false) } else if self.nav.areas[1].get() == Some(o) { Some(true) } else { None }
    }

    /// `f` with the store and the origin of the document. `None` while the document URL is not configured.
    fn storage_with<T>(&self, f: impl FnOnce(&WebStorage, &str) -> T) -> Result<Option<T>, HeapError> {
        let s = self.nav.s.try_borrow().map_err(|_| HeapError::HostUnavailable)?;
        Ok(s.entries.get(s.cur).map(|e| f(&s.store, &e.url.origin())))
    }

    pub(crate) fn storage_get(&self, session: bool, key: &[u16]) -> Result<Option<Vec<u16>>, HeapError> {
        Ok(self.storage_with(|st, o| st.get(session, o, key))?.flatten().map(Vec::from))
    }

    pub(crate) fn storage_len(&self, session: bool) -> Result<usize, HeapError> {
        Ok(self.storage_with(|st, o| st.len(session, o))?.unwrap_or(0))
    }

    pub(crate) fn storage_keys(&self, session: bool) -> Result<Vec<Vec<u16>>, HeapError> {
        Ok(self.storage_with(|st, o| st.keys(session, o))?.unwrap_or_default().into_iter().map(Vec::from).collect())
    }
}

/// The text of a key for a message (lone surrogates become U+FFFD).
fn lossy(u: &[u16]) -> String {
    String::from_utf16_lossy(u)
}

/// `Host::named_write` for `localStorage` and `sessionStorage`: the named setter, deleter and `[[DefineOwnProperty]]`.
/// `Ok(false)` when `req.owner` is no `Storage` object (`Dom::named_write` calls this for `Storage` objects only).
pub(crate) fn write(d: &Dom, it: &mut Interp<'_>, req: NamedRequest) -> R<bool> {
    let Some(session) = d.storage_area(req.owner) else { return Ok(false) };
    let key: Vec<u16> = match req.key {
        PropertyKey::Index(i) => i.to_string().encode_utf16().collect(),
        PropertyKey::String(s) => match it.heap.str_units(s) {
            Ok(u) => u.to_vec(),
            Err(e) => return Err(it.heap_error(e)),
        },
        PropertyKey::Symbol(_) => return Ok(false),
    };
    let named = format!("Failed to set a named property '{}' on 'Storage'", lossy(&key));
    let value = match req.op {
        NamedOp::Set(v) => v,
        NamedOp::Define(desc) => {
            if desc.get.is_some() || desc.set.is_some() {
                return Err(it.type_error(&format!("{named}: Accessor properties are not allowed.")));
            }
            // WebIDL: only a data descriptor (a `value` or a `writable` field) runs the setter. Its flags are ignored.
            if desc.value.is_none() && desc.writable.is_none() {
                return Ok(false);
            }
            desc.value.unwrap_or(Value::Undefined)
        }
        NamedOp::Delete => {
            let origin = origin_of(d)?;
            let store = d.nav.s.borrow().store.clone();
            store.remove(session, &origin, &key);
            return Ok(true);
        }
    };
    let v = units(d, it, value, &named)?;
    let origin = origin_of(d)?;
    store_set(d, it, session, &origin, &key, &v, &named).map(|()| true)
}

/// The write of `setItem` and of the named setter. `who` starts the quota message.
fn store_set(d: &Dom, it: &mut Interp<'_>, session: bool, origin: &str, k: &[u16], v: &[u16], who: &str) -> R<()> {
    let store = d.nav.s.borrow().store.clone();
    it.charge(1 + store.len(session, origin) as u64 / 16)?;
    match store.set(session, origin, k, v) {
        Ok(()) => Ok(()),
        Err(StoreError::Quota) => {
            let k = lossy(k);
            Err(d.dom_err(it, "QuotaExceededError", &format!("{who}: Setting the value of '{k}' exceeded the quota.")))
        }
        Err(StoreError::Limit(w)) => Err(d.lim(w)),
    }
}

fn need(it: &mut Interp<'_>, a: &[Value], n: usize, name: &str) -> R<()> {
    if a.len() < n {
        return Err(it.type_error(&format!("Failed to execute '{name}' on 'Storage': {n} argument{} required, but only {} present.", if n == 1 { "" } else { "s" }, a.len())));
    }
    Ok(())
}

/// `ToString(v)` as UTF-16 units (lone surrogates are kept).
fn units(d: &Dom, it: &mut Interp<'_>, v: Value, who: &str) -> R<Vec<u16>> {
    if matches!(v, Value::Symbol(_)) {
        return Err(it.type_error(&format!("{who}: Cannot convert a Symbol value to a string")));
    }
    let s = it.to_js_string(v)?;
    let len = it.heap.str_len(s).map_err(Error::Heap)?;
    if len > d.nav.lim.get().max_item_units {
        return Err(d.lim("storage-item"));
    }
    it.charge((len / 32) as u64 + 1)?;
    match it.heap.str_units(s) {
        Ok(u) => Ok(u.to_vec()),
        Err(e) => Err(it.heap_error(e)),
    }
}

fn str_value(it: &mut Interp<'_>, u: &[u16]) -> R<Value> {
    match it.heap.intern_units(u) {
        Ok(s) => Ok(Value::String(s)),
        Err(e) => Err(it.heap_error(e)),
    }
}

pub(crate) fn get_item(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> R<Value> {
    let session = area(d, it, this)?;
    need(it, a, 1, "getItem")?;
    let k = units(d, it, a[0], "Failed to execute 'getItem' on 'Storage'")?;
    let (_, origin) = target(d, it, this)?;
    let store = d.nav.s.borrow().store.clone();
    it.charge(1 + store.len(session, &origin) as u64 / 16)?;
    match store.get(session, &origin, &k) {
        Some(v) => str_value(it, &v),
        None => Ok(Value::Null),
    }
}

pub(crate) fn set_item(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> R<Value> {
    let session = area(d, it, this)?;
    need(it, a, 2, "setItem")?;
    let who = "Failed to execute 'setItem' on 'Storage'";
    let k = units(d, it, a[0], who)?;
    let v = units(d, it, a[1], who)?;
    // A script may have changed the document URL while it coerced the arguments: read the origin now.
    let (_, origin) = target(d, it, this)?;
    store_set(d, it, session, &origin, &k, &v, "Failed to execute 'setItem' on 'Storage'")?;
    Ok(Value::Undefined)
}

pub(crate) fn remove_item(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> R<Value> {
    let session = area(d, it, this)?;
    need(it, a, 1, "removeItem")?;
    let k = units(d, it, a[0], "Failed to execute 'removeItem' on 'Storage'")?;
    let (_, origin) = target(d, it, this)?;
    let store = d.nav.s.borrow().store.clone();
    it.charge(1 + store.len(session, &origin) as u64 / 16)?;
    store.remove(session, &origin, &k);
    Ok(Value::Undefined)
}

pub(crate) fn clear(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> R<Value> {
    let (session, origin) = target(d, it, this)?;
    let store = d.nav.s.borrow().store.clone();
    store.clear(session, &origin);
    Ok(Value::Undefined)
}

pub(crate) fn key(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> R<Value> {
    let session = area(d, it, this)?;
    need(it, a, 1, "key")?;
    let n = it.to_js_number(a[0])?;
    let (_, origin) = target(d, it, this)?;
    // WebIDL `unsigned long`: NaN is 0, the rest wraps modulo 2^32.
    let i = if n.is_finite() { n.trunc().rem_euclid(4294967296.0) as usize } else { 0 };
    let store = d.nav.s.borrow().store.clone();
    match store.key_at(session, &origin, i) {
        Some(k) => str_value(it, &k),
        None => Ok(Value::Null),
    }
}

pub(crate) fn length(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> R<Value> {
    let (session, origin) = target(d, it, this)?;
    let n = d.nav.s.borrow().store.len(session, &origin);
    Ok(Value::Number(n as f64))
}
