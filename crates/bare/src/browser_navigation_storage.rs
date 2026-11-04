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
//! Gates: `Storage` is an ordinary object. `localStorage.token` and `localStorage["a"] = 1` are NOT storage access (the
//! object is not extensible, so such a write is dropped or throws in strict mode). `Object.keys(localStorage)` is empty.
//! There is no `storage` event, and no sharing of a live area between two `Browser`s except through a shared `WebStorage`.

use crate::browser::*;
use bare_js::eval::{Error, Interp};
use bare_js::value::Value;
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
    lim: StorageLimits,
}

impl Default for WebStorage {
    fn default() -> Self {
        WebStorage::new(StorageLimits::default())
    }
}

impl WebStorage {
    pub fn new(lim: StorageLimits) -> WebStorage {
        WebStorage { local: Default::default(), session: Default::default(), lim }
    }

    /// The same `localStorage` data with a new, empty `sessionStorage` (a new tab).
    pub fn new_session(&self) -> WebStorage {
        WebStorage { local: self.local.clone(), session: Default::default(), lim: self.lim }
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

/// Which area `this` is, and the origin of the document. `Unsupported` while the document URL is not configured.
fn target(d: &Dom, it: &mut Interp<'_>, this: Value) -> R<(bool, String)> {
    let session = match (this, d.nav.p.get()) {
        (Value::Object(o), Some(p)) if o == p.local => false,
        (Value::Object(o), Some(p)) if o == p.session => true,
        _ => return Err(it.type_error("Illegal invocation")),
    };
    let origin = d.nav.s.borrow().entries.get(d.nav.s.borrow().cur).map(|e| e.url.origin());
    match origin {
        Some(o) => Ok((session, o)),
        None => Err(Error::Unsupported(unsupported(crate::browser_navigation::NOURL))),
    }
}

fn need(it: &mut Interp<'_>, a: &[Value], n: usize, name: &str) -> R<()> {
    if a.len() < n {
        return Err(it.type_error(&format!("Failed to execute '{name}' on 'Storage': {n} argument{} required, but only {} present.", if n == 1 { "" } else { "s" }, a.len())));
    }
    Ok(())
}

/// `ToString(v)` as UTF-16 units (lone surrogates are kept).
fn units(d: &Dom, it: &mut Interp<'_>, v: Value) -> R<Vec<u16>> {
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
    let (session, _) = target(d, it, this)?;
    need(it, a, 1, "getItem")?;
    let k = units(d, it, a[0])?;
    let (_, origin) = target(d, it, this)?;
    let store = d.nav.s.borrow().store.clone();
    it.charge(1 + store.len(session, &origin) as u64 / 16)?;
    match store.get(session, &origin, &k) {
        Some(v) => str_value(it, &v),
        None => Ok(Value::Null),
    }
}

pub(crate) fn set_item(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> R<Value> {
    let (session, _) = target(d, it, this)?;
    need(it, a, 2, "setItem")?;
    let k = units(d, it, a[0])?;
    let v = units(d, it, a[1])?;
    // A script may have changed the document URL while it coerced the arguments: read the origin now.
    let (_, origin) = target(d, it, this)?;
    let store = d.nav.s.borrow().store.clone();
    it.charge(1 + store.len(session, &origin) as u64 / 16)?;
    match store.set(session, &origin, &k, &v) {
        Ok(()) => Ok(Value::Undefined),
        Err(StoreError::Quota) => Err(d.dom_err(it, "QuotaExceededError", "Failed to execute 'setItem' on 'Storage': Setting the value exceeded the quota.")),
        Err(StoreError::Limit(w)) => Err(d.lim(w)),
    }
}

pub(crate) fn remove_item(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> R<Value> {
    let (session, _) = target(d, it, this)?;
    need(it, a, 1, "removeItem")?;
    let k = units(d, it, a[0])?;
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
    let (session, _) = target(d, it, this)?;
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
