//! Page basics: the names that live pages read on `document`, `window` and `navigator` and that bare did not define. A missing name
//! was `undefined`, and code such as Mixpanel's `document.referrer.search(...)` threw at load (wise.com showed its error page).
//!
//! Cost rule: a realm that never reads a name pays almost nothing. Constants are data properties (no function object). A value that
//! depends on the document is a getter. `navigator` is lazy as a whole (`browser_web.rs`), so its names cost a page that never reads
//! `navigator` nothing. Values are Chrome 155 on Linux where the HTML Standard leaves a choice (`docs/decisions.md` lists each one).
//!
//! `document`: `referrer` "" (bare sends no `Referer`), `visibilityState` "visible", `hidden` false, `designMode` "off" (data
//! properties: a write only stores the value, the page has no design mode); `characterSet`, `charset` and `inputEncoding` (one getter:
//! the `<meta charset>` or `<meta http-equiv=content-type>` in `<head>` through `bare_html::encoding`'s labels, else "UTF-8"; the
//! `Content-Type` header is not known to the document), `contentType` "text/html", `compatMode` ("CSS1Compat" with a doctype, else
//! "BackCompat"; the tree builder keeps the doctype name only, so a legacy public id is not seen), `dir` (the `dir` of `<html>`
//! lower-cased when it is `ltr`, `rtl` or `auto`, else ""; the setter writes the attribute), `lastModified` (the load time in UTC as
//! `MM/DD/YYYY hh:mm:ss`; the `Last-Modified` header is not known to the document).
//!
//! `window`: `devicePixelRatio` 1, `screenX` and `screenY` 0, `crossOriginIsolated` false, `closed` false, `opener` and `frameElement`
//! null (data properties); `isSecureContext` (https, or a loopback or `.localhost` host); `outerWidth` and `outerHeight` are the inner
//! size (`browser_scroll.rs`); `alert`, `print`, `focus` and `blur` do nothing; `confirm` gives false; `prompt` gives null; `open` gives
//! null (a blocked popup). `postMessage(message, targetOrigin | { targetOrigin, transfer })` clones the message (`structuredClone`), drops
//! it when `targetOrigin` is not "*", "/" or the origin of the document (an unparsable `targetOrigin` is a `SyntaxError`), and queues a
//! `message` event for the window as one task (`Dom::post_window`). The event has `origin` (the document origin), `source` (the window)
//! and no ports. A transfer list with items is `Unsupported`, as for `MessagePort`.
//!
//! `navigator`: `vendor` "Google Inc.", `maxTouchPoints` 0, `webdriver` false, `pdfViewerEnabled` true, `appName` "Netscape", `appVersion`
//! (the user agent without "Mozilla/"), `product` "Gecko", `productSub` "20030107", `vendorSub` "", `doNotTrack` null, `javaEnabled()`
//! false, `plugins` and `mimeTypes` (the five PDF viewers and two PDF types of Chrome; made on the first read, then plain data), and
//! `sendBeacon(url, data)`: a `POST` with credentials through the queue of `fetch`. It returns false, and sends nothing, when the body
//! would take the bytes that wait in the queue over 64 KiB (the keepalive quota of the Fetch Standard: the bytes of requests in flight; a
//! request here is in flight from the call until the network pump sends it). A URL that does not resolve, or is not http(s), is a
//! `TypeError`. The reply is ignored and a failure is silent.

use crate::browser::*;
use crate::browser_platform::arg;
use bare_html::dom::{Document, NodeData};
use bare_js::eval::Interp;
use bare_js::value::Value;
use std::cell::Cell;

/// The keepalive quota: bytes of request bodies in flight (Fetch Standard, "keepalive").
const QUOTA: usize = 65536;

/// The cost that a queued window message counts against the message caps (the clone has no byte count).
pub(crate) const MESSAGE_COST: usize = 1024;

#[derive(Default)]
pub(crate) struct Basics {
    /// The wall clock when the realm was made: `lastModified`.
    loaded: Cell<f64>,
    /// The callback that makes a beacon promise handled. Made by the first beacon, then kept in `Dom::fixed`.
    noop: Cell<Option<bare_js::value::ObjId>>,
}

pub(crate) fn install(d: &Dom, it: &mut Interp<'_>) -> R<()> {
    d.basics.loaded.set(it.wall_now_ms());
    Ok(())
}

/// The names of `window` and of `Document.prototype` made here. The heap asks the host for them on the first miss (`Heap::browser_name`),
/// so a realm that never reads one pays nothing.
const WINDOW: [&str; 18] = ["devicePixelRatio", "screenX", "screenY", "crossOriginIsolated", "closed", "opener", "frameElement", "postMessage", "alert", "print", "focus", "blur", "confirm", "prompt", "open", "isSecureContext", "outerWidth", "outerHeight"];
const DOCUMENT: [&str; 11] = ["hidden", "referrer", "visibilityState", "contentType", "compatMode", "characterSet", "charset", "inputEncoding", "lastModified", "designMode", "dir"];

/// The ask of the heap for `key` (`browser_handlers::lazy`): define it when it is one of the names above. The name `open` is also a
/// method of `document` (`browser_loader.rs`), so it is defined here and the caller goes on.
pub(crate) fn define(d: &Dom, it: &mut Interp<'_>, key: &str) -> R<()> {
    let (g, doc) = (*d.global.get().expect("installed"), d.p().document);
    let data = |it: &mut Interp<'_>, o, v| -> R<()> {
        let k = d.key(it, key)?;
        it.define_data(o, k, v, true, true, true)
    };
    let hidden = |it: &mut Interp<'_>, h: H| -> R<()> {
        let f = d.add_fn(it, h, key, 0, false)?;
        let k = d.key(it, key)?;
        it.define_data(g, k, Value::Object(f), true, false, true)
    };
    match key {
        "devicePixelRatio" => data(it, g, Value::Number(1.0)),
        "screenX" | "screenY" => data(it, g, Value::Number(0.0)),
        "crossOriginIsolated" | "closed" => data(it, g, Value::Bool(false)),
        "opener" | "frameElement" => data(it, g, Value::Null),
        "postMessage" => d.def_method(it, g, "postMessage", 1, post_message).map(|_| ()),
        "alert" | "print" | "focus" | "blur" => hidden(it, nothing),
        "confirm" => hidden(it, no),
        "prompt" | "open" => hidden(it, null),
        "isSecureContext" => getter(d, it, g, &["isSecureContext"], |d, _, _, _| Ok(Value::Bool(secure(d))), false),
        "outerWidth" => getter(d, it, g, &["outerWidth"], crate::browser_scroll::win_inner_width, false),
        "outerHeight" => getter(d, it, g, &["outerHeight"], crate::browser_scroll::win_inner_height, false),
        "hidden" => data(it, doc, Value::Bool(false)),
        "referrer" => {
            let empty = d.js_str(it, "")?;
            data(it, doc, empty)
        }
        "visibilityState" => getter(d, it, doc, &[key], |d, it, _, _| d.js_str(it, "visible"), false),
        "contentType" => getter(d, it, doc, &[key], |d, it, _, _| d.js_str(it, "text/html"), false),
        "compatMode" => getter(d, it, doc, &[key], |d, it, _, _| d.js_str(it, if has_doctype(&d.doc.borrow()) { "CSS1Compat" } else { "BackCompat" }), false),
        "characterSet" | "charset" | "inputEncoding" => getter(d, it, doc, &[key], |d, it, _, _| d.js_str(it, charset(&d.doc.borrow())), false),
        "lastModified" => getter(d, it, doc, &[key], |d, it, _, _| d.js_str(it, &last_modified(d.basics.loaded.get())), false),
        // There is no design mode: the setter keeps nothing.
        "designMode" => getter(d, it, doc, &[key], |d, it, _, a| if a.is_empty() { d.js_str(it, "off") } else { Ok(Value::Undefined) }, true),
        "dir" => getter(d, it, doc, &[key], dir, true),
        _ => Ok(()),
    }
}

/// Every name of `owner` (`Object.keys(window)` and the like ask for all names of an owner).
pub(crate) fn define_all(d: &Dom, it: &mut Interp<'_>, owner: bare_js::value::ObjId) -> R<()> {
    let names: &[&str] = if owner == *d.global.get().expect("installed") { &WINDOW } else if owner == d.p().document { &DOCUMENT } else { return Ok(()) };
    for n in names.iter() {
        let k = d.key(it, n)?;
        if !it.heap.has_own_property(owner, k).map_err(|e| it.heap_error(e))? {
            define(d, it, n)?;
        }
    }
    Ok(())
}

/// An accessor of `target` for each of `names`, all with one function (named as the first key, which the heap has already). With
/// `set` the same function is the setter: it runs as one when it gets an argument.
pub(crate) fn getter(d: &Dom, it: &mut Interp<'_>, target: bare_js::value::ObjId, names: &[&str], h: H, set: bool) -> R<()> {
    let f = d.add_fn(it, h, names[0], 0, false)?;
    for name in names {
        let k = d.key(it, name)?;
        d.define_accessor(it, target, k, f, set.then_some(f))?;
    }
    Ok(())
}

fn nothing(_: &Dom, _: &mut Interp<'_>, _: Value, _: &[Value]) -> R<Value> {
    Ok(Value::Undefined)
}

fn no(_: &Dom, _: &mut Interp<'_>, _: Value, _: &[Value]) -> R<Value> {
    Ok(Value::Bool(false))
}

fn null(_: &Dom, _: &mut Interp<'_>, _: Value, _: &[Value]) -> R<Value> {
    Ok(Value::Null)
}

// ---------------------------------------------------------------------------------------------- document

fn html_of(doc: &Document) -> Option<bare_html::dom::NodeId> {
    doc.children(doc.root()).find(|&n| matches!(doc.data(n), NodeData::Element { .. }))
}

fn has_doctype(doc: &Document) -> bool {
    doc.children(doc.root()).any(|n| matches!(doc.data(n), NodeData::Doctype(_)))
}

/// The encoding label of the first `<meta charset>` or `<meta http-equiv=content-type content="...charset=...">` of `<head>`, as the name
/// of an encoding that `bare_html::encoding` reads: "windows-1252" for the Latin-1 labels, else "UTF-8".
fn charset(doc: &Document) -> &'static str {
    let Some(html) = html_of(doc) else { return "UTF-8" };
    let Some(head) = doc.children(html).find(|&c| doc.is_html(c, "head")) else { return "UTF-8" };
    // An atom that no element uses is not in the document, so no attribute has that name.
    let (cs, he, ct) = (doc.find_atom("charset"), doc.find_atom("http-equiv"), doc.find_atom("content"));
    let get = |m, a: Option<bare_html::dom::Atom>| a.and_then(|a| doc.attr(m, a));
    for m in doc.children(head).filter(|&c| doc.is_html(c, "meta")).take(64) {
        let label = match (get(m, cs), get(m, he), get(m, ct)) {
            (Some(l), ..) => l,
            (None, Some(h), Some(c)) if h.eq_ignore_ascii_case("content-type") && c.to_ascii_lowercase().contains("charset=") => c.rsplit_once('=').map_or("", |(_, l)| l),
            _ => continue,
        };
        let l = label.trim().trim_matches(['"', '\'', ';']).to_ascii_lowercase();
        return match l.as_str() {
            "iso-8859-1" | "latin1" | "windows-1252" | "ascii" | "us-ascii" | "l1" | "cp1252" | "iso8859-1" | "iso_8859-1" => "windows-1252",
            _ => "UTF-8",
        };
    }
    "UTF-8"
}

/// `document.dir`: the `dir` of `<html>` when it is `ltr`, `rtl` or `auto` (any case), else "". The setter writes the attribute.
fn dir(d: &Dom, it: &mut Interp<'_>, _: Value, a: &[Value]) -> R<Value> {
    if let Some(&v) = a.first() {
        let s = it.to_js_string(v)?;
        let v = it.heap.to_string_lossy(s).map_err(|e| it.heap_error(e))?;
        let html = html_of(&d.doc.borrow());
        if let Some(h) = html {
            crate::browser_dom::attr_set(d, it, h, "dir", &v)?;
        }
        return Ok(Value::Undefined);
    }
    let v = {
        let doc = d.doc.borrow();
        html_of(&doc).and_then(|h| doc.find_atom("dir").and_then(|a| doc.attr(h, a))).map(str::to_ascii_lowercase).filter(|v| matches!(v.as_str(), "ltr" | "rtl" | "auto")).unwrap_or_default()
    };
    d.js_str(it, &v)
}

/// `MM/DD/YYYY hh:mm:ss` in UTC of `ms` since the epoch (Howard Hinnant's `civil_from_days`).
fn last_modified(ms: f64) -> String {
    let secs = (ms / 1000.0).floor() as i64;
    let (days, rem) = (secs.div_euclid(86400), secs.rem_euclid(86400));
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z.rem_euclid(146097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + (month <= 2) as i64;
    format!("{month:02}/{day:02}/{year:04} {:02}:{:02}:{:02}", rem / 3600, rem % 3600 / 60, rem % 60)
}

// ---------------------------------------------------------------------------------------------- window

/// Is the document a secure context? https, or a loopback host (Secure Contexts, "potentially trustworthy origin").
fn secure(d: &Dom) -> bool {
    d.fetch_base().is_some_and(|u| u.scheme == bare_url::Scheme::Https || crate::browser_fetch::potentially_trustworthy(&u))
}

/// The origin of the document, serialized: "null" while no URL is configured.
fn doc_origin(d: &Dom) -> String {
    let nav = d.nav.s.borrow();
    if let Some(e) = nav.entries.get(nav.cur) {
        return e.url.origin();
    }
    drop(nav);
    d.fetch_base().and_then(|u| crate::browser_navigation_url::parse(&u.to_string(), None).ok()).map_or_else(|| "null".into(), |r| r.origin())
}

impl Dom {
    /// The origin that a `message` event of the window reports.
    pub(crate) fn window_origin(&self) -> String {
        doc_origin(self)
    }
}

fn post_message(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> R<Value> {
    if !matches!(this, Value::Undefined | Value::Null) && !matches!(this, Value::Object(o) if Some(&o) == d.global.get()) {
        return Err(it.type_error("Illegal invocation"));
    }
    if a.is_empty() {
        return Err(it.type_error("Failed to execute 'postMessage' on 'Window': 1 argument required, but only 0 present."));
    }
    // `postMessage(message, targetOrigin, transfer)` or `postMessage(message, { targetOrigin, transfer })`.
    let (target, transfer) = match arg(a, 1) {
        Value::Undefined | Value::Null => (Value::Undefined, Value::Undefined),
        o @ Value::Object(_) => {
            let (kt, kr) = (d.key(it, "targetOrigin")?, d.key(it, "transfer")?);
            (it.get_value(o, kt)?, it.get_value(o, kr)?)
        }
        v => (v, arg(a, 2)),
    };
    let target = match target {
        Value::Undefined => "/".to_string(),
        v => {
            let s = it.to_js_string(v)?;
            it.heap.to_string_lossy(s).map_err(|e| it.heap_error(e))?
        }
    };
    d.no_transfer(it, transfer)?;
    if target != "*" && target != "/" {
        let Ok(r) = crate::browser_navigation_url::parse(&target, None) else {
            return Err(d.dom_err(it, "SyntaxError", &format!("Failed to execute 'postMessage' on 'Window': Invalid target origin '{target}' in a call to 'postMessage'.")));
        };
        if r.origin() != doc_origin(d) {
            return Ok(Value::Undefined);
        }
    }
    let data = crate::browser_navigation_state::clone(d, it, a[0])?;
    d.post_window(data)?;
    Ok(Value::Undefined)
}

// ---------------------------------------------------------------------------------------------- navigator

/// The names of `navigator` that this file adds, on its prototype `np`. `this` is checked with `check` (the one of `browser_web.rs`).
pub(crate) fn navigator(d: &Dom, it: &mut Interp<'_>, np: bare_js::value::ObjId) -> R<()> {
    // Read-only data properties, not getters: a write fails as it does for an accessor with no setter.
    let strings = [("vendor", "Google Inc."), ("appName", "Netscape"), ("appVersion", crate::browser_web::USER_AGENT.trim_start_matches("Mozilla/")), ("product", "Gecko"), ("productSub", "20030107"), ("vendorSub", "")];
    for (name, v) in strings {
        let (k, v) = (d.key(it, name)?, d.js_str(it, v)?);
        it.define_data(np, k, v, false, true, true)?;
    }
    for (name, v) in [("maxTouchPoints", Value::Number(0.0)), ("webdriver", Value::Bool(false)), ("pdfViewerEnabled", Value::Bool(true)), ("doNotTrack", Value::Null)] {
        let k = d.key(it, name)?;
        it.define_data(np, k, v, false, true, true)?;
    }
    d.def_get(it, np, "plugins", |d, it, t, _| plugins(d, it, t, 0), None)?;
    d.def_get(it, np, "mimeTypes", |d, it, t, _| plugins(d, it, t, 1), None)?;
    d.def_method(it, np, "javaEnabled", 0, |d, it, t, _| crate::browser_web::nav_this(d, it, t).map(|_| Value::Bool(false)))?;
    d.def_method(it, np, "sendBeacon", 1, send_beacon)?;
    Ok(())
}

const PLUGINS: [&str; 5] = ["PDF Viewer", "Chrome PDF Viewer", "Chromium PDF Viewer", "Microsoft Edge PDF Viewer", "WebKit built-in PDF"];
const MIMES: [(&str, &str); 2] = [("application/pdf", "Portable Document Format"), ("text/pdf", "Portable Document Format")];

/// `navigator.plugins` (`which` 0) or `navigator.mimeTypes` (1): both arrays are made on the first read of either and become own data
/// properties of `navigator`, so the second read is the same object.
fn plugins(d: &Dom, it: &mut Interp<'_>, this: Value, which: usize) -> R<Value> {
    crate::browser_web::nav_this(d, it, this)?;
    let Value::Object(nav) = this else { unreachable!() };
    let op = it.object_prototype();
    let mk = |it: &mut Interp<'_>, props: &[(&str, Value)]| -> R<bare_js::value::ObjId> {
        let o = d.obj(it, op)?;
        for &(k, v) in props {
            let k = d.key(it, k)?;
            it.define_data(o, k, v, false, true, true)?;
        }
        Ok(o)
    };
    let mut mimes = Vec::new();
    for (t, desc) in MIMES {
        let (t, desc, sfx) = (d.js_str(it, t)?, d.js_str(it, desc)?, d.js_str(it, "pdf")?);
        mimes.push(Value::Object(mk(it, &[("type", t), ("suffixes", sfx), ("description", desc)])?));
    }
    let mut plugs = Vec::new();
    for name in PLUGINS {
        let (n, f, desc) = (d.js_str(it, name)?, d.js_str(it, "internal-pdf-viewer")?, d.js_str(it, "Portable Document Format")?);
        let p = mk(it, &[("name", n), ("filename", f), ("description", desc), ("length", Value::Number(MIMES.len() as f64))])?;
        for (i, &m) in mimes.iter().enumerate() {
            let k = d.key(it, &i.to_string())?;
            it.define_data(p, k, m, false, true, true)?;
        }
        plugs.push(Value::Object(p));
    }
    for &m in &mimes {
        let (k, v) = (d.key(it, "enabledPlugin")?, plugs[0]);
        let Value::Object(m) = m else { unreachable!() };
        it.define_data(m, k, v, false, true, true)?;
    }
    let (plugs, mimes) = (it.array_from(&plugs)?, it.array_from(&mimes)?);
    for (name, v) in [("plugins", plugs), ("mimeTypes", mimes)] {
        let k = d.key(it, name)?;
        it.define_data(nav, k, v, true, true, true)?;
    }
    Ok([plugs, mimes][which])
}

fn send_beacon(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> R<Value> {
    crate::browser_web::nav_this(d, it, this)?;
    if a.is_empty() {
        return Err(it.type_error("Failed to execute 'sendBeacon' on 'Navigator': 1 argument required, but only 0 present."));
    }
    let s = it.to_js_string(a[0])?;
    let url = it.heap.to_string_lossy(s).map_err(|e| it.heap_error(e))?;
    if let Some(base) = d.fetch_base() {
        match base.join(&url) {
            Ok(u) if matches!(u.scheme, bare_url::Scheme::Http | bare_url::Scheme::Https) => {}
            Ok(_) => return Err(it.type_error("Failed to execute 'sendBeacon' on 'Navigator': Beacons are only supported over HTTP(S).")),
            Err(_) => return Err(it.type_error(&format!("Failed to execute 'sendBeacon' on 'Navigator': The URL '{url}' is invalid."))),
        }
    }
    let data = arg(a, 1);
    let n = match data {
        Value::Undefined | Value::Null => 0,
        Value::Object(_) if d.web_body_len(data).is_some() => d.web_body_len(data).unwrap_or(0),
        Value::Object(o) if it.heap.bytes(o).is_some() => it.heap.bytes(o).map_or(0, <[u8]>::len),
        v => {
            let s = it.to_js_string(v)?;
            it.heap.to_string_lossy(s).map_err(|e| it.heap_error(e))?.len()
        }
    };
    if d.queued_body_bytes().saturating_add(n) > QUOTA {
        return Ok(Value::Bool(false));
    }
    let init = d.obj(it, it.object_prototype())?;
    for (k, v) in [("method", d.js_str(it, "POST")?), ("credentials", d.js_str(it, "include")?), ("keepalive", Value::Bool(true))] {
        let k = d.key(it, k)?;
        it.define_data(init, k, v, true, true, true)?;
    }
    if n > 0 || !matches!(data, Value::Undefined | Value::Null) {
        let k = d.key(it, "body")?;
        it.define_data(init, k, data, true, true, true)?;
    }
    // Make the promise handled: a beacon has no reply, so a failure is silent.
    let p = crate::browser_fetch::fetch(d, it, Value::Undefined, &[a[0], Value::Object(init)])?;
    let noop = match d.basics.noop.get() {
        Some(f) => f,
        None => {
            let f = d.add_fn(it, nothing, "", 0, false)?;
            d.fixed.borrow_mut().push(f);
            d.basics.noop.set(Some(f));
            f
        }
    };
    it.host_promise_then(p, Value::Undefined, Value::Object(noop))?;
    Ok(Value::Bool(true))
}
