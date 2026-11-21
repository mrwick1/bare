//! `crypto`, `navigator`, `Blob`, `File`, `FormData` and `SubtleCrypto`.
//!
//! Lazy: the six globals are accessors until a script first reads or writes one (the way `Intl` and `BigInt` work in the
//! engine). A page that uses none of them pays for six functions. The first read makes every object of this file
//! once (`made`) and replaces its own accessor with a data property. A write replaces the accessor with the written value.
//!
//! `crypto` (Web Crypto): `getRandomValues(array)` and `randomUUID()`. The bytes come from `bare_tls::random::fill`, the
//! OS source, with no fallback: if it fails, the call throws an `OperationError`. `getRandomValues` takes an integer typed
//! array (not `Float32Array`, `Float64Array`, `DataView` or `ArrayBuffer`: `TypeMismatchError`), at most 65,536 bytes
//! (`QuotaExceededError`), fills it in place and returns it. `randomUUID` is RFC 9562 version 4, in lower case.
//! `crypto.subtle` is a `SubtleCrypto` in a secure context (`browser_basics::secure`) and `undefined` otherwise. Its
//! `digest(algorithm, data)` hashes with SHA-1, SHA-256, SHA-384 or SHA-512 (`bare_tls`), a name or `{name}`, ASCII
//! case-insensitive. It copies the bytes of an ArrayBuffer, a typed array or a DataView when it is called, and resolves
//! with a new ArrayBuffer. An unknown name rejects with `NotSupportedError`; bad data rejects with `TypeError`. The other
//! methods reject with `NotSupportedError` until they are built.
//!
//! `navigator`: getters on `Navigator.prototype` with fixed values (`USER_AGENT` is the one place of the string).
//! `userAgentData` (WICG UA Client Hints) has `brands` (a frozen array), `mobile` and `platform`, `toJSON()` of these three, and
//! `getHighEntropyValues(hints)`. That method returns a Promise of an object with the three plus each hint asked for:
//! `architecture` "x86", `bitness` "64", `model` "", `platformVersion` "", `uaFullVersion` and `fullVersionList` (from `CHROME`),
//! `wow64` false and `formFactors` ["Desktop"]. It ignores other hints. `userAgent` and `userAgentData` report the same data.
//! The requests of the page send the same `User-Agent` and the low-entropy hints (`wire_identity`).
//! `cookieEnabled` is true, as asked. `document.cookie` and the network share one jar (`bare_net::cookie`, `browser_cookie.rs`). There is no `mediaDevices`.
//!
//! `Blob`: `new Blob(parts, {type})` with strings, `ArrayBuffer`s, views and Blobs, `size`, `type`, `text()`,
//! `arrayBuffer()`, `slice()`, `stream()` (the `body` of a hidden byte response, `browser_fetch::bytes_stream`). `File extends Blob` adds `name` and `lastModified`. A Blob keeps its bytes in a side table
//! (a copy), bounded by `MAX_BLOB` for each Blob and `MAX_TOTAL` together. `endings` is ignored (always "transparent").
//! Absent: `bytes()`, structured clone.
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
use crate::browser_scoped::*;
use bare_js::eval::{Error, Interp};
use bare_js::value::{ObjId, Value};
use bare_js_api::{Args, Attrs, Elem, Error as AErr, HostFn, Intrinsic, Local, Type};
use std::cell::{Cell, RefCell};
use std::fmt::Write;

type V = R<Value>;

/// The user agent string, and the major version that `userAgentData.brands` reports. The only place of both.
pub(crate) const USER_AGENT: &str = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/155.0.0.0 Safari/537.36";
const CHROME: &str = "155";
/// The brands of `userAgentData.brands` and of `sec-ch-ua`, in this order. The only place of them.
const BRANDS: [(&str, &str); 3] = [("Not.A/Brand", "8"), ("Chromium", CHROME), ("Google Chrome", CHROME)];
/// The platform of `userAgentData.platform`, and of `sec-ch-ua-platform` (quoted there). The only place of it.
const PLATFORM: &str = "Linux";

/// What the browser sends on the wire (`Client::set_identity`): `USER_AGENT`, and the low-entropy client hints of `userAgentData`
/// (`sec-ch-ua`, `sec-ch-ua-mobile`, `sec-ch-ua-platform`), made from the same constants as `navigator`.
pub(crate) fn wire_identity() -> bare_net::http::Identity {
    let brands: Vec<String> = BRANDS.iter().map(|(b, v)| format!("\"{b}\";v=\"{v}\"")).collect();
    let hints = vec![("sec-ch-ua", brands.join(", ")), ("sec-ch-ua-mobile", "?0".to_string()), ("sec-ch-ua-platform", format!("\"{PLATFORM}\""))];
    bare_net::http::Identity { user_agent: USER_AGENT, hints }
}

const MAX_BLOB: usize = 64 << 20;
const MAX_TOTAL: usize = 256 << 20;
const MAX_OBJECTS: usize = 1 << 16;
const MAX_ENTRIES: usize = 1 << 16;
const MAX_URLS: usize = 1 << 12;
const MAX_RANDOM: usize = 65536;

const NAMES: [&str; 6] = ["crypto", "navigator", "Blob", "File", "FormData", "SubtleCrypto"];

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
    /// `crypto`, `navigator`, `Blob`, `File`, `FormData`, `SubtleCrypto` (the order of `NAMES`).
    glob: [ObjId; 6],
    /// The one `crypto.subtle` object.
    subtle: ObjId,
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
        out.extend(m.glob.into_iter().chain([m.subtle, m.langs, m.uad, m.brands, m.blob_p, m.file_p, m.form_p, m.iter_p]).map(Value::Object));
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
        if page_random(&mut r).is_err() {
            return Err(random_failed_old(self, it));
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
    ($($f:ident $i:literal),*) => { $(fn $f<'s>(d: &Dom, s: &mut Sc<'s, '_>, _: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> { lazy(d, s, $i, a) })* };
}
lazy!(lazy0 0, lazy1 1, lazy2 2, lazy3 3, lazy4 4, lazy5 5);

pub(crate) fn install<'s>(d: &Dom, s: &mut Sc<'s, '_>) -> Res<'s, ()> {
    let g = s.global();
    for (i, h) in [lazy0 as HS, lazy1, lazy2, lazy3, lazy4, lazy5].into_iter().enumerate() {
        let f = d.add_fn_s(s, h, NAMES[i], 0, HostFn::Method)?;
        s.define_accessor_str(g, NAMES[i], Some(f), Some(f), Attrs { writable: false, enumerable: i < 2, configurable: true })?;
    }
    Ok(())
}

/// The getter (no argument) and the setter (the new value) of global `NAMES[i]`.
fn lazy<'s>(d: &Dom, s: &mut Sc<'s, '_>, i: usize, a: Args<'s>) -> Res<'s, Local<'s>> {
    let g = s.global();
    let attrs = Attrs { writable: true, enumerable: i < 2, configurable: true };
    if let Some(v) = a.get(0) {
        s.define_str(g, NAMES[i], v, attrs)?;
        return Ok(s.undefined());
    }
    let m = made_s(d, s)?;
    let v = d.obj_local(s, m.glob[i]);
    s.define_str(g, NAMES[i], v, attrs)?;
    Ok(v)
}

fn made_s<'s>(d: &Dom, s: &mut Sc<'s, '_>) -> Res<'s, Made> {
    let m = d.web.s.borrow().made;
    if let Some(m) = m {
        return Ok(m);
    }
    let m = make(d, s)?;
    d.web.s.borrow_mut().made = Some(m);
    Ok(m)
}

fn made(d: &Dom, it: &mut Interp<'_>) -> R<Made> {
    run(it, |s| made_s(d, s))
}

/// A WebIDL operation: writable, enumerable, configurable.
fn op<'s>(d: &Dom, s: &mut Sc<'s, '_>, on: Local<'s>, name: &str, len: u32, h: HS) -> Res<'s, Local<'s>> {
    let f = d.add_fn_s(s, h, name, len, HostFn::Method)?;
    s.define_str(on, name, f, Attrs::ALL)?;
    Ok(f)
}

/// An attribute getter: enumerable, configurable.
fn getter<'s>(d: &Dom, s: &mut Sc<'s, '_>, on: Local<'s>, name: &str, h: HS) -> Res<'s, ()> {
    let f = d.add_fn_s(s, h, &format!("get {name}"), 0, HostFn::Method)?;
    s.define_accessor_str(on, name, Some(f), None, Attrs { writable: false, enumerable: true, configurable: true })
}

/// A constructor with its `prototype`, and `constructor` of the prototype.
fn iface<'s>(d: &Dom, s: &mut Sc<'s, '_>, name: &str, len: u32, h: HS, proto: Local<'s>) -> Res<'s, Local<'s>> {
    let f = d.add_fn_s(s, h, name, len, HostFn::CtorNewTarget)?;
    s.define_str(f, "prototype", proto, Attrs { writable: false, enumerable: false, configurable: false })?;
    s.define_str(proto, "constructor", f, Attrs { writable: true, enumerable: false, configurable: true })?;
    Ok(f)
}

fn lid<'s>(s: &mut Sc<'s, '_>, l: Local<'s>) -> ObjId {
    obj_id(s, l).expect("an object that this file made")
}

fn make<'s>(d: &Dom, s: &mut Sc<'s, '_>) -> Res<'s, Made> {
    let obj_proto = s.engine().object_prototype();
    let op_ = d.obj_local(s, obj_proto);
    let tag = s.intrinsic(Intrinsic::SymbolToStringTag);
    let proto = |d: &Dom, s: &mut Sc<'s, '_>, parent: Local<'s>, name: &str| -> Res<'s, Local<'s>> {
        let po = lid(s, parent);
        let o = d.obj_s(s, po)?;
        let o = d.obj_local(s, o);
        let n = s.string(name)?;
        s.define(o, tag, n, Attrs { writable: false, enumerable: false, configurable: true })?;
        Ok(o)
    };
    let plain = |d: &Dom, s: &mut Sc<'s, '_>, parent: Local<'s>| -> Res<'s, Local<'s>> {
        let po = lid(s, parent);
        let o = d.obj_s(s, po)?;
        Ok(d.obj_local(s, o))
    };

    // crypto
    let cp = proto(d, s, op_, "Crypto")?;
    op(d, s, cp, "getRandomValues", 1, get_random_values)?;
    op(d, s, cp, "randomUUID", 0, random_uuid)?;
    getter(d, s, cp, "subtle", subtle_attr)?;
    let crypto = plain(d, s, cp)?;

    // SubtleCrypto: `digest` hashes; the other methods reject until they are built.
    let sp = proto(d, s, op_, "SubtleCrypto")?;
    op(d, s, sp, "digest", 2, subtle_digest)?;
    for (name, len) in [("encrypt", 3), ("decrypt", 3), ("sign", 3), ("verify", 4), ("generateKey", 3), ("importKey", 5), ("exportKey", 2), ("deriveKey", 5), ("deriveBits", 2), ("wrapKey", 4), ("unwrapKey", 7)] {
        op(d, s, sp, name, len, subtle_unsupported)?;
    }
    let subtle = plain(d, s, sp)?;
    let subtle_ctor = iface(d, s, "SubtleCrypto", 0, subtle_illegal, sp)?;

    // navigator
    let np = proto(d, s, op_, "Navigator")?;
    let getters: [(&str, HS); 8] = [
        ("userAgent", nav_user_agent),
        ("userAgentData", nav_uad),
        ("language", nav_language),
        ("languages", nav_languages),
        ("platform", nav_platform),
        ("onLine", nav_on_line),
        ("cookieEnabled", nav_cookie_enabled),
        ("hardwareConcurrency", nav_cores),
    ];
    for (name, h) in getters {
        getter(d, s, np, name, h)?;
    }
    let np_id = lid(s, np);
    old(s, |it| crate::browser_basics::navigator(d, it, np_id))?;
    let navigator = plain(d, s, np)?;
    let en = [d.js_str_s(s, "en-US")?, d.js_str_s(s, "en")?];
    let langs = s.new_array(&en)?;
    s.set_integrity(langs, true)?;

    // navigator.userAgentData
    let up = proto(d, s, op_, "NavigatorUAData")?;
    getter(d, s, up, "brands", uad_brands)?;
    getter(d, s, up, "mobile", uad_mobile)?;
    getter(d, s, up, "platform", uad_platform)?;
    op(d, s, up, "getHighEntropyValues", 1, high_entropy)?;
    op(d, s, up, "toJSON", 0, uad_to_json)?;
    let uad = plain(d, s, up)?;
    let mut brands = Vec::new();
    for (b, v) in BRANDS {
        let o = plain(d, s, op_)?;
        for (k, x) in [("brand", b), ("version", v)] {
            let x = d.js_str_s(s, x)?;
            s.define_str(o, k, x, Attrs::ALL)?;
        }
        brands.push(o);
    }
    let brands = s.new_array(&brands)?;
    s.set_integrity(brands, true)?;

    // Blob, File
    let bp = proto(d, s, op_, "Blob")?;
    let blob = iface(d, s, "Blob", 0, blob_ctor, bp)?;
    getter(d, s, bp, "size", blob_size)?;
    getter(d, s, bp, "type", blob_type_get)?;
    op(d, s, bp, "slice", 2, blob_slice)?;
    op(d, s, bp, "text", 0, blob_text)?;
    op(d, s, bp, "arrayBuffer", 0, blob_array_buffer)?;
    op(d, s, bp, "stream", 0, blob_stream)?;
    let fp = proto(d, s, bp, "File")?;
    let file = iface(d, s, "File", 2, file_ctor, fp)?;
    s.set_prototype_of(file, Some(blob))?;
    getter(d, s, fp, "name", file_name_get)?;
    getter(d, s, fp, "lastModified", file_modified)?;

    // FormData and its iterator
    let fo = proto(d, s, op_, "FormData")?;
    let form = iface(d, s, "FormData", 0, form_ctor, fo)?;
    for (name, len, h) in [("append", 2, f_append as HS), ("delete", 1, f_delete), ("get", 1, f_get), ("getAll", 1, f_get_all), ("has", 1, f_has), ("set", 2, f_set), ("forEach", 1, f_for_each), ("keys", 0, f_keys), ("values", 0, f_values)] {
        op(d, s, fo, name, len, h)?;
    }
    let entries = op(d, s, fo, "entries", 0, f_entries)?;
    let sym = s.intrinsic(Intrinsic::SymbolIterator);
    s.define(fo, sym, entries, Attrs { writable: true, enumerable: false, configurable: true })?;
    let ipr = s.engine().iterator_prototype();
    let ip = d.obj_local(s, ipr);
    let ip = plain(d, s, ip)?;
    op(d, s, ip, "next", 0, iter_next)?;

    let [crypto, navigator, blob, file, form, subtle_ctor, subtle, langs, uad, brands, bp, fp, fo, ip] = [crypto, navigator, blob, file, form, subtle_ctor, subtle, langs, uad, brands, bp, fp, fo, ip].map(|l| lid(s, l));
    Ok(Made { glob: [crypto, navigator, blob, file, form, subtle_ctor], subtle, langs, uad, brands, blob_p: bp, file_p: fp, form_p: fo, iter_p: ip })
}

fn need<'s>(s: &mut Sc<'s, '_>, a: Args<'s>, n: usize, what: &str) -> Res<'s, ()> {
    if a.len() < n {
        return Err(s.type_error(&format!("Failed to execute '{what}': {n} argument{} required, but only {} present.", if n == 1 { "" } else { "s" }, a.len())));
    }
    Ok(())
}

/// `ToString(v)` as a USVString: a lone surrogate becomes U+FFFD.
fn usv<'s>(d: &Dom, s: &mut Sc<'s, '_>, v: Local<'s>) -> Res<'s, String> {
    let l = s.to_string(v)?;
    let (len, text) = {
        let t = s.as_str(l).expect("a string");
        (t.len(), (t.len() <= d.lim.max_string_bytes).then(|| t.to_string_lossy()))
    };
    let Some(text) = text else { return Err(d.lim_s(s, "dom-string")) };
    s.charge((len / 32) as u64)?;
    Ok(text)
}

fn illegal<'s>(s: &mut Sc<'s, '_>) -> AErr<'s> {
    s.type_error("Illegal invocation")
}

// ------------------------------------------------------------------------------------------ crypto

fn crypto_this<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>) -> Res<'s, ()> {
    match (obj_id(s, this), d.web.s.borrow().made) {
        (Some(o), Some(m)) if o == m.glob[0] => Ok(()),
        _ => Err(illegal(s)),
    }
}

fn random_failed<'s>(d: &Dom, s: &mut Sc<'s, '_>) -> AErr<'s> {
    d.dom_err_s(s, "OperationError", "The operating system random source failed.")
}

/// The old form of `random_failed`, for `boundary`.
fn random_failed_old(d: &Dom, it: &mut Interp<'_>) -> Error {
    d.dom_err(it, "OperationError", "The operating system random source failed.")
}

fn get_random_values<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    crypto_this(d, s, this)?;
    need(s, a, 1, "getRandomValues' on 'Crypto")?;
    let x = arg_s(s, a, 0);
    let integer = s.view(x).is_some_and(|v| v.elem != Elem::Data && !matches!(v.elem, Elem::F32 | Elem::F64));
    if !integer {
        return Err(d.dom_err_s(s, "TypeMismatchError", "The data argument must be an integer-type TypedArray."));
    }
    let len = s.bytes(x).map_or(0, <[u8]>::len);
    if len > MAX_RANDOM {
        return Err(d.dom_err_s(s, "QuotaExceededError", &format!("The ArrayBufferView's byte length ({len}) exceeds the number of bytes of entropy available via this API ({MAX_RANDOM}).")));
    }
    s.charge((len / 64) as u64)?;
    if s.bytes_mut(x).is_some_and(|b| page_random(b).is_err()) {
        return Err(random_failed(d, s));
    }
    Ok(x)
}

/// Random bytes for page code (`crypto`, multipart boundaries): from the OS. With `BARE_SEED=<number>` in the environment they are one fixed
/// stream instead, so that a benchmark that replays recorded replies makes the same URLs and bodies in every run. The TLS code does not use it.
fn page_random(buf: &mut [u8]) -> Result<(), bare_tls::random::RandomError> {
    use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
    static SEED: std::sync::OnceLock<Option<AtomicU64>> = std::sync::OnceLock::new();
    let Some(state) = SEED.get_or_init(|| std::env::var("BARE_SEED").ok().and_then(|v| v.parse().ok()).map(AtomicU64::new)) else { return bare_tls::random::fill(buf) };
    for chunk in buf.chunks_mut(8) {
        // SplitMix64.
        let mut z = state.fetch_add(0x9E37_79B9_7F4A_7C15, Relaxed).wrapping_add(0x9E37_79B9_7F4A_7C15);
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        chunk.copy_from_slice(&(z ^ (z >> 31)).to_le_bytes()[..chunk.len()]);
    }
    Ok(())
}

/// A random UUID, RFC 9562 version 4, in lower case. `None` if the OS source fails.
fn uuid_v4() -> Option<String> {
    let mut b = [0u8; 16];
    page_random(&mut b).ok()?;
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

fn random_uuid<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    crypto_this(d, s, this)?;
    let Some(id) = uuid_v4() else { return Err(random_failed(d, s)) };
    d.js_str_s(s, &id)
}

/// `crypto.subtle`: a `SubtleCrypto` in a secure context, `undefined` otherwise.
fn subtle_attr<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    crypto_this(d, s, this)?;
    if !crate::browser_basics::secure(d) {
        return Ok(s.undefined());
    }
    let o = d.web.s.borrow().made.expect("made").subtle;
    Ok(d.obj_local(s, o))
}

/// Settle `p` with the outcome of `r`: a value fulfills, a thrown value rejects, any other error is the result.
fn settle<'s>(s: &mut Sc<'s, '_>, p: Local<'s>, r: Res<'s, Local<'s>>) -> Res<'s, Local<'s>> {
    match r {
        Ok(v) => s.resolve_promise(p, v)?,
        Err(AErr::Throw(v)) => s.reject_promise(p, v)?,
        Err(e) => return Err(e),
    }
    Ok(p)
}

// ------------------------------------------------------------------------------------------ SubtleCrypto

fn subtle_this<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>) -> Res<'s, ()> {
    match (obj_id(s, this), d.web.s.borrow().made) {
        (Some(o), Some(m)) if o == m.subtle => Ok(()),
        _ => Err(illegal(s)),
    }
}

fn subtle_illegal<'s>(_: &Dom, s: &mut Sc<'s, '_>, _: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    Err(s.type_error("Illegal constructor"))
}

/// The digest of `data` for an algorithm name. The name is ASCII case-insensitive. `None` for a name we do not hash.
fn digest_bytes(name: &str, data: &[u8]) -> Option<Vec<u8>> {
    Some(match name.to_ascii_uppercase().as_str() {
        "SHA-1" => bare_tls::sha1::sha1(data).to_vec(),
        "SHA-256" => bare_tls::sha256::sha256(data).to_vec(),
        "SHA-384" => bare_tls::sha384::sha384(data).to_vec(),
        "SHA-512" => bare_tls::sha512::sha512(data).to_vec(),
        _ => return None,
    })
}

/// `digest(algorithm, data)`: converts both arguments first (a TypeError rejects), then looks up the name (`NotSupportedError`).
fn subtle_digest<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let p = s.new_promise()?;
    let r = (|| -> Res<'s, Local<'s>> {
        subtle_this(d, s, this)?;
        need(s, a, 2, "SubtleCrypto.digest")?;
        let alg = arg_s(s, a, 0);
        let name = if is_object(s, alg) {
            let n = s.get_str(alg, "name")?;
            if s.type_of(n) == bare_js_api::Type::Undefined {
                return Err(s.type_error("Failed to execute 'digest' on 'SubtleCrypto': Algorithm: Missing 'name'."));
            }
            usv(d, s, n)?
        } else {
            usv(d, s, alg)?
        };
        let data = arg_s(s, a, 1);
        let Some(data) = s.bytes(data).map(<[u8]>::to_vec) else { return Err(s.type_error("Failed to execute 'digest' on 'SubtleCrypto': The provided value is not of type '(ArrayBuffer or ArrayBufferView)'.")) };
        s.charge((data.len() / 64) as u64)?;
        match digest_bytes(&name, &data) {
            Some(h) => s.new_array_buffer(h),
            None => Err(d.dom_err_s(s, "NotSupportedError", "Unrecognized algorithm name.")),
        }
    })();
    settle(s, p, r)
}

/// The methods that are not built yet. Each returns a Promise rejected with `NotSupportedError`.
fn subtle_unsupported<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let p = s.new_promise()?;
    let r = (|| -> Res<'s, Local<'s>> {
        subtle_this(d, s, this)?;
        Err(d.dom_err_s(s, "NotSupportedError", "This SubtleCrypto method is not supported yet."))
    })();
    settle(s, p, r)
}

// ------------------------------------------------------------------------------------------ navigator

fn nav<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>) -> Res<'s, Made> {
    match (obj_id(s, this), d.web.s.borrow().made) {
        (Some(o), Some(m)) if o == m.glob[1] => Ok(m),
        _ => Err(illegal(s)),
    }
}

pub(crate) fn nav_this(d: &Dom, it: &mut Interp<'_>, this: Value) -> R<()> {
    match (this, d.web.s.borrow().made) {
        (Value::Object(o), Some(m)) if o == m.glob[1] => Ok(()),
        _ => Err(it.type_error("Illegal invocation")),
    }
}

fn uad<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>) -> Res<'s, Made> {
    match (obj_id(s, this), d.web.s.borrow().made) {
        (Some(o), Some(m)) if o == m.uad => Ok(m),
        _ => Err(illegal(s)),
    }
}

fn nav_user_agent<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    nav(d, s, this)?;
    d.js_str_s(s, USER_AGENT)
}

fn nav_uad<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let m = nav(d, s, this)?;
    Ok(d.obj_local(s, m.uad))
}

fn nav_language<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    nav(d, s, this)?;
    d.js_str_s(s, "en-US")
}

fn nav_languages<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let m = nav(d, s, this)?;
    Ok(d.obj_local(s, m.langs))
}

fn nav_platform<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    nav(d, s, this)?;
    d.js_str_s(s, "Linux x86_64")
}

fn nav_on_line<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    nav(d, s, this)?;
    Ok(s.boolean(true))
}

fn nav_cookie_enabled<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    nav(d, s, this)?;
    Ok(s.boolean(true))
}

fn nav_cores<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    nav(d, s, this)?;
    Ok(s.number(std::thread::available_parallelism().map_or(1, |n| n.get()) as f64))
}

fn uad_brands<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let m = uad(d, s, this)?;
    Ok(d.obj_local(s, m.brands))
}

fn uad_mobile<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    uad(d, s, this)?;
    Ok(s.boolean(false))
}

fn uad_platform<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    uad(d, s, this)?;
    d.js_str_s(s, PLATFORM)
}

fn uad_to_json<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let m = uad(d, s, this)?;
    low_entropy(d, s, m)
}

/// A new object with `brands`, `mobile` and `platform`: the low entropy hints.
fn low_entropy<'s>(d: &Dom, s: &mut Sc<'s, '_>, m: Made) -> Res<'s, Local<'s>> {
    let op = s.engine().object_prototype();
    let o = d.obj_s(s, op)?;
    let o = d.obj_local(s, o);
    let (b, f, p) = (d.obj_local(s, m.brands), s.boolean(false), d.js_str_s(s, PLATFORM)?);
    for (k, v) in [("brands", b), ("mobile", f), ("platform", p)] {
        s.define_str(o, k, v, Attrs::ALL)?;
    }
    Ok(o)
}

/// `getHighEntropyValues(hints)`: `hints` is any iterable of strings.
fn high_entropy<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let p = s.new_promise()?;
    let r = (|| -> Res<'s, Local<'s>> {
        let m = uad(d, s, this)?;
        need(s, a, 1, "NavigatorUAData.getHighEntropyValues")?;
        let bad = "Failed to execute 'getHighEntropyValues' on 'NavigatorUAData': The provided value cannot be converted to a sequence.";
        let hints = arg_s(s, a, 0);
        let Some(mut iter) = (if is_object(s, hints) { s.iter_open(hints)? } else { None }) else { return Err(s.type_error(bad)) };
        let out = low_entropy(d, s, m)?;
        loop {
            let step = match s.iter_next(&mut iter) {
                Ok(Some(x)) => s.charge(1).and_then(|_| usv(d, s, x)).and_then(|h| hint(d, s, out, &h)).map(|_| true),
                Ok(None) => Ok(false),
                Err(e) => Err(e),
            };
            match step {
                Ok(true) => {}
                Ok(false) => return Ok(out),
                Err(e) => return Err(s.iter_fail(&mut iter, e)),
            }
        }
    })();
    settle(s, p, r)
}

/// One high entropy hint of `getHighEntropyValues`. Unknown names add nothing.
fn hint<'s>(d: &Dom, s: &mut Sc<'s, '_>, out: Local<'s>, name: &str) -> Res<'s, ()> {
    let v = match name {
        "architecture" => d.js_str_s(s, "x86")?,
        "bitness" => d.js_str_s(s, "64")?,
        "model" | "platformVersion" => d.js_str_s(s, "")?,
        "uaFullVersion" => d.js_str_s(s, &format!("{CHROME}.0.0.0"))?,
        "wow64" => s.boolean(false),
        "formFactors" => {
            let f = d.js_str_s(s, "Desktop")?;
            s.new_array(&[f])?
        }
        "fullVersionList" => {
            let mut list = Vec::new();
            for (b, v) in [("Not.A/Brand", "8.0.0.0".to_string()), ("Chromium", format!("{CHROME}.0.0.0")), ("Google Chrome", format!("{CHROME}.0.0.0"))] {
                let op = s.engine().object_prototype();
                let o = d.obj_s(s, op)?;
                let o = d.obj_local(s, o);
                let (bv, vv) = (d.js_str_s(s, b)?, d.js_str_s(s, &v)?);
                s.define_str(o, "brand", bv, Attrs::ALL)?;
                s.define_str(o, "version", vv, Attrs::ALL)?;
                list.push(o);
            }
            s.new_array(&list)?
        }
        _ => return Ok(()),
    };
    s.define_str(out, name, v, Attrs::ALL)
}

// ------------------------------------------------------------------------------------------ object URLs

/// `URL.createObjectURL(blob)`.
#[cold]
#[inline(never)]
pub(crate) fn create_object_url<'s>(d: &Dom, s: &mut Sc<'s, '_>, _: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    need(s, a, 1, "createObjectURL' on 'URL")?;
    let v = arg_s(s, a, 0);
    let blob = match obj_id(s, v) {
        Some(o) if d.web.s.borrow().blobs.contains_key(&o) => o,
        _ => return Err(s.type_error("Failed to execute 'createObjectURL' on 'URL': Overload resolution failed.")),
    };
    if d.web.s.borrow().urls.len() >= MAX_URLS {
        return Err(d.lim_s(s, "web-objects"));
    }
    let Some(id) = uuid_v4() else { return Err(random_failed(d, s)) };
    let origin = d.cur_rec().map_or_else(|_| "null".to_string(), |r| r.origin());
    let url = format!("blob:{origin}/{id}");
    d.web.s.borrow_mut().urls.insert(url.clone(), blob);
    d.js_str_s(s, &url)
}

/// `URL.revokeObjectURL(url)`. A URL that is not in the store is ignored.
#[cold]
#[inline(never)]
pub(crate) fn revoke_object_url<'s>(d: &Dom, s: &mut Sc<'s, '_>, _: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    need(s, a, 1, "revokeObjectURL' on 'URL")?;
    let v = arg_s(s, a, 0);
    let url = d.string_arg_s(s, v)?;
    d.web.s.borrow_mut().urls.remove(url.trim_matches(|c: char| c <= ' ').split('#').next().unwrap_or(""));
    Ok(s.undefined())
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

fn new_blob_s<'s>(d: &Dom, s: &mut Sc<'s, '_>, proto: ObjId, b: BlobData) -> Res<'s, Local<'s>> {
    let n = b.bytes.len();
    lift(s, room(d, n))?;
    s.charge((n / 64) as u64)?;
    let o = d.obj_s(s, proto)?;
    let mut w = d.web.s.borrow_mut();
    w.bytes += n;
    w.blobs.insert(o, b);
    drop(w);
    Ok(d.obj_local(s, o))
}

fn new_blob(d: &Dom, it: &mut Interp<'_>, proto: ObjId, b: BlobData) -> V {
    run_v(it, |s| new_blob_s(d, s, proto, b))
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

fn blob_of<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>) -> Res<'s, ObjId> {
    match obj_id(s, this) {
        Some(o) if d.web.s.borrow().blobs.contains_key(&o) => Ok(o),
        _ => Err(illegal(s)),
    }
}

fn file_of<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>) -> Res<'s, (String, f64)> {
    let o = blob_of(d, s, this)?;
    let f = d.web.s.borrow().blobs[&o].file.clone();
    f.ok_or_else(|| illegal(s))
}

/// The type of a Blob: the empty string when it has a character outside U+0020 to U+007E, else in lower case.
fn blob_type(t: String) -> String {
    if t.bytes().all(|b| (0x20..=0x7e).contains(&b)) { t.to_ascii_lowercase() } else { String::new() }
}

fn add_part<'s>(d: &Dom, s: &mut Sc<'s, '_>, out: &mut Vec<u8>, x: Local<'s>) -> Res<'s, ()> {
    let own = obj_id(s, x).and_then(|o| d.web.s.borrow().blobs.get(&o).map(|b| b.bytes.len()));
    let n = own.or_else(|| s.bytes(x).map(<[u8]>::len));
    if let Some(n) = n {
        if out.len().saturating_add(n) > MAX_BLOB {
            return Err(d.lim_s(s, "blob-bytes"));
        }
        match obj_id(s, x) {
            Some(o) if own.is_some() => out.extend_from_slice(&d.web.s.borrow().blobs[&o].bytes),
            _ => out.extend_from_slice(s.bytes(x).unwrap_or_default()),
        }
        return Ok(());
    }
    let text = usv(d, s, x)?;
    if out.len().saturating_add(text.len()) > MAX_BLOB {
        return Err(d.lim_s(s, "blob-bytes"));
    }
    out.extend_from_slice(text.as_bytes());
    Ok(())
}

fn parts<'s>(d: &Dom, s: &mut Sc<'s, '_>, v: Local<'s>, who: &str) -> Res<'s, Vec<u8>> {
    let bad = format!("Failed to construct '{who}': The provided value cannot be converted to a sequence.");
    if !is_object(s, v) {
        return Err(s.type_error(&bad));
    }
    let Some(mut iter) = s.iter_open(v)? else { return Err(s.type_error(&bad)) };
    let mut out = Vec::new();
    loop {
        let step = match s.iter_next(&mut iter) {
            Ok(Some(x)) => add_part(d, s, &mut out, x).map(|_| true),
            Ok(None) => Ok(false),
            Err(e) => Err(e),
        };
        match step {
            Ok(true) => {}
            Ok(false) => return Ok(out),
            Err(e) => return Err(s.iter_fail(&mut iter, e)),
        }
    }
}

fn options<'s>(d: &Dom, s: &mut Sc<'s, '_>, v: Local<'s>, who: &str, file: bool) -> Res<'s, (String, Option<f64>)> {
    if is_nullish(s, v) {
        return Ok((String::new(), None));
    }
    if !is_object(s, v) {
        return Err(s.type_error(&format!("Failed to construct '{who}': The provided value is not of type '{who}PropertyBag'.")));
    }
    let t = s.get_str(v, "type")?;
    let ty = if s.type_of(t) == Type::Undefined { String::new() } else { blob_type(usv(d, s, t)?) };
    let mut modified = None;
    if file {
        let m = s.get_str(v, "lastModified")?;
        if s.type_of(m) != Type::Undefined {
            let n = s.to_number(m)?;
            if !n.is_finite() {
                return Err(s.type_error("Failed to construct 'File': The provided value is not finite."));
            }
            modified = Some(n.trunc());
        }
    }
    Ok((ty, modified))
}

fn blob_ctor<'s>(d: &Dom, s: &mut Sc<'s, '_>, _: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let intrinsic = d.web.s.borrow().made.expect("made").blob_p;
    let intrinsic = d.obj_local(s, intrinsic);
    let proto = s.proto_from_new_target(intrinsic)?;
    let proto = lid(s, proto);
    let v = arg_s(s, a, 0);
    let bytes = if s.type_of(v) == Type::Undefined { Vec::new() } else { parts(d, s, v, "Blob")? };
    let o = arg_s(s, a, 1);
    let (ty, _) = options(d, s, o, "Blob", false)?;
    new_blob_s(d, s, proto, BlobData { bytes, ty, file: None })
}

fn file_ctor<'s>(d: &Dom, s: &mut Sc<'s, '_>, _: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let intrinsic = d.web.s.borrow().made.expect("made").file_p;
    let intrinsic = d.obj_local(s, intrinsic);
    let proto = s.proto_from_new_target(intrinsic)?;
    let proto = lid(s, proto);
    need(s, a, 2, "construct 'File")?;
    let (v, n, o) = (arg_s(s, a, 0), arg_s(s, a, 1), arg_s(s, a, 2));
    let bytes = parts(d, s, v, "File")?;
    let name = usv(d, s, n)?;
    let (ty, modified) = options(d, s, o, "File", true)?;
    let modified = modified.unwrap_or_else(|| s.wall_now_ms());
    new_blob_s(d, s, proto, BlobData { bytes, ty, file: Some((name, modified)) })
}

fn offset<'s>(s: &mut Sc<'s, '_>, v: Local<'s>, size: usize, default: usize) -> Res<'s, usize> {
    if s.type_of(v) == Type::Undefined {
        return Ok(default);
    }
    let n = s.to_number(v)?;
    let n = if n.is_nan() { 0.0 } else { n.trunc() };
    Ok(if n < 0.0 { (size as f64 + n).max(0.0) } else { n.min(size as f64) } as usize)
}

fn blob_slice<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let o = blob_of(d, s, this)?;
    let size = d.web.s.borrow().blobs[&o].bytes.len();
    let (x0, x1, x2) = (arg_s(s, a, 0), arg_s(s, a, 1), arg_s(s, a, 2));
    let start = offset(s, x0, size, 0)?;
    let end = offset(s, x1, size, size)?;
    let ty = if s.type_of(x2) == Type::Undefined { String::new() } else { blob_type(usv(d, s, x2)?) };
    let bytes = d.web.s.borrow().blobs[&o].bytes.get(start..end.max(start)).unwrap_or_default().to_vec();
    let proto = d.web.s.borrow().made.expect("made").blob_p;
    new_blob_s(d, s, proto, BlobData { bytes, ty, file: None })
}

fn blob_text<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let p = s.new_promise()?;
    let r = (|| -> Res<'s, Local<'s>> {
        let o = blob_of(d, s, this)?;
        let text = {
            let w = d.web.s.borrow();
            let b = &w.blobs[&o].bytes;
            String::from_utf8_lossy(b.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(b)).into_owned()
        };
        s.charge((text.len() / 16) as u64)?;
        d.js_str_s(s, &text)
    })();
    settle(s, p, r)
}

fn blob_array_buffer<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let p = s.new_promise()?;
    let r = (|| -> Res<'s, Local<'s>> {
        let o = blob_of(d, s, this)?;
        let bytes = d.web.s.borrow().blobs[&o].bytes.clone();
        s.charge((bytes.len() / 64) as u64)?;
        s.new_array_buffer(bytes)
    })();
    settle(s, p, r)
}

fn blob_stream<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let o = blob_of(d, s, this)?;
    let bytes = d.web.s.borrow().blobs[&o].bytes.clone();
    old_v(s, |it| crate::browser_fetch::bytes_stream(d, it, bytes))
}

fn blob_size<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let o = blob_of(d, s, this)?;
    let n = d.web.s.borrow().blobs[&o].bytes.len();
    Ok(s.number(n as f64))
}

fn blob_type_get<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let o = blob_of(d, s, this)?;
    let ty = d.web.s.borrow().blobs[&o].ty.clone();
    d.js_str_s(s, &ty)
}

fn file_name_get<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let (n, _) = file_of(d, s, this)?;
    d.js_str_s(s, &n)
}

fn file_modified<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let (_, m) = file_of(d, s, this)?;
    Ok(s.number(m))
}

// ------------------------------------------------------------------------------------------ FormData

fn cost(n: &str, e: &Ent) -> usize {
    n.len() + 32 + if let Ent::S(v) = e { v.len() } else { 0 }
}

fn form_ctor<'s>(d: &Dom, s: &mut Sc<'s, '_>, _: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let intrinsic = d.web.s.borrow().made.expect("made").form_p;
    let intrinsic = d.obj_local(s, intrinsic);
    let proto = s.proto_from_new_target(intrinsic)?;
    let proto = lid(s, proto);
    let v = arg_s(s, a, 0);
    let form = if s.type_of(v) == Type::Undefined {
        None
    } else {
        match d.node_of_s(s, v).filter(|&n| d.doc.borrow().is_html(n, "form")) {
            Some(n) => Some(n),
            None => return Err(s.type_error("Failed to construct 'FormData': parameter 1 is not of type 'HTMLFormElement'.")),
        }
    };
    lift(s, room(d, 0))?;
    // The entries first: the form walk can fail, and then no FormData exists.
    let list = match form {
        Some(f) => old(s, |it| d.entry_list(it, f, None))?,
        None => Vec::new(),
    };
    fill_form(d, s, proto, list)
}

pub(crate) fn form_from_entries(d: &Dom, it: &mut Interp<'_>, list: Vec<(String, Option<Ent>)>) -> V {
    let m = made(d, it)?;
    run_v(it, |s| fill_form(d, s, m.form_p, list))
}

fn fill_form<'s>(d: &Dom, s: &mut Sc<'s, '_>, proto: ObjId, list: Vec<(String, Option<Ent>)>) -> Res<'s, Local<'s>> {
    let o = d.obj_s(s, proto)?;
    d.web.s.borrow_mut().forms.insert(o, Form::default());
    d.web.active.set(true);
    for (n, v) in list {
        let e = match v {
            Some(e) => e,
            None => {
                let fp = d.web.s.borrow().made.expect("made").file_p;
                let now = s.wall_now_ms();
                let f = new_blob_s(d, s, fp, BlobData { bytes: Vec::new(), ty: "application/octet-stream".into(), file: Some((String::new(), now)) })?;
                Ent::B(lid(s, f))
            }
        };
        lift(s, push(d, o, n, e))?;
    }
    Ok(d.obj_local(s, o))
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

fn form_of<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>) -> Res<'s, ObjId> {
    match obj_id(s, this) {
        Some(o) if d.web.s.borrow().forms.contains_key(&o) => Ok(o),
        _ => Err(illegal(s)),
    }
}

fn entry<'s>(d: &Dom, s: &mut Sc<'s, '_>, a: Args<'s>, what: &str) -> Res<'s, Ent> {
    let x = arg_s(s, a, 1);
    let blob = obj_id(s, x).filter(|o| d.web.s.borrow().blobs.contains_key(o));
    let Some(o) = blob else {
        if a.len() > 2 {
            return Err(s.type_error(&format!("Failed to execute '{what}' on 'FormData': parameter 2 is not of type 'Blob'.")));
        }
        return Ok(Ent::S(usv(d, s, x)?));
    };
    let f = arg_s(s, a, 2);
    let given = if s.type_of(f) == Type::Undefined { None } else { Some(usv(d, s, f)?) };
    let (file, bytes, ty) = {
        let w = d.web.s.borrow();
        let b = &w.blobs[&o];
        if b.file.is_some() && given.is_none() {
            return Ok(Ent::B(o));
        }
        (b.file.clone(), b.bytes.clone(), b.ty.clone())
    };
    let (name, modified) = file.unzip();
    let file = (given.or(name).unwrap_or_else(|| "blob".into()), modified.unwrap_or_else(|| s.wall_now_ms()));
    let proto = d.web.s.borrow().made.expect("made").file_p;
    let f = new_blob_s(d, s, proto, BlobData { bytes, ty, file: Some(file) })?;
    Ok(Ent::B(lid(s, f)))
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

fn f_append<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let f = form_of(d, s, this)?;
    need(s, a, 2, "append' on 'FormData")?;
    let x = arg_s(s, a, 0);
    let n = usv(d, s, x)?;
    let e = entry(d, s, a, "append")?;
    lift(s, push(d, f, n, e))?;
    Ok(s.undefined())
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

fn f_set<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let f = form_of(d, s, this)?;
    need(s, a, 2, "set' on 'FormData")?;
    let x = arg_s(s, a, 0);
    let n = usv(d, s, x)?;
    let e = entry(d, s, a, "set")?;
    let len = d.web.s.borrow().forms[&f].list.len();
    s.charge(len as u64 / 8)?;
    let pos = d.web.s.borrow().forms[&f].list.iter().position(|x| x.0 == n);
    match pos {
        None => lift(s, push(d, f, n, e))?,
        Some(i) => edit(d, f, |l| {
            l[i].1 = e;
            let mut k = 0;
            l.retain(|x| {
                k += 1;
                k - 1 == i || x.0 != n
            });
        }),
    }
    Ok(s.undefined())
}

fn f_delete<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let f = form_of(d, s, this)?;
    need(s, a, 1, "delete' on 'FormData")?;
    let x = arg_s(s, a, 0);
    let n = usv(d, s, x)?;
    edit(d, f, |l| l.retain(|x| x.0 != n));
    Ok(s.undefined())
}

fn ent_value<'s>(d: &Dom, s: &mut Sc<'s, '_>, e: &Ent) -> Res<'s, Local<'s>> {
    match e {
        Ent::S(v) => d.js_str_s(s, v),
        Ent::B(o) => Ok(d.obj_local(s, *o)),
    }
}

pub(crate) fn clone_ent(e: &Ent) -> Ent {
    match e {
        Ent::S(v) => Ent::S(v.clone()),
        Ent::B(o) => Ent::B(*o),
    }
}

fn f_get<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let f = form_of(d, s, this)?;
    need(s, a, 1, "get' on 'FormData")?;
    let x = arg_s(s, a, 0);
    let n = usv(d, s, x)?;
    let found = d.web.s.borrow().forms[&f].list.iter().find(|x| x.0 == n).map(|x| clone_ent(&x.1));
    match found {
        Some(e) => ent_value(d, s, &e),
        None => Ok(s.null()),
    }
}

fn f_get_all<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let f = form_of(d, s, this)?;
    need(s, a, 1, "getAll' on 'FormData")?;
    let x = arg_s(s, a, 0);
    let n = usv(d, s, x)?;
    let found: Vec<Ent> = d.web.s.borrow().forms[&f].list.iter().filter(|x| x.0 == n).map(|x| clone_ent(&x.1)).collect();
    let mut vals = Vec::with_capacity(found.len());
    for e in &found {
        vals.push(ent_value(d, s, e)?);
    }
    s.new_array(&vals)
}

fn f_has<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let f = form_of(d, s, this)?;
    need(s, a, 1, "has' on 'FormData")?;
    let x = arg_s(s, a, 0);
    let n = usv(d, s, x)?;
    let has = d.web.s.borrow().forms[&f].list.iter().any(|x| x.0 == n);
    Ok(s.boolean(has))
}

/// The pair at `i` of form `f`, if both still exist.
fn pair_at(d: &Dom, f: ObjId, i: usize) -> Option<(String, Ent)> {
    d.web.s.borrow().forms.get(&f)?.list.get(i).map(|(n, e)| (n.clone(), clone_ent(e)))
}

fn f_for_each<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let f = form_of(d, s, this)?;
    let (cb, this_arg) = (arg_s(s, a, 0), arg_s(s, a, 1));
    if !s.is_callable(cb) {
        return Err(s.type_error("Failed to execute 'forEach' on 'FormData': parameter 1 is not of type 'Function'."));
    }
    let mut i = 0;
    loop {
        s.charge(1)?;
        let Some((n, e)) = pair_at(d, f, i) else { return Ok(s.undefined()) };
        let (v, n) = (ent_value(d, s, &e)?, d.js_str_s(s, &n)?);
        s.call(cb, this_arg, &[v, n, this])?;
        i += 1;
    }
}

fn make_iter<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, kind: u8) -> Res<'s, Local<'s>> {
    let form = form_of(d, s, this)?;
    lift(s, room(d, 0))?;
    let proto = d.web.s.borrow().made.expect("made").iter_p;
    let o = d.obj_s(s, proto)?;
    d.web.s.borrow_mut().iters.insert(o, IterObj { form, idx: 0, kind });
    d.web.active.set(true);
    Ok(d.obj_local(s, o))
}

fn f_keys<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    make_iter(d, s, this, 0)
}

fn f_values<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    make_iter(d, s, this, 1)
}

fn f_entries<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    make_iter(d, s, this, 2)
}

fn iter_next<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let id = match obj_id(s, this) {
        Some(o) if d.web.s.borrow().iters.contains_key(&o) => o,
        _ => return Err(illegal(s)),
    };
    s.charge(1)?;
    let (form, idx, kind) = {
        let w = d.web.s.borrow();
        let i = &w.iters[&id];
        (i.form, i.idx, i.kind)
    };
    let (value, done) = match pair_at(d, form, idx) {
        None => (s.undefined(), true),
        Some((n, e)) => {
            d.web.s.borrow_mut().iters.get_mut(&id).expect("checked").idx += 1;
            let v = match kind {
                0 => d.js_str_s(s, &n)?,
                1 => ent_value(d, s, &e)?,
                _ => {
                    let (n, v) = (d.js_str_s(s, &n)?, ent_value(d, s, &e)?);
                    s.new_array(&[n, v])?
                }
            };
            (v, false)
        }
    };
    iter_result_s(s, value, done)
}
