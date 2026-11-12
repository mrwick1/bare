//! `crypto`, `navigator`, `Blob`, `File` and `FormData`.
//!
//! Lazy: the five globals are accessors until a script first reads or writes one (the way `Intl` and `BigInt` work in the
//! engine). A page that uses none of them pays for five functions. The first read makes every object of this file
//! once (`made`) and replaces its own accessor with a data property. A write replaces the accessor with the written value.
//!
//! `crypto` (Web Crypto): `getRandomValues(array)` and `randomUUID()`. The bytes come from `bare_tls::random::fill`, the
//! OS source, with no fallback: if it fails, the call throws an `OperationError`. `getRandomValues` takes an integer typed
//! array (not `Float32Array`, `Float64Array`, `DataView` or `ArrayBuffer`: `TypeMismatchError`), at most 65,536 bytes
//! (`QuotaExceededError`), fills it in place and returns it. `randomUUID` is RFC 9562 version 4, in lower case.
//! There is no `crypto.subtle`.
//!
//! `navigator`: getters on `Navigator.prototype` with fixed values (`USER_AGENT` is the one place of the string).
//! `userAgentData` (WICG UA Client Hints) has `brands` (a frozen array), `mobile` and `platform`, `toJSON()` of these three, and
//! `getHighEntropyValues(hints)`. That method returns a Promise of an object with the three plus each hint asked for:
//! `architecture` "x86", `bitness` "64", `model` "", `platformVersion` "", `uaFullVersion` and `fullVersionList` (from `CHROME`),
//! `wow64` false and `formFactors` ["Desktop"]. It ignores other hints. `userAgent` and `userAgentData` report the same data.
//! `cookieEnabled` is true, as asked. `document.cookie` and the network share one jar (`bare_net::cookie`, `browser_cookie.rs`). There is no `mediaDevices`.
//!
//! `Blob`: `new Blob(parts, {type})` with strings, `ArrayBuffer`s, views and Blobs, `size`, `type`, `text()`,
//! `arrayBuffer()`, `slice()`. `File extends Blob` adds `name` and `lastModified`. A Blob keeps its bytes in a side table
//! (a copy), bounded by `MAX_BLOB` for each Blob and `MAX_TOTAL` together. `endings` is ignored (always "transparent").
//! Absent: `bytes()`, `stream()`, structured clone.
//!
//! Object URLs (W3C File API, "Blob URL Store"): `URL.createObjectURL(blob)` registers the Blob under a new `blob:ORIGIN/UUID` in a
//! store of this document and returns the URL. `URL.revokeObjectURL(url)` removes it. A URL that is not in the store (never made,
//! revoked, or made by another document) cannot be read. The store holds its Blobs alive and at most `MAX_URLS` entries. The readers are
//! `blob_url_read` callers: `fetch` and `XMLHttpRequest` (`browser_fetch.rs`, `browser_xhr.rs`) and a script element made by a script (`browser_loader.rs`).
//! A bytes copy is made at each read. `MediaSource` is not supported, so `createObjectURL` takes a Blob or a File only.
//!
//! `FormData`: `append`, `set`, `get`, `getAll`, `has`, `delete`, `entries`, `keys`, `values`, `forEach` and
//! `[Symbol.iterator]`. A value is a string or a File. Another Blob becomes a File ("blob", or `filename`). A value that
//! is not a Blob goes through `ToString`. The iterators are live. `new FormData(form)` takes the entry list of the form (`Dom::entry_list` in `browser_submit.rs`: the successful controls it owns, in tree order); a file input is an empty File named "". It fires no `formdata` event.
//! As a `fetch`, `Request` or `Response` body (`Dom::web_body`): a Blob is its bytes, typed by its `type`; a FormData is
//! `multipart/form-data` with a new random boundary for each request, written in one buffer.
//!
//! GC: the made objects are roots. A form traces its Blobs and an iterator traces its form. Side tables are pruned at `collect`.

use crate::browser::*;
use crate::browser_platform::{V, arg};
use bare_js::eval::{Error, Interp};
use bare_js::heap::{Elem, PropertyDescriptor};
use bare_js::value::{ObjId, PropertyKey, Value};
use std::cell::{Cell, RefCell};
use std::fmt::Write;

/// The user agent string, and the major version that `userAgentData.brands` reports. The only place of both.
pub(crate) const USER_AGENT: &str = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/155.0.0.0 Safari/537.36";
const CHROME: &str = "155";

const MAX_BLOB: usize = 64 << 20;
const MAX_TOTAL: usize = 256 << 20;
const MAX_OBJECTS: usize = 1 << 16;
const MAX_ENTRIES: usize = 1 << 16;
const MAX_URLS: usize = 1 << 12;
const MAX_RANDOM: usize = 65536;

const NAMES: [&str; 5] = ["crypto", "navigator", "Blob", "File", "FormData"];

struct BlobData {
    bytes: Vec<u8>,
    ty: String,
    /// `Some((name, lastModified))` for a File.
    file: Option<(String, f64)>,
}

pub(crate) enum Ent {
    S(String),
    B(ObjId),
}

#[derive(Default)]
struct Form {
    list: Vec<(String, Ent)>,
    bytes: usize,
}

struct IterObj {
    form: ObjId,
    idx: usize,
    /// 0 keys, 1 values, 2 entries.
    kind: u8,
}

/// Every object made on the first use. All are roots.
#[derive(Clone, Copy)]
struct Made {
    /// `crypto`, `navigator`, `Blob`, `File`, `FormData` (the order of `NAMES`).
    glob: [ObjId; 5],
    langs: ObjId,
    uad: ObjId,
    brands: ObjId,
    blob_p: ObjId,
    file_p: ObjId,
    form_p: ObjId,
    iter_p: ObjId,
}

#[derive(Default)]
struct WState {
    made: Option<Made>,
    blobs: IdMap<ObjId, BlobData>,
    forms: IdMap<ObjId, Form>,
    iters: IdMap<ObjId, IterObj>,
    /// The Blob URL store: `blob:` URL to its Blob. The Blobs are roots.
    urls: std::collections::HashMap<String, ObjId>,
    /// Bytes of all Blobs and of the names and strings of all forms.
    bytes: usize,
}

#[derive(Default)]
pub(crate) struct WebHost {
    s: RefCell<WState>,
    /// A form or an iterator was made and has not been pruned. `Dom::trace` runs for every object of a collect, so it skips
    /// `trace` below, with no borrow, while this is false.
    pub(crate) active: Cell<bool>,
    /// Tests set this to get a fixed multipart boundary.
    #[cfg(test)]
    pub(crate) boundary: RefCell<Option<String>>,
}

pub(crate) fn roots(d: &Dom, out: &mut Vec<Value>) {
    let s = d.web.s.borrow();
    out.extend(s.urls.values().map(|&o| Value::Object(o)));
    if let Some(m) = s.made {
        out.extend(m.glob.into_iter().chain([m.langs, m.uad, m.brands, m.blob_p, m.file_p, m.form_p, m.iter_p]).map(Value::Object));
    }
}

pub(crate) fn trace(d: &Dom, owner: ObjId, out: &mut Vec<Value>) {
    let s = d.web.s.borrow();
    if let Some(f) = s.forms.get(&owner) {
        out.extend(f.list.iter().filter_map(|(_, e)| if let Ent::B(o) = e { Some(Value::Object(*o)) } else { None }));
    } else if let Some(i) = s.iters.get(&owner) {
        out.push(Value::Object(i.form));
    }
}

pub(crate) fn prune(d: &Dom, live: &dyn Fn(ObjId) -> bool) {
    let mut s = d.web.s.borrow_mut();
    s.blobs.retain(|&o, _| live(o));
    s.forms.retain(|&o, _| live(o));
    s.iters.retain(|&o, _| live(o));
    d.web.active.set(!s.forms.is_empty() || !s.iters.is_empty());
    s.bytes = s.blobs.values().map(|b| b.bytes.len()).sum::<usize>() + s.forms.values().map(|f| f.bytes).sum::<usize>();
}

impl Dom {
    /// A lower bound of the body bytes of `v`, if `v` is a Blob or a FormData of this file. `fetch` reserves it before `web_body` copies.
    pub(crate) fn web_body_len(&self, v: Value) -> Option<usize> {
        let Value::Object(o) = v else { return None };
        let s = self.web.s.borrow();
        if let Some(b) = s.blobs.get(&o) {
            return Some(b.bytes.len());
        }
        let f = s.forms.get(&o)?;
        Some(f.list.iter().map(|(n, e)| n.len() + match e { Ent::S(v) => v.len(), Ent::B(b) => s.blobs[b].bytes.len() }).sum())
    }

    /// "Extract a body" of a Blob or a FormData: its bytes, written once, and the `Content-Type` it implies. A Blob body is its
    /// bytes, typed by its `type`. A FormData body is `multipart/form-data` (HTML "multipart/form-data encoding algorithm")
    /// with a new random boundary. A result over `max` bytes is a limit error.
    pub(crate) fn web_body(&self, it: &mut Interp<'_>, v: Value, max: usize) -> R<(Vec<u8>, Option<String>)> {
        let Value::Object(o) = v else { unreachable!() };
        let s = self.web.s.borrow();
        if let Some(b) = s.blobs.get(&o) {
            it.charge((b.bytes.len() / 64) as u64)?;
            return Ok((b.bytes.clone(), Some(b.ty.clone()).filter(|t| !t.is_empty())));
        }
        let f = &s.forms[&o];
        let boundary = self.boundary(it)?;
        // An upper bound of the size, so that the buffer never grows: a body can be many MB. A name grows 6 times at most, a value 2.
        let wire = f.list.iter().map(|(n, e)| match e {
            Ent::S(v) => v.len() * 2,
            Ent::B(b) => s.blobs[b].bytes.len() + s.blobs[b].file.as_ref().map_or(0, |f| f.0.len() * 3) + 80,
        } + n.len() * 6 + boundary.len() + 120);
        let mut out = Vec::with_capacity(wire.sum::<usize>() + boundary.len() + 8);
        for (name, e) in &f.list {
            out.extend_from_slice(b"--");
            out.extend_from_slice(boundary.as_bytes());
            out.extend_from_slice(b"\r\nContent-Disposition: form-data; name=\"");
            text(&mut out, name, true, true);
            out.push(b'"');
            match e {
                Ent::S(v) => {
                    out.extend_from_slice(b"\r\n\r\n");
                    text(&mut out, v, true, false);
                }
                Ent::B(b) => {
                    let b = &s.blobs[b];
                    out.extend_from_slice(b"; filename=\"");
                    text(&mut out, b.file.as_ref().map_or("blob", |f| &f.0), false, true);
                    out.extend_from_slice(b"\"\r\nContent-Type: ");
                    out.extend_from_slice(if b.ty.is_empty() { "application/octet-stream" } else { &b.ty }.as_bytes());
                    out.extend_from_slice(b"\r\n\r\n");
                    out.extend_from_slice(&b.bytes);
                }
            }
            out.extend_from_slice(b"\r\n");
            if out.len() > max {
                return Err(self.lim("fetch-request-body"));
            }
        }
        out.extend_from_slice(b"--");
        out.extend_from_slice(boundary.as_bytes());
        out.extend_from_slice(b"--\r\n");
        it.charge((out.len() / 64) as u64)?;
        Ok((out, Some(format!("multipart/form-data; boundary={boundary}"))))
    }

    /// A new boundary for each request, as Chrome makes it: `----WebKitFormBoundary` and 16 letters or digits from the OS source.
    fn boundary(&self, it: &mut Interp<'_>) -> R<String> {
        #[cfg(test)]
        if let Some(b) = self.web.boundary.borrow().clone() {
            return Ok(b);
        }
        let mut r = [0u8; 16];
        if bare_tls::random::fill(&mut r).is_err() {
            return Err(random_failed(self, it));
        }
        const ALNUM: &[u8; 62] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
        Ok(r.iter().fold(String::from("----WebKitFormBoundary"), |mut s, b| {
            s.push(ALNUM[*b as usize % 62] as char);
            s
        }))
    }
}

/// Write `s` for a multipart part. `norm`: a lone CR or LF becomes CRLF. `esc`: `"`, CR and LF become `%22`, `%0D` and `%0A`.
fn text(out: &mut Vec<u8>, s: &str, norm: bool, esc: bool) {
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'\r' | b'\n' if norm => {
                i += usize::from(b[i] == b'\r' && b.get(i + 1) == Some(&b'\n'));
                out.extend_from_slice(if esc { b"%0D%0A" } else { b"\r\n" });
            }
            b'\r' if esc => out.extend_from_slice(b"%0D"),
            b'\n' if esc => out.extend_from_slice(b"%0A"),
            b'"' if esc => out.extend_from_slice(b"%22"),
            c => out.push(c),
        }
        i += 1;
    }
}

// ------------------------------------------------------------------------------------------ install

macro_rules! lazy {
    ($($f:ident $i:literal),*) => { $(fn $f(d: &Dom, it: &mut Interp<'_>, _: Value, a: &[Value]) -> V { lazy(d, it, $i, a) })* };
}
lazy!(lazy0 0, lazy1 1, lazy2 2, lazy3 3, lazy4 4);

pub(crate) fn install(d: &Dom, it: &mut Interp<'_>) -> R<()> {
    let g = it.global_object();
    for (i, h) in [lazy0 as H, lazy1, lazy2, lazy3, lazy4].into_iter().enumerate() {
        let f = d.add_fn(it, h, NAMES[i], 0, false)?;
        let k = d.key(it, NAMES[i])?;
        let desc = PropertyDescriptor { get: Some(Some(f)), set: Some(Some(f)), enumerable: Some(i < 2), configurable: Some(true), ..Default::default() };
        it.heap.define_own_property(g, k, &desc).map_err(|e| it.heap_error(e))?;
    }
    Ok(())
}

/// The getter (no argument) and the setter (the new value) of global `NAMES[i]`.
fn lazy(d: &Dom, it: &mut Interp<'_>, i: usize, a: &[Value]) -> V {
    let k = d.key(it, NAMES[i])?;
    let g = it.global_object();
    if let Some(&v) = a.first() {
        it.define_data(g, k, v, true, i < 2, true)?;
        return Ok(Value::Undefined);
    }
    let m = made(d, it)?;
    let v = Value::Object(m.glob[i]);
    it.define_data(g, k, v, true, i < 2, true)?;
    Ok(v)
}

fn made(d: &Dom, it: &mut Interp<'_>) -> R<Made> {
    let m = d.web.s.borrow().made;
    if let Some(m) = m {
        return Ok(m);
    }
    let m = make(d, it)?;
    d.web.s.borrow_mut().made = Some(m);
    Ok(m)
}

/// A WebIDL operation: writable, enumerable, configurable.
fn op(d: &Dom, it: &mut Interp<'_>, on: ObjId, name: &str, len: u32, h: H) -> R<ObjId> {
    let f = d.add_fn(it, h, name, len, false)?;
    let k = d.key(it, name)?;
    it.define_data(on, k, Value::Object(f), true, true, true)?;
    Ok(f)
}

/// A constructor with its `prototype`, and `constructor` of the prototype.
fn iface(d: &Dom, it: &mut Interp<'_>, name: &str, len: u32, h: H, proto: ObjId) -> R<ObjId> {
    let f = d.add_ctor_nt(it, h, name, len)?;
    let (kp, kc) = (d.key(it, "prototype")?, d.key(it, "constructor")?);
    it.define_data(f, kp, Value::Object(proto), false, false, false)?;
    it.define_data(proto, kc, Value::Object(f), true, false, true)?;
    Ok(f)
}

fn make(d: &Dom, it: &mut Interp<'_>) -> R<Made> {
    let g = it.global_object();
    let op_ = it.object_prototype();
    let (ks, kt) = (d.key(it, "Symbol")?, d.key(it, "toStringTag")?);
    let sym = it.get_value(Value::Object(g), ks)?;
    let tag = match it.get_value(sym, kt)? {
        Value::Symbol(s) => Some(PropertyKey::Symbol(s)),
        _ => None,
    };
    let tagged = |it: &mut Interp<'_>, o: ObjId, name: &str| -> R<()> {
        if let Some(k) = tag {
            let v = d.js_str(it, name)?;
            it.define_data(o, k, v, false, false, true)?;
        }
        Ok(())
    };
    let proto = |it: &mut Interp<'_>, parent: ObjId, name: &str| -> R<ObjId> {
        let o = d.obj(it, parent)?;
        tagged(it, o, name)?;
        Ok(o)
    };

    // crypto
    let cp = proto(it, op_, "Crypto")?;
    op(d, it, cp, "getRandomValues", 1, get_random_values)?;
    op(d, it, cp, "randomUUID", 0, random_uuid)?;
    let crypto = d.obj(it, cp)?;

    // navigator
    let np = proto(it, op_, "Navigator")?;
    let getters: [(&str, H); 8] = [
        ("userAgent", |d, it, t, _| nav(d, it, t).and_then(|_| d.js_str(it, USER_AGENT))),
        ("userAgentData", |d, it, t, _| nav(d, it, t).map(|m| Value::Object(m.uad))),
        ("language", |d, it, t, _| nav(d, it, t).and_then(|_| d.js_str(it, "en-US"))),
        ("languages", |d, it, t, _| nav(d, it, t).map(|m| Value::Object(m.langs))),
        ("platform", |d, it, t, _| nav(d, it, t).and_then(|_| d.js_str(it, "Linux x86_64"))),
        ("onLine", |d, it, t, _| nav(d, it, t).map(|_| Value::Bool(true))),
        ("cookieEnabled", |d, it, t, _| nav(d, it, t).map(|_| Value::Bool(true))),
        ("hardwareConcurrency", |d, it, t, _| nav(d, it, t).map(|_| Value::Number(std::thread::available_parallelism().map_or(1, |n| n.get()) as f64))),
    ];
    for (name, h) in getters {
        d.def_get(it, np, name, h, None)?;
    }
    crate::browser_basics::navigator(d, it, np)?;
    let navigator = d.obj(it, np)?;
    let en = [d.js_str(it, "en-US")?, d.js_str(it, "en")?];
    let langs = it.array_from(&en)?;
    let Value::Object(langs) = langs else { unreachable!() };
    freeze(it, langs)?;

    // navigator.userAgentData
    let up = proto(it, op_, "NavigatorUAData")?;
    d.def_get(it, up, "brands", |d, it, t, _| uad(d, it, t).map(|m| Value::Object(m.brands)), None)?;
    d.def_get(it, up, "mobile", |d, it, t, _| uad(d, it, t).map(|_| Value::Bool(false)), None)?;
    d.def_get(it, up, "platform", |d, it, t, _| uad(d, it, t).and_then(|_| d.js_str(it, "Linux")), None)?;
    op(d, it, up, "getHighEntropyValues", 1, high_entropy)?;
    op(d, it, up, "toJSON", 0, |d, it, t, _| uad(d, it, t).and_then(|m| low_entropy(d, it, m)).map(Value::Object))?;
    let uad = d.obj(it, up)?;
    let mut brands = Vec::new();
    for (b, v) in [("Not.A/Brand", "8"), ("Chromium", CHROME), ("Google Chrome", CHROME)] {
        let o = d.obj(it, op_)?;
        for (k, s) in [("brand", b), ("version", v)] {
            let (k, s) = (d.key(it, k)?, d.js_str(it, s)?);
            it.define_data(o, k, s, true, true, true)?;
        }
        brands.push(Value::Object(o));
    }
    let Value::Object(brands) = it.array_from(&brands)? else { unreachable!() };
    freeze(it, brands)?;

    // Blob, File
    let bp = proto(it, op_, "Blob")?;
    let blob = iface(d, it, "Blob", 0, blob_ctor, bp)?;
    d.def_get(it, bp, "size", |d, it, t, _| blob_of(d, it, t).map(|o| Value::Number(d.web.s.borrow().blobs[&o].bytes.len() as f64)), None)?;
    d.def_get(it, bp, "type", |d, it, t, _| blob_of(d, it, t).and_then(|o| { let ty = d.web.s.borrow().blobs[&o].ty.clone(); d.js_str(it, &ty) }), None)?;
    op(d, it, bp, "slice", 2, blob_slice)?;
    op(d, it, bp, "text", 0, blob_text)?;
    op(d, it, bp, "arrayBuffer", 0, blob_array_buffer)?;
    let fp = proto(it, bp, "File")?;
    let file = iface(d, it, "File", 2, file_ctor, fp)?;
    it.heap.set_prototype_of(file, Some(blob)).map_err(|e| it.heap_error(e))?;
    d.def_get(it, fp, "name", |d, it, t, _| file_of(d, it, t).and_then(|(n, _)| d.js_str(it, &n)), None)?;
    d.def_get(it, fp, "lastModified", |d, it, t, _| file_of(d, it, t).map(|(_, m)| Value::Number(m)), None)?;

    // FormData and its iterator
    let fo = proto(it, op_, "FormData")?;
    let form = iface(d, it, "FormData", 0, form_ctor, fo)?;
    for (name, len, h) in [("append", 2, f_append as H), ("delete", 1, f_delete), ("get", 1, f_get), ("getAll", 1, f_get_all), ("has", 1, f_has), ("set", 2, f_set), ("forEach", 1, f_for_each), ("keys", 0, |d, it, t, _| make_iter(d, it, t, 0)), ("values", 0, |d, it, t, _| make_iter(d, it, t, 1))] {
        op(d, it, fo, name, len, h)?;
    }
    let entries = op(d, it, fo, "entries", 0, |d, it, t, _| make_iter(d, it, t, 2))?;
    it.define_data(fo, it.symbol_iterator(), Value::Object(entries), true, false, true)?;
    let ip = d.obj(it, it.iterator_prototype())?;
    op(d, it, ip, "next", 0, iter_next)?;

    Ok(Made { glob: [crypto, navigator, blob, file, form], langs, uad, brands, blob_p: bp, file_p: fp, form_p: fo, iter_p: ip })
}

fn freeze(it: &mut Interp<'_>, o: ObjId) -> R<()> {
    it.heap.set_integrity(o, true).map(drop).map_err(|e| it.heap_error(e))
}

fn need(it: &mut Interp<'_>, a: &[Value], n: usize, what: &str) -> R<()> {
    if a.len() < n {
        return Err(it.type_error(&format!("Failed to execute '{what}': {n} argument{} required, but only {} present.", if n == 1 { "" } else { "s" }, a.len())));
    }
    Ok(())
}

/// `ToString(v)` as a USVString: a lone surrogate becomes U+FFFD.
fn usv(d: &Dom, it: &mut Interp<'_>, v: Value) -> R<String> {
    let s = it.to_js_string(v)?;
    let len = it.heap.str_len(s).map_err(Error::Heap)?;
    if len > d.lim.max_string_bytes {
        return Err(d.lim("dom-string"));
    }
    it.charge((len / 32) as u64)?;
    it.heap.to_string_lossy(s).map_err(|e| it.heap_error(e))
}

fn illegal(it: &mut Interp<'_>) -> Error {
    it.type_error("Illegal invocation")
}

// ------------------------------------------------------------------------------------------ crypto

fn crypto_this(d: &Dom, it: &mut Interp<'_>, this: Value) -> R<()> {
    match (this, d.web.s.borrow().made) {
        (Value::Object(o), Some(m)) if o == m.glob[0] => Ok(()),
        _ => Err(illegal(it)),
    }
}

fn random_failed(d: &Dom, it: &mut Interp<'_>) -> Error {
    d.dom_err(it, "OperationError", "The operating system random source failed.")
}

fn get_random_values(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    crypto_this(d, it, this)?;
    need(it, a, 1, "getRandomValues' on 'Crypto")?;
    let integer = matches!(a[0], Value::Object(o) if it.heap.view_type(o).is_some_and(|t| t != Elem::Data && !t.is_float()));
    if !integer {
        return Err(d.dom_err(it, "TypeMismatchError", "The data argument must be an integer-type TypedArray."));
    }
    let Value::Object(o) = a[0] else { unreachable!() };
    let len = it.heap.bytes(o).map_or(0, <[u8]>::len);
    if len > MAX_RANDOM {
        return Err(d.dom_err(it, "QuotaExceededError", &format!("The ArrayBufferView's byte length ({len}) exceeds the number of bytes of entropy available via this API ({MAX_RANDOM}).")));
    }
    it.charge((len / 64) as u64)?;
    if let Some(b) = it.heap.bytes_mut(o)
        && bare_tls::random::fill(b).is_err()
    {
        return Err(random_failed(d, it));
    }
    Ok(a[0])
}

/// A random UUID, RFC 9562 version 4, in lower case. `None` if the OS source fails.
fn uuid_v4() -> Option<String> {
    let mut b = [0u8; 16];
    bare_tls::random::fill(&mut b).ok()?;
    b[6] = b[6] & 0x0f | 0x40;
    b[8] = b[8] & 0x3f | 0x80;
    let mut s = String::with_capacity(36);
    for (i, x) in b.iter().enumerate() {
        if matches!(i, 4 | 6 | 8 | 10) {
            s.push('-');
        }
        let _ = write!(s, "{x:02x}");
    }
    Some(s)
}

fn random_uuid(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    crypto_this(d, it, this)?;
    let Some(s) = uuid_v4() else { return Err(random_failed(d, it)) };
    d.js_str(it, &s)
}

// ------------------------------------------------------------------------------------------ navigator

fn nav(d: &Dom, it: &mut Interp<'_>, this: Value) -> R<Made> {
    match (this, d.web.s.borrow().made) {
        (Value::Object(o), Some(m)) if o == m.glob[1] => Ok(m),
        _ => Err(illegal(it)),
    }
}

pub(crate) fn nav_this(d: &Dom, it: &mut Interp<'_>, this: Value) -> R<()> {
    nav(d, it, this).map(|_| ())
}

fn uad(d: &Dom, it: &mut Interp<'_>, this: Value) -> R<Made> {
    match (this, d.web.s.borrow().made) {
        (Value::Object(o), Some(m)) if o == m.uad => Ok(m),
        _ => Err(illegal(it)),
    }
}

/// A new object with `brands`, `mobile` and `platform`: the low entropy hints.
fn low_entropy(d: &Dom, it: &mut Interp<'_>, m: Made) -> R<ObjId> {
    let o = d.obj(it, it.object_prototype())?;
    let p = d.js_str(it, "Linux")?;
    for (k, v) in [("brands", Value::Object(m.brands)), ("mobile", Value::Bool(false)), ("platform", p)] {
        put(d, it, o, k, v)?;
    }
    Ok(o)
}

fn put(d: &Dom, it: &mut Interp<'_>, o: ObjId, k: &str, v: Value) -> R<()> {
    let k = d.key(it, k)?;
    it.define_data(o, k, v, true, true, true)
}

/// `getHighEntropyValues(hints)`: `hints` is any iterable of strings.
fn high_entropy(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let p = it.host_new_promise()?;
    let r = (|| -> V {
        let m = uad(d, it, this)?;
        need(it, a, 1, "NavigatorUAData.getHighEntropyValues")?;
        let bad = "Failed to execute 'getHighEntropyValues' on 'NavigatorUAData': The provided value cannot be converted to a sequence.";
        let hints = a[0];
        let Some(mut iter) = (if matches!(hints, Value::Object(_)) { it.host_iter_open(hints)? } else { None }) else { return Err(it.type_error(bad)) };
        let out = low_entropy(d, it, m)?;
        loop {
            let step = match it.host_iter_next(&mut iter) {
                Ok(Some(x)) => it.charge(1).and_then(|_| usv(d, it, x)).and_then(|h| hint(d, it, out, &h)).map(|_| true),
                Ok(None) => Ok(false),
                Err(e) => Err(e),
            };
            match step {
                Ok(true) => {}
                Ok(false) => return Ok(Value::Object(out)),
                Err(e) => return Err(it.host_iter_fail(&mut iter, e)),
            }
        }
    })();
    crate::browser_fetch::settle(it, p, r)
}

/// One high entropy hint of `getHighEntropyValues`. Unknown names add nothing.
fn hint(d: &Dom, it: &mut Interp<'_>, out: ObjId, name: &str) -> R<()> {
    let v = match name {
        "architecture" => d.js_str(it, "x86")?,
        "bitness" => d.js_str(it, "64")?,
        "model" | "platformVersion" => d.js_str(it, "")?,
        "uaFullVersion" => d.js_str(it, &format!("{CHROME}.0.0.0"))?,
        "wow64" => Value::Bool(false),
        "formFactors" => {
            let f = d.js_str(it, "Desktop")?;
            it.array_from(&[f])?
        }
        "fullVersionList" => {
            let mut list = Vec::new();
            for (b, v) in [("Not.A/Brand", "8.0.0.0".to_string()), ("Chromium", format!("{CHROME}.0.0.0")), ("Google Chrome", format!("{CHROME}.0.0.0"))] {
                let o = d.obj(it, it.object_prototype())?;
                let (bv, vv) = (d.js_str(it, b)?, d.js_str(it, &v)?);
                put(d, it, o, "brand", bv)?;
                put(d, it, o, "version", vv)?;
                list.push(Value::Object(o));
            }
            it.array_from(&list)?
        }
        _ => return Ok(()),
    };
    put(d, it, out, name, v)
}

// ------------------------------------------------------------------------------------------ object URLs

/// `URL.createObjectURL(blob)`.
#[cold]
#[inline(never)]
pub(crate) fn create_object_url(d: &Dom, it: &mut Interp<'_>, _: Value, a: &[Value]) -> V {
    need(it, a, 1, "createObjectURL' on 'URL")?;
    let blob = match a[0] {
        Value::Object(o) if d.web.s.borrow().blobs.contains_key(&o) => o,
        _ => return Err(it.type_error("Failed to execute 'createObjectURL' on 'URL': Overload resolution failed.")),
    };
    if d.web.s.borrow().urls.len() >= MAX_URLS {
        return Err(d.lim("web-objects"));
    }
    let Some(id) = uuid_v4() else { return Err(random_failed(d, it)) };
    let origin = d.cur_rec().map_or_else(|_| "null".to_string(), |r| r.origin());
    let url = format!("blob:{origin}/{id}");
    d.web.s.borrow_mut().urls.insert(url.clone(), blob);
    d.js_str(it, &url)
}

/// `URL.revokeObjectURL(url)`. A URL that is not in the store is ignored.
#[cold]
#[inline(never)]
pub(crate) fn revoke_object_url(d: &Dom, it: &mut Interp<'_>, _: Value, a: &[Value]) -> V {
    need(it, a, 1, "revokeObjectURL' on 'URL")?;
    let url = d.string_arg(it, a[0])?;
    d.web.s.borrow_mut().urls.remove(url.trim_matches(|c: char| c <= ' ').split('#').next().unwrap_or(""));
    Ok(Value::Undefined)
}

/// Is `s` a `blob:` URL (any case, after leading and trailing control and space characters)?
pub(crate) fn is_blob_url(s: &str) -> bool {
    s.trim_start_matches(|c: char| c <= ' ').get(..5).is_some_and(|p| p.eq_ignore_ascii_case("blob:"))
}

/// The bytes and type of the Blob under the `blob:` URL `url` in the store of this document. `None`: not there (never made, revoked).
/// The fragment is not part of the key.
#[cold]
#[inline(never)]
pub(crate) fn blob_url_read(d: &Dom, url: &str) -> Option<(Vec<u8>, String)> {
    let s = d.web.s.borrow();
    let b = s.blobs.get(s.urls.get(url.trim_matches(|c: char| c <= ' ').split('#').next()?)?)?;
    Some((b.bytes.clone(), b.ty.clone()))
}

// ------------------------------------------------------------------------------------------ Blob and File

/// Room for `n` more bytes and one more object.
fn room(d: &Dom, n: usize) -> R<()> {
    let s = d.web.s.borrow();
    if s.blobs.len() + s.forms.len() + s.iters.len() >= MAX_OBJECTS || s.bytes.saturating_add(n) > MAX_TOTAL {
        return Err(d.lim("web-objects"));
    }
    Ok(())
}

fn new_blob(d: &Dom, it: &mut Interp<'_>, proto: ObjId, b: BlobData) -> V {
    let n = b.bytes.len();
    room(d, n)?;
    it.charge((n / 64) as u64)?;
    let o = d.obj(it, proto)?;
    let mut s = d.web.s.borrow_mut();
    s.bytes += n;
    s.blobs.insert(o, b);
    Ok(Value::Object(o))
}

/// A new Blob with these bytes and type (the `response` of an `XMLHttpRequest` with `responseType = "blob"`).
pub(crate) fn blob_from_bytes(d: &Dom, it: &mut Interp<'_>, bytes: Vec<u8>, ty: String) -> V {
    let m = made(d, it)?;
    new_blob(d, it, m.blob_p, BlobData { bytes, ty: blob_type(ty), file: None })
}

/// The value of `key="..."` in a header line value.
fn param(v: &str, key: &str) -> Option<String> {
    let at = v.find(&format!("; {key}=\""))? + key.len() + 4;
    Some(v[at..v[at..].find('"')? + at].to_string())
}

/// "Parse a multipart/form-data name" (Fetch): `%22`, `%0D` and `%0A` are `"`, CR and LF.
fn unescape(s: &str) -> String {
    s.replace("%22", "\"").replace("%0D", "\r").replace("%0A", "\n")
}

/// `Some((filename, content type))` for a file.
type FileInfo = Option<(String, String)>;
type Part<'a> = (String, FileInfo, &'a [u8]);

/// The parts of a `multipart/form-data` body: name, `Some((filename, content type))` for a file, and the content.
fn multipart<'a>(b: &'a [u8], boundary: &str) -> Option<Vec<Part<'a>>> {
    let find = |from: usize, pat: &[u8]| b[from..].windows(pat.len()).position(|w| w == pat).map(|p| p + from);
    let delim = [b"--".as_slice(), boundary.as_bytes()].concat();
    let sep = [b"\r\n".as_slice(), &delim].concat();
    let mut pos = find(0, &delim)? + delim.len();
    let mut out = Vec::new();
    while !b[pos..].starts_with(b"--") {
        pos = find(pos, b"\r\n")? + 2;
        let hend = if b[pos..].starts_with(b"\r\n") { pos - 2 } else { find(pos, b"\r\n\r\n")? };
        let head = String::from_utf8_lossy(&b[pos.min(hend)..hend]).into_owned();
        let end = find(hend + 4, &sep)?;
        let (mut name, mut file, mut ty) = (None, None, "text/plain".to_string());
        for line in head.split("\r\n") {
            let Some((h, v)) = line.split_once(':') else { continue };
            match h.to_ascii_lowercase().as_str() {
                "content-disposition" => {
                    let v = format!("; {}", v.trim());
                    name = param(&v, "name").map(|n| unescape(&n));
                    file = param(&v, "filename").map(|n| unescape(&n));
                }
                "content-type" => ty = v.trim().to_string(),
                _ => {}
            }
        }
        out.push((name?, file.map(|f| (f, ty)), &b[hend + 4..end]));
        pos = end + sep.len();
    }
    Some(out)
}

/// `Body.formData()`: a FormData from an `application/x-www-form-urlencoded` or `multipart/form-data` body, or `None`.
pub(crate) fn form_from(d: &Dom, it: &mut Interp<'_>, bytes: &[u8], ct: &str) -> R<Option<Value>> {
    let essence = ct.split(';').next().unwrap_or("").trim().to_ascii_lowercase();
    let list: Vec<(String, FileInfo, Vec<u8>)> = match essence.as_str() {
        "application/x-www-form-urlencoded" => crate::browser_navigation_url::form_parse(&String::from_utf8_lossy(bytes)).into_iter().map(|(n, v)| (n, None, v.into_bytes())).collect(),
        "multipart/form-data" => {
            let Some(b) = ct.split(';').skip(1).find_map(|p| p.trim().strip_prefix("boundary=")).map(|b| b.trim_matches('"')) else { return Ok(None) };
            let Some(parts) = multipart(bytes, b) else { return Ok(None) };
            parts.into_iter().map(|(n, f, c)| (n, f, c.to_vec())).collect()
        }
        _ => return Ok(None),
    };
    let m = made(d, it)?;
    room(d, 0)?;
    let o = d.obj(it, m.form_p)?;
    d.web.s.borrow_mut().forms.insert(o, Form::default());
    d.web.active.set(true);
    for (n, file, data) in list {
        let e = match file {
            None => Ent::S(String::from_utf8_lossy(&data).into_owned()),
            Some((name, ty)) => {
                let mtime = it.wall_now_ms();
                let Value::Object(f) = new_blob(d, it, m.file_p, BlobData { bytes: data, ty: blob_type(ty), file: Some((name, mtime)) })? else { unreachable!() };
                Ent::B(f)
            }
        };
        push(d, o, n, e)?;
    }
    Ok(Some(Value::Object(o)))
}

fn blob_of(d: &Dom, it: &mut Interp<'_>, this: Value) -> R<ObjId> {
    match this {
        Value::Object(o) if d.web.s.borrow().blobs.contains_key(&o) => Ok(o),
        _ => Err(illegal(it)),
    }
}

fn file_of(d: &Dom, it: &mut Interp<'_>, this: Value) -> R<(String, f64)> {
    let o = blob_of(d, it, this)?;
    d.web.s.borrow().blobs[&o].file.clone().ok_or_else(|| illegal(it))
}

/// The type of a Blob: the empty string when it has a character outside U+0020 to U+007E, else in lower case.
fn blob_type(t: String) -> String {
    if t.bytes().all(|b| (0x20..=0x7e).contains(&b)) { t.to_ascii_lowercase() } else { String::new() }
}

fn add_part(d: &Dom, it: &mut Interp<'_>, out: &mut Vec<u8>, x: Value) -> R<()> {
    let own = match x {
        Value::Object(o) => d.web.s.borrow().blobs.get(&o).map(|b| b.bytes.len()),
        _ => None,
    };
    let n = own.or_else(|| it.bytes_of(x).map(<[u8]>::len));
    if let Some(n) = n {
        if out.len().saturating_add(n) > MAX_BLOB {
            return Err(d.lim("blob-bytes"));
        }
        match x {
            Value::Object(o) if own.is_some() => out.extend_from_slice(&d.web.s.borrow().blobs[&o].bytes),
            _ => out.extend_from_slice(it.bytes_of(x).unwrap_or_default()),
        }
        return Ok(());
    }
    let s = usv(d, it, x)?;
    if out.len().saturating_add(s.len()) > MAX_BLOB {
        return Err(d.lim("blob-bytes"));
    }
    out.extend_from_slice(s.as_bytes());
    Ok(())
}

/// `sequence<BlobPart>`: the bytes of every part, read with the engine's own iteration.
fn parts(d: &Dom, it: &mut Interp<'_>, v: Value, who: &str) -> R<Vec<u8>> {
    let bad = format!("Failed to construct '{who}': The provided value cannot be converted to a sequence.");
    if !matches!(v, Value::Object(_)) {
        return Err(it.type_error(&bad));
    }
    let Some(mut iter) = it.host_iter_open(v)? else { return Err(it.type_error(&bad)) };
    let mut out = Vec::new();
    loop {
        let step = match it.host_iter_next(&mut iter) {
            Ok(Some(x)) => add_part(d, it, &mut out, x).map(|_| true),
            Ok(None) => Ok(false),
            Err(e) => Err(e),
        };
        match step {
            Ok(true) => {}
            Ok(false) => return Ok(out),
            Err(e) => return Err(it.host_iter_fail(&mut iter, e)),
        }
    }
}

/// `type` of a `BlobPropertyBag`, and `lastModified` of a `FilePropertyBag` when `file`.
fn options(d: &Dom, it: &mut Interp<'_>, v: Value, who: &str, file: bool) -> R<(String, Option<f64>)> {
    match v {
        Value::Undefined | Value::Null => return Ok((String::new(), None)),
        Value::Object(_) => {}
        _ => return Err(it.type_error(&format!("Failed to construct '{who}': The provided value is not of type '{who}PropertyBag'."))),
    }
    let kt = d.key(it, "type")?;
    let ty = match it.get_value(v, kt)? {
        Value::Undefined => String::new(),
        t => blob_type(usv(d, it, t)?),
    };
    let mut modified = None;
    if file {
        let km = d.key(it, "lastModified")?;
        match it.get_value(v, km)? {
            Value::Undefined => {}
            m => {
                let n = it.to_js_number(m)?;
                if !n.is_finite() {
                    return Err(it.type_error("Failed to construct 'File': The provided value is not finite."));
                }
                modified = Some(n.trunc());
            }
        }
    }
    Ok((ty, modified))
}

fn blob_ctor(d: &Dom, it: &mut Interp<'_>, _: Value, a: &[Value]) -> V {
    let intrinsic = d.web.s.borrow().made.expect("made").blob_p;
    let proto = it.host_proto_from_new_target(intrinsic)?;
    let bytes = match arg(a, 0) {
        Value::Undefined => Vec::new(),
        v => parts(d, it, v, "Blob")?,
    };
    let (ty, _) = options(d, it, arg(a, 1), "Blob", false)?;
    new_blob(d, it, proto, BlobData { bytes, ty, file: None })
}

fn file_ctor(d: &Dom, it: &mut Interp<'_>, _: Value, a: &[Value]) -> V {
    let intrinsic = d.web.s.borrow().made.expect("made").file_p;
    let proto = it.host_proto_from_new_target(intrinsic)?;
    need(it, a, 2, "construct 'File")?;
    let bytes = parts(d, it, a[0], "File")?;
    let name = usv(d, it, a[1])?;
    let (ty, modified) = options(d, it, arg(a, 2), "File", true)?;
    let modified = modified.unwrap_or_else(|| it.wall_now_ms());
    new_blob(d, it, proto, BlobData { bytes, ty, file: Some((name, modified)) })
}

/// `[Clamp] long long` start or end of `slice`, as an offset in `0..=size`.
fn offset(it: &mut Interp<'_>, v: Value, size: usize, default: usize) -> R<usize> {
    if matches!(v, Value::Undefined) {
        return Ok(default);
    }
    let n = it.to_js_number(v)?;
    let n = if n.is_nan() { 0.0 } else { n.trunc() };
    Ok(if n < 0.0 { (size as f64 + n).max(0.0) } else { n.min(size as f64) } as usize)
}

fn blob_slice(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let o = blob_of(d, it, this)?;
    let size = d.web.s.borrow().blobs[&o].bytes.len();
    let start = offset(it, arg(a, 0), size, 0)?;
    let end = offset(it, arg(a, 1), size, size)?;
    let ty = match arg(a, 2) {
        Value::Undefined => String::new(),
        t => blob_type(usv(d, it, t)?),
    };
    let bytes = d.web.s.borrow().blobs[&o].bytes.get(start..end.max(start)).unwrap_or_default().to_vec();
    let proto = d.web.s.borrow().made.expect("made").blob_p;
    new_blob(d, it, proto, BlobData { bytes, ty, file: None })
}

fn blob_text(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let p = it.host_new_promise()?;
    let r = (|| -> V {
        let o = blob_of(d, it, this)?;
        let text = {
            let s = d.web.s.borrow();
            let b = &s.blobs[&o].bytes;
            String::from_utf8_lossy(b.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(b)).into_owned()
        };
        it.charge((text.len() / 16) as u64)?;
        d.js_str(it, &text)
    })();
    crate::browser_fetch::settle(it, p, r)
}

fn blob_array_buffer(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let p = it.host_new_promise()?;
    let r = (|| -> V {
        let o = blob_of(d, it, this)?;
        let bytes = d.web.s.borrow().blobs[&o].bytes.clone();
        it.charge((bytes.len() / 64) as u64)?;
        it.new_array_buffer(bytes)
    })();
    crate::browser_fetch::settle(it, p, r)
}

// ------------------------------------------------------------------------------------------ FormData

fn cost(n: &str, e: &Ent) -> usize {
    n.len() + 32 + if let Ent::S(v) = e { v.len() } else { 0 }
}

fn form_ctor(d: &Dom, it: &mut Interp<'_>, _: Value, a: &[Value]) -> V {
    let intrinsic = d.web.s.borrow().made.expect("made").form_p;
    let proto = it.host_proto_from_new_target(intrinsic)?;
    let form = match arg(a, 0) {
        Value::Undefined => None,
        v => match d.node_of(v).filter(|&n| d.doc.borrow().is_html(n, "form")) {
            Some(n) => Some(n),
            None => return Err(it.type_error("Failed to construct 'FormData': parameter 1 is not of type 'HTMLFormElement'.")),
        },
    };
    room(d, 0)?;
    // The entries first: the form walk can fail, and then no FormData exists.
    let list = match form {
        Some(f) => d.entry_list(it, f, None)?,
        None => Vec::new(),
    };
    fill_form(d, it, proto, list)
}

/// A new FormData with these entries; `None` (a file input with no file) is an empty File with no name.
pub(crate) fn form_from_entries(d: &Dom, it: &mut Interp<'_>, list: Vec<(String, Option<Ent>)>) -> V {
    let m = made(d, it)?;
    fill_form(d, it, m.form_p, list)
}

fn fill_form(d: &Dom, it: &mut Interp<'_>, proto: ObjId, list: Vec<(String, Option<Ent>)>) -> V {
    let o = d.obj(it, proto)?;
    d.web.s.borrow_mut().forms.insert(o, Form::default());
    d.web.active.set(true);
    for (n, v) in list {
        let e = match v {
            Some(e) => e,
            None => {
                let fp = d.web.s.borrow().made.expect("made").file_p;
                let Value::Object(f) = new_blob(d, it, fp, BlobData { bytes: Vec::new(), ty: "application/octet-stream".into(), file: Some((String::new(), it.wall_now_ms())) })? else { unreachable!() };
                Ent::B(f)
            }
        };
        push(d, o, n, e)?;
    }
    Ok(Value::Object(o))
}

/// The most that one uploaded file may hold.
pub(crate) const MAX_UPLOAD: u64 = 100 << 20;

/// The content type of a file name, from its extension (a short list; else `application/octet-stream`).
fn mime_of(name: &str) -> &'static str {
    match name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).as_deref() {
        Some("txt" | "log") => "text/plain",
        Some("html" | "htm") => "text/html",
        Some("css") => "text/css",
        Some("csv") => "text/csv",
        Some("json") => "application/json",
        Some("pdf") => "application/pdf",
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("svg") => "image/svg+xml",
        Some("zip") => "application/zip",
        _ => "application/octet-stream",
    }
}

/// A File with the bytes of a file on disk, read now.
pub(crate) fn file_from_ref(d: &Dom, it: &mut Interp<'_>, f: &crate::browser::FileRef) -> V {
    let bad = |d: &Dom, it: &mut Interp<'_>, e: String| d.dom_err(it, "NotReadableError", &format!("The file {:?} cannot be read: {e}", f.name));
    let m = made(d, it)?;
    let len = std::fs::metadata(&f.path).map_err(|e| bad(d, it, e.to_string()))?.len();
    if len > MAX_UPLOAD {
        return Err(bad(d, it, format!("{len} bytes is over the {} MiB limit", MAX_UPLOAD >> 20)));
    }
    let bytes = std::fs::read(&f.path).map_err(|e| bad(d, it, e.to_string()))?;
    new_blob(d, it, m.file_p, BlobData { bytes, ty: mime_of(&f.name).into(), file: Some((f.name.clone(), f.modified_ms)) })
}

/// The entries of the FormData object `v`, a copy. `None` when `v` is not a FormData.
pub(crate) fn form_entries(d: &Dom, v: Value) -> Option<Vec<(String, Ent)>> {
    let Value::Object(o) = v else { return None };
    Some(d.web.s.borrow().forms.get(&o)?.list.iter().map(|(n, e)| (n.clone(), clone_ent(e))).collect())
}

/// The name of the File object `v`. `None` when `v` is not a File.
pub(crate) fn file_name(d: &Dom, v: Value) -> Option<String> {
    let Value::Object(o) = v else { return None };
    Some(d.web.s.borrow().blobs.get(&o)?.file.as_ref()?.0.clone())
}

fn form_of(d: &Dom, it: &mut Interp<'_>, this: Value) -> R<ObjId> {
    match this {
        Value::Object(o) if d.web.s.borrow().forms.contains_key(&o) => Ok(o),
        _ => Err(illegal(it)),
    }
}

/// The value of `append` and `set` (`a[1]`, and the file name `a[2]`): a string, or a File.
fn entry(d: &Dom, it: &mut Interp<'_>, a: &[Value], what: &str) -> R<Ent> {
    let blob = match a[1] {
        Value::Object(o) if d.web.s.borrow().blobs.contains_key(&o) => Some(o),
        _ => None,
    };
    let Some(o) = blob else {
        if a.len() > 2 {
            return Err(it.type_error(&format!("Failed to execute '{what}' on 'FormData': parameter 2 is not of type 'Blob'.")));
        }
        return Ok(Ent::S(usv(d, it, a[1])?));
    };
    let given = match arg(a, 2) {
        Value::Undefined => None,
        f => Some(usv(d, it, f)?),
    };
    let (file, bytes, ty) = {
        let s = d.web.s.borrow();
        let b = &s.blobs[&o];
        if b.file.is_some() && given.is_none() {
            return Ok(Ent::B(o));
        }
        (b.file.clone(), b.bytes.clone(), b.ty.clone())
    };
    let (name, modified) = file.unzip();
    let file = (given.or(name).unwrap_or_else(|| "blob".into()), modified.unwrap_or_else(|| it.wall_now_ms()));
    let proto = d.web.s.borrow().made.expect("made").file_p;
    let Value::Object(f) = new_blob(d, it, proto, BlobData { bytes, ty, file: Some(file) })? else { unreachable!() };
    Ok(Ent::B(f))
}

fn push(d: &Dom, f: ObjId, n: String, e: Ent) -> R<()> {
    let c = cost(&n, &e);
    let mut s = d.web.s.borrow_mut();
    if s.forms[&f].list.len() >= MAX_ENTRIES || s.bytes.saturating_add(c) > MAX_TOTAL {
        return Err(d.lim("formdata-entries"));
    }
    s.bytes += c;
    let form = s.forms.get_mut(&f).expect("checked");
    form.bytes += c;
    form.list.push((n, e));
    Ok(())
}

fn f_append(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let f = form_of(d, it, this)?;
    need(it, a, 2, "append' on 'FormData")?;
    let n = usv(d, it, a[0])?;
    let e = entry(d, it, a, "append")?;
    push(d, f, n, e)?;
    Ok(Value::Undefined)
}

/// Change the list with `f` (no JS runs inside) and count its bytes again.
fn edit(d: &Dom, o: ObjId, f: impl FnOnce(&mut Vec<(String, Ent)>)) {
    let mut s = d.web.s.borrow_mut();
    let s = &mut *s;
    let form = s.forms.get_mut(&o).expect("checked");
    f(&mut form.list);
    let now = form.list.iter().map(|(n, e)| cost(n, e)).sum();
    s.bytes = s.bytes - form.bytes + now;
    form.bytes = now;
}

fn f_set(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let f = form_of(d, it, this)?;
    need(it, a, 2, "set' on 'FormData")?;
    let n = usv(d, it, a[0])?;
    let e = entry(d, it, a, "set")?;
    let len = d.web.s.borrow().forms[&f].list.len();
    it.charge(len as u64 / 8)?;
    let pos = d.web.s.borrow().forms[&f].list.iter().position(|x| x.0 == n);
    match pos {
        None => push(d, f, n, e)?,
        Some(i) => edit(d, f, |l| {
            l[i].1 = e;
            let mut k = 0;
            l.retain(|x| {
                k += 1;
                k - 1 == i || x.0 != n
            });
        }),
    }
    Ok(Value::Undefined)
}

fn f_delete(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let f = form_of(d, it, this)?;
    need(it, a, 1, "delete' on 'FormData")?;
    let n = usv(d, it, a[0])?;
    edit(d, f, |l| l.retain(|x| x.0 != n));
    Ok(Value::Undefined)
}

fn ent_value(d: &Dom, it: &mut Interp<'_>, e: &Ent) -> V {
    match e {
        Ent::S(v) => d.js_str(it, v),
        Ent::B(o) => Ok(Value::Object(*o)),
    }
}

pub(crate) fn clone_ent(e: &Ent) -> Ent {
    match e {
        Ent::S(v) => Ent::S(v.clone()),
        Ent::B(o) => Ent::B(*o),
    }
}

fn f_get(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let f = form_of(d, it, this)?;
    need(it, a, 1, "get' on 'FormData")?;
    let n = usv(d, it, a[0])?;
    let found = d.web.s.borrow().forms[&f].list.iter().find(|x| x.0 == n).map(|x| clone_ent(&x.1));
    match found {
        Some(e) => ent_value(d, it, &e),
        None => Ok(Value::Null),
    }
}

fn f_get_all(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let f = form_of(d, it, this)?;
    need(it, a, 1, "getAll' on 'FormData")?;
    let n = usv(d, it, a[0])?;
    let found: Vec<Ent> = d.web.s.borrow().forms[&f].list.iter().filter(|x| x.0 == n).map(|x| clone_ent(&x.1)).collect();
    let mut vals = Vec::with_capacity(found.len());
    for e in &found {
        vals.push(ent_value(d, it, e)?);
    }
    it.array_from(&vals)
}

fn f_has(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let f = form_of(d, it, this)?;
    need(it, a, 1, "has' on 'FormData")?;
    let n = usv(d, it, a[0])?;
    Ok(Value::Bool(d.web.s.borrow().forms[&f].list.iter().any(|x| x.0 == n)))
}

/// The pair at `i` of form `f`, if both still exist.
fn pair_at(d: &Dom, f: ObjId, i: usize) -> Option<(String, Ent)> {
    d.web.s.borrow().forms.get(&f)?.list.get(i).map(|(n, e)| (n.clone(), clone_ent(e)))
}

fn f_for_each(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let f = form_of(d, it, this)?;
    let cb = arg(a, 0);
    if !it.is_callable_value(cb) {
        return Err(it.type_error("Failed to execute 'forEach' on 'FormData': parameter 1 is not of type 'Function'."));
    }
    let mut i = 0;
    loop {
        it.charge(1)?;
        let Some((n, e)) = pair_at(d, f, i) else { return Ok(Value::Undefined) };
        let (v, n) = (ent_value(d, it, &e)?, d.js_str(it, &n)?);
        it.call(cb, arg(a, 1), &[v, n, this])?;
        i += 1;
    }
}

fn make_iter(d: &Dom, it: &mut Interp<'_>, this: Value, kind: u8) -> V {
    let form = form_of(d, it, this)?;
    room(d, 0)?;
    let proto = d.web.s.borrow().made.expect("made").iter_p;
    let o = d.obj(it, proto)?;
    d.web.s.borrow_mut().iters.insert(o, IterObj { form, idx: 0, kind });
    d.web.active.set(true);
    Ok(Value::Object(o))
}

fn iter_next(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let id = match this {
        Value::Object(o) if d.web.s.borrow().iters.contains_key(&o) => o,
        _ => return Err(illegal(it)),
    };
    it.charge(1)?;
    let (form, idx, kind) = {
        let s = d.web.s.borrow();
        let i = &s.iters[&id];
        (i.form, i.idx, i.kind)
    };
    let (value, done) = match pair_at(d, form, idx) {
        None => (Value::Undefined, true),
        Some((n, e)) => {
            d.web.s.borrow_mut().iters.get_mut(&id).expect("checked").idx += 1;
            let v = match kind {
                0 => d.js_str(it, &n)?,
                1 => ent_value(d, it, &e)?,
                _ => {
                    let (n, v) = (d.js_str(it, &n)?, ent_value(d, it, &e)?);
                    it.array_from(&[n, v])?
                }
            };
            (v, false)
        }
    };
    let o = d.obj(it, it.object_prototype())?;
    let (kv, kd) = (d.key(it, "value")?, d.key(it, "done")?);
    it.define_data(o, kv, value, true, true, true)?;
    it.define_data(o, kd, Value::Bool(done), true, true, true)?;
    Ok(Value::Object(o))
}
