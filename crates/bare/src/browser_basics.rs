//! Page basics: the names that live pages read on `document`, `window` and `navigator` and that bare did not define. A missing name
//! was `undefined`, and code such as Mixpanel's `document.referrer.search(...)` threw at load (wise.com showed its error page).
//!
//! Cost rule: a realm that never reads a name pays almost nothing. Constants are data properties (no function object). A value that
//! depends on the document is a getter. `navigator` is lazy as a whole (`browser_web.rs`), so its names cost a page that never reads
//! `navigator` nothing. Values are Chrome 155 on Linux where the HTML Standard leaves a choice (`docs/internal/decisions.md` lists each one).
//!
//! `document`: `visibilityState` "visible" and `hidden` false (getters: "hidden" and true once the page closes, `Browser::hide`),
//! `referrer` "" (bare sends no `Referer`), `designMode` "off" (data properties: a write only stores the value, the page has no design
//! mode); `characterSet`, `charset` and `inputEncoding` (one getter:
//! the `<meta charset>` or `<meta http-equiv=content-type>` in `<head>` through `bare_html::encoding`'s labels, else "UTF-8"; the
//! `Content-Type` header is not known to the document), `contentType` "text/html", `compatMode` ("CSS1Compat" with a doctype, else
//! "BackCompat"; the tree builder keeps the doctype name only, so a legacy public id is not seen), `dir` (the `dir` of `<html>`
//! lower-cased when it is `ltr`, `rtl` or `auto`, else ""; the setter writes the attribute), `lastModified` (the load time in UTC as
//! `MM/DD/YYYY hh:mm:ss`; the `Last-Modified` header is not known to the document).
//!
//! `window`: `devicePixelRatio` 1, `screenX` and `screenY` 0, `crossOriginIsolated` reflects the host agent, `closed` false, `opener` and `frameElement`
//! null (data properties); `isSecureContext` (https, or a loopback or `.localhost` host); `outerWidth` and `outerHeight` are the inner
//! size (`browser_scroll.rs`); `print`, `focus` and `blur` do nothing; `alert`, `confirm`, `prompt` and `open` answer a driver (`agent.rs`); without one
//! `alert` does nothing, `confirm` gives false, `prompt` gives null and `open` gives null (a blocked popup, as it does for a driver). `postMessage(message, targetOrigin | { targetOrigin, transfer })` clones the message (`structuredClone`), drops
//! it when `targetOrigin` is not "*", "/" or the origin of the document (an unparsable `targetOrigin` is a `SyntaxError`), and queues a
//! `message` event for the window as one task (`Dom::post_window`). The event has `origin` (the document origin), `source` (the window)
//! and `ports` (a frozen array of the new ports of the transfer list). The transfer list is the one of `MessagePort.postMessage`
//! (`Dom::transfer_list`): `ArrayBuffer` and `MessagePort` items, else `DataCloneError`. A message to another origin is dropped before
//! the list is read, so its buffers stay attached.
//!
//! `navigator`: `vendor` "Google Inc.", `maxTouchPoints` 0, `webdriver` false, `pdfViewerEnabled` true, `appName` "Netscape", `appVersion`
//! (the user agent without "Mozilla/"), `product` "Gecko", `productSub` "20030107", `vendorSub` "", `doNotTrack` null, `javaEnabled()`
//! false, `plugins` and `mimeTypes` (the five PDF viewers and two PDF types of Chrome; made on the first read, then plain data), and
//! `sendBeacon(url, data)`: a `POST` with credentials through the queue of `fetch`. It returns false, and sends nothing, when the body
//! would take the bytes that wait in the queue over 64 KiB (the keepalive quota of the Fetch Standard: the bytes of requests in flight; a
//! request here is in flight from the call until the network pump sends it). A URL that does not resolve, or is not http(s), is a
//! `TypeError`. The reply is ignored and a failure is silent.

use crate::eng::{Eng, Ops, Val};
use crate::browser::*;
use crate::browser_scoped::*;
use bare_js_api::{Args, Engine, Local, Type};
use crate::browser_platform::arg;
use bare_html::dom::{Document, NodeData};
use bare_js_api::HostFn;
use std::cell::Cell;

/// The keepalive quota: bytes of request bodies in flight (Fetch Standard, "keepalive").
const QUOTA: usize = 65536;

/// The cost that a queued window message counts against the message caps (the clone has no byte count).
pub(crate) const MESSAGE_COST: usize = 1024;

#[derive(Default)]
pub(crate) struct Basics<E: Eng> {
    /// The wall clock when the realm was made: `lastModified`.
    loaded: Cell<f64>,
    /// The callback that makes a beacon promise handled. Made by the first beacon, then kept in `Dom::fixed`.
    noop: Cell<Option<<E as Eng>::Obj>>,
    /// The page closes (`Browser::hide`): `document.visibilityState` is "hidden" and `document.hidden` is true.
    pub(crate) hidden: Cell<bool>,
}

pub(crate) fn install<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>) -> Res<'s, ()> {
    d.basics.loaded.set(s.wall_now_ms());
    Ok(())
}

/// The names of `window` and of `Document.prototype` made here. The heap asks the host for them on the first miss (`Heap::browser_name`),
/// so a realm that never reads one pays nothing.
const WINDOW: [&str; 21] = ["DOMException", "FontFace", "devicePixelRatio", "screenX", "screenY", "crossOriginIsolated", "closed", "opener", "frameElement", "postMessage", "alert", "print", "focus", "blur", "close", "confirm", "prompt", "open", "isSecureContext", "outerWidth", "outerHeight"];
const DOCUMENT: [&str; 12] = ["hidden", "referrer", "visibilityState", "contentType", "compatMode", "characterSet", "charset", "inputEncoding", "lastModified", "designMode", "dir", "fonts"];

/// The ask of the heap for `key` (`browser_handlers::lazy_key`): define it when it is one of the names above. The name `open` is also a
/// method of `document` (`browser_loader.rs`), so it is defined here and the caller goes on.
pub(crate) fn define<E: Eng>(d: &Dom<E>, it: &mut E::Vm, key: &str) -> R<E, ()> {
    let (g, doc) = (*d.global.get().expect("installed"), d.p().document);
    let data = |it: &mut E::Vm, o, v| -> R<E, ()> {
        let k = d.key(it, key)?;
        it.define_data(o, k, v, true, true, true)
    };
    let hidden = |it: &mut E::Vm, h: HS<E>| -> R<E, ()> {
        let f = d.add_fn_s_obj(it, h, key, 0, HostFn::Method)?;
        let k = d.key(it, key)?;
        it.define_data(g, k, E::obj_raw(f), true, false, true)
    };
    match key {
        "DOMException" => crate::browser::define_window(d, it, key).map(|_| ()),
        "fonts" | "FontFace" => crate::browser_fonts::define(d, it),
        "devicePixelRatio" => data(it, g, E::num(1.0)),
        "screenX" | "screenY" => data(it, g, E::num(0.0)),
        "crossOriginIsolated" => getter(d, it, g, &[key], |d, s, _, _| Ok(s.boolean(d.agent_cluster.get().is_some())), false),
        "closed" => data(it, g, E::bool_raw(false)),
        "opener" | "frameElement" => data(it, g, E::null()),
        "postMessage" => d.def_method_s(it, g, "postMessage", 1, post_message).map(|_| ()),
        // `close`: a page that no script opened stays open (Chrome ignores the call). Teams binds it while it builds its window wrapper.
        "print" | "focus" | "blur" | "close" => hidden(it, nothing),
        "alert" => hidden(it, alert),
        "confirm" => hidden(it, confirm),
        "prompt" => hidden(it, prompt),
        "open" => hidden(it, open),
        "isSecureContext" => getter(d, it, g, &["isSecureContext"], |d, s, _, _| Ok(s.boolean(secure(d))), false),
        "outerWidth" => getter(d, it, g, &["outerWidth"], |d, s, _, _| Ok(s.number(d.plat.s.borrow().viewport.0 as f64)), false),
        "outerHeight" => getter(d, it, g, &["outerHeight"], |d, s, _, _| Ok(s.number(d.plat.s.borrow().viewport.1 as f64)), false),
        "hidden" => getter(d, it, doc, &[key], |d, s, _, _| Ok(s.boolean(d.basics.hidden.get())), false),
        "referrer" => {
            let empty = d.js_str(it, "")?;
            data(it, doc, empty)
        }
        "visibilityState" => getter(d, it, doc, &[key], |d, s, _, _| d.js_str_s(s, if d.basics.hidden.get() { "hidden" } else { "visible" }), false),
        "contentType" => getter(d, it, doc, &[key], |d, s, _, _| d.js_str_s(s, "text/html"), false),
        "compatMode" => getter(d, it, doc, &[key], |d, s, _, _| d.js_str_s(s, if has_doctype(&d.doc.borrow()) { "CSS1Compat" } else { "BackCompat" }), false),
        "characterSet" | "charset" | "inputEncoding" => getter(d, it, doc, &[key], |d, s, _, _| d.js_str_s(s, charset(&d.doc.borrow())), false),
        "lastModified" => getter(d, it, doc, &[key], |d, s, _, _| d.js_str_s(s, &last_modified(d.basics.loaded.get())), false),
        // There is no design mode: the setter keeps nothing.
        "designMode" => getter(d, it, doc, &[key], |d, s, _, a| if a.is_empty() { d.js_str_s(s, "off") } else { Ok(s.undefined()) }, true),
        "dir" => getter(d, it, doc, &[key], dir, true),
        _ => Ok(()),
    }
}

/// Every name of `owner` (`Object.keys(window)` and the like ask for all names of an owner).
pub(crate) fn define_all<E: Eng>(d: &Dom<E>, it: &mut E::Vm, owner: E::Obj) -> R<E, ()> {
    let names: &[&str] = if owner == *d.global.get().expect("installed") { &WINDOW } else if owner == d.p().document { &DOCUMENT } else { return Ok(()) };
    for n in names.iter() {
        let k = d.key(it, n)?;
        if !it.h_has_own(owner, k)? {
            define(d, it, n)?;
        }
    }
    Ok(())
}

/// An accessor of `target` for each of `names`, all with one function (named as the first key, which the heap has already). With
/// `set` the same function is the setter: it runs as one when it gets an argument.
pub(crate) fn getter<E: Eng>(d: &Dom<E>, it: &mut E::Vm, target: E::Obj, names: &[&str], h: HS<E>, set: bool) -> R<E, ()> {
    let f = d.add_fn_s_obj(it, h, names[0], 0, HostFn::Method)?;
    for name in names {
        let k = d.key(it, name)?;
        d.define_accessor(it, target, k, f, set.then_some(f))?;
    }
    Ok(())
}

fn nothing<'s, E: Eng>(_: &Dom<E>, s: &mut Sc<'s, E>, _: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    Ok(s.undefined())
}

/// The argument `i` as text; a missing one is "".
fn text_arg<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, a: Args<'s>, i: usize) -> Res<'s, String> {
    if i >= a.len() {
        return Ok(String::new());
    }
    let v = arg_s::<E>(s, a, i);
    d.string_arg_s(s, v)
}

/// The dialogs answer from the policy of a driver (`agent.rs`). With no policy: `alert` does nothing, `confirm` gives false and `prompt` null.
fn alert<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, _: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let m = text_arg(d, s, a, 0)?;
    crate::agent::dialog("alert", &m, "");
    Ok(s.undefined())
}

fn confirm<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, _: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let m = text_arg(d, s, a, 0)?;
    Ok(s.boolean(crate::agent::dialog("confirm", &m, "").is_some_and(|(ok, _)| ok)))
}

fn prompt<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, _: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let (m, dflt) = (text_arg(d, s, a, 0)?, text_arg(d, s, a, 1)?);
    match crate::agent::dialog("prompt", &m, &dflt) {
        Some((true, Some(t))) => d.js_str_s(s, &t),
        _ => Ok(s.null()),
    }
}

/// `window.open(url)`: a tab for a driver (`agent.rs`); else null, a blocked popup. It always gives null: the page gets no window.
fn open<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, _: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let u = text_arg(d, s, a, 0)?;
    if let Some(url) = d.resolve_http(&u) {
        crate::agent::tab(&url);
    }
    Ok(s.null())
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
fn dir<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, _: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    if let Some(v) = a.get(0) {
        let v = d.string_arg_s(s, v)?;
        let html = html_of(&d.doc.borrow());
        if let Some(h) = html {
            crate::browser_dom::attr_set_s(d, s, h, "dir", &v)?;
        }
        return Ok(s.undefined());
    }
    let v = {
        let doc = d.doc.borrow();
        html_of(&doc).and_then(|h| doc.find_atom("dir").and_then(|a| doc.attr(h, a))).map(str::to_ascii_lowercase).filter(|v| matches!(v.as_str(), "ltr" | "rtl" | "auto")).unwrap_or_default()
    };
    d.js_str_s(s, &v)
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
pub(crate) fn secure<E: Eng>(d: &Dom<E>) -> bool {
    d.fetch_base().is_some_and(|u| u.scheme == bare_url::Scheme::Https || crate::browser_fetch::potentially_trustworthy(&u))
}

/// The origin of the document, serialized: "null" while no URL is configured.
fn doc_origin<E: Eng>(d: &Dom<E>) -> String {
    let nav = d.nav.s.borrow();
    if let Some(e) = nav.entries.get(nav.cur) {
        return e.url.origin();
    }
    drop(nav);
    d.fetch_base().and_then(|u| crate::browser_navigation_url::parse(&u.to_string(), None).ok()).map_or_else(|| "null".into(), |r| r.origin())
}

impl<E: Eng> Dom<E> {
    /// The origin that a `message` event of the window reports.
    pub(crate) fn window_origin(&self) -> String {
        doc_origin(self)
    }
}

/// The arguments of `window.postMessage`, own or remote: the target origin as given ("/" when absent) and the transfer list.
/// `postMessage(message, targetOrigin, transfer)` or `postMessage(message, { targetOrigin, transfer })`.
pub(crate) fn post_args<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, a: Args<'s>) -> Res<'s, (String, Vec<E::Obj>)> {
    if a.is_empty() {
        return Err(s.type_error("Failed to execute 'postMessage' on 'Window': 1 argument required, but only 0 present."));
    }
    let options = arg_s::<E>(s, a, 1);
    let (target, transfer) = match s.type_of(options) {
        Type::Undefined | Type::Null => (s.undefined(), s.undefined()),
        Type::Object | Type::Function => {
            let target = s.get_str(options, "targetOrigin")?;
            (target, s.get_str(options, "transfer")?)
        }
        _ => (options, arg_s::<E>(s, a, 2)),
    };
    let target = match s.type_of(target) {
        Type::Undefined => "/".to_string(),
        _ => {
            let t = s.to_string(target)?;
            s.as_str(t).map_or_else(String::new, |t| t.to_string_lossy())
        }
    };
    let list = d.transfer_list(s, transfer, None, "Failed to execute 'postMessage' on 'Window'")?;
    Ok((target, list))
}

/// The origin that a target origin names: `None` for "*" and "/", else the origin of the URL (`SyntaxError` when it does not parse).
pub(crate) fn target_origin<'s, E: Eng>(s: &mut Sc<'s, E>, d: &Dom<E>, target: &str) -> Res<'s, Option<String>> {
    if target == "*" || target == "/" {
        return Ok(None);
    }
    match crate::browser_navigation_url::parse(target, None) {
        Ok(r) => Ok(Some(r.origin())),
        Err(_) => Err(d.dom_err_s(s, "SyntaxError", &format!("Failed to execute 'postMessage' on 'Window': Invalid target origin '{target}' in a call to 'postMessage'."))),
    }
}

fn post_message<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let this = s.engine().raw(this);
    // The window of a blank frame has no message listeners that run here, so a message to it is dropped.
    if matches!(E::kind(this), Val::Object(o) if crate::browser_frames::is_blank_window(d, o)) {
        return Ok(s.undefined());
    }
    if !matches!(E::kind(this), Val::Undefined | Val::Null) && !matches!(E::kind(this), Val::Object(o) if Some(&o) == d.global.get()) {
        return Err(s.type_error("Illegal invocation"));
    }
    let (target, list) = post_args(d, s, a)?;
    if let Some(origin) = target_origin(s, d, &target)?
        && origin != doc_origin(d)
    {
        return Ok(s.undefined());
    }
    let msg = arg_s::<E>(s, a, 0);
    let (data, ports, _) = crate::browser_navigation_state::clone_with(d, s, msg, &list)?;
    let data = s.engine().raw(data);
    lift::<E, _>(s, d.post_window(data, ports.into_boxed_slice()))?;
    Ok(s.undefined())
}

// ---------------------------------------------------------------------------------------------- navigator

/// The names of `navigator` that this file adds, on its prototype `np`. `this` is checked with `check` (the one of `browser_web.rs`).
pub(crate) fn navigator<E: Eng>(d: &Dom<E>, it: &mut E::Vm, np: E::Obj) -> R<E, ()> {
    // Read-only data properties, not getters: a write fails as it does for an accessor with no setter.
    let strings = [("vendor", "Google Inc."), ("appName", "Netscape"), ("appVersion", crate::browser_web::USER_AGENT.trim_start_matches("Mozilla/")), ("product", "Gecko"), ("productSub", "20030107"), ("vendorSub", "")];
    for (name, v) in strings {
        let (k, v) = (d.key(it, name)?, d.js_str(it, v)?);
        it.define_data(np, k, v, false, true, true)?;
    }
    for (name, v) in [("maxTouchPoints", E::num(0.0)), ("webdriver", E::bool_raw(false)), ("pdfViewerEnabled", E::bool_raw(true)), ("doNotTrack", E::null())] {
        let k = d.key(it, name)?;
        it.define_data(np, k, v, false, true, true)?;
    }
    d.def_get_s(it, np, "plugins", |d, s, t, _| plugins(d, s, t, 0), None)?;
    d.def_get_s(it, np, "mimeTypes", |d, s, t, _| plugins(d, s, t, 1), None)?;
    d.def_method_s(it, np, "javaEnabled", 0, |d, s, t, _| {
        let t = s.engine().raw(t);
        old::<E, _>(s, |it| crate::browser_web::nav_this(d, it, t))?;
        Ok(s.boolean(false))
    })?;
    d.def_method_s(it, np, "sendBeacon", 1, send_beacon)?;
    Ok(())
}

const PLUGINS: [&str; 5] = ["PDF Viewer", "Chrome PDF Viewer", "Chromium PDF Viewer", "Microsoft Edge PDF Viewer", "WebKit built-in PDF"];
const MIMES: [(&str, &str); 2] = [("application/pdf", "Portable Document Format"), ("text/pdf", "Portable Document Format")];

/// `navigator.plugins` (`which` 0) or `navigator.mimeTypes` (1): both arrays are made on the first read of either and become own data
/// properties of `navigator`, so the second read is the same object.
fn plugins<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, which: usize) -> Res<'s, Local<'s>> {
    let this = s.engine().raw(this);
    old_v::<E>(s, |it| plugins_old(d, it, this, which))
}

fn plugins_old<E: Eng>(d: &Dom<E>, it: &mut E::Vm, this: E::Raw, which: usize) -> R<E, E::Raw> {
    crate::browser_web::nav_this(d, it, this)?;
    let Val::Object(nav) = E::kind(this) else { unreachable!() };
    let op = it.object_prototype();
    let mk = |it: &mut E::Vm, props: &[(&str, E::Raw)]| -> R<E, E::Obj> {
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
        mimes.push(E::obj_raw(mk(it, &[("type", t), ("suffixes", sfx), ("description", desc)])?));
    }
    let mut plugs = Vec::new();
    for name in PLUGINS {
        let (n, f, desc) = (d.js_str(it, name)?, d.js_str(it, "internal-pdf-viewer")?, d.js_str(it, "Portable Document Format")?);
        let p = mk(it, &[("name", n), ("filename", f), ("description", desc), ("length", E::num(MIMES.len() as f64))])?;
        for (i, &m) in mimes.iter().enumerate() {
            let k = d.key(it, &i.to_string())?;
            it.define_data(p, k, m, false, true, true)?;
        }
        plugs.push(E::obj_raw(p));
    }
    for &m in &mimes {
        let (k, v) = (d.key(it, "enabledPlugin")?, plugs[0]);
        let Val::Object(m) = E::kind(m) else { unreachable!() };
        it.define_data(m, k, v, false, true, true)?;
    }
    let (plugs, mimes) = (it.array_from(&plugs)?, it.array_from(&mimes)?);
    for (name, v) in [("plugins", plugs), ("mimeTypes", mimes)] {
        let k = d.key(it, name)?;
        it.define_data(nav, k, v, true, true, true)?;
    }
    Ok([plugs, mimes][which])
}

fn send_beacon<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let this = s.engine().raw(this);
    old::<E, _>(s, |it| crate::browser_web::nav_this(d, it, this))?;
    if a.is_empty() {
        return Err(s.type_error("Failed to execute 'sendBeacon' on 'Navigator': 1 argument required, but only 0 present."));
    }
    let a: Vec<E::Raw> = a.iter().map(|v| s.engine().raw(v)).collect();
    let queued = old::<E, _>(s, |it| send_beacon_old(d, it, &a))?;
    Ok(s.boolean(queued))
}

/// The rest of `sendBeacon`: whether the beacon was queued.
fn send_beacon_old<E: Eng>(d: &Dom<E>, it: &mut E::Vm, a: &[E::Raw]) -> R<E, bool> {
    let s = it.to_js_string(a[0])?;
    let url = it.h_to_string_lossy(s)?;
    if let Some(base) = d.fetch_base() {
        match base.join(&url) {
            Ok(u) if matches!(u.scheme, bare_url::Scheme::Http | bare_url::Scheme::Https) => {}
            Ok(_) => return Err(it.type_error_o("Failed to execute 'sendBeacon' on 'Navigator': Beacons are only supported over HTTP(S).")),
            Err(_) => return Err(it.type_error_o(&format!("Failed to execute 'sendBeacon' on 'Navigator': The URL '{url}' is invalid."))),
        }
    }
    let data = arg::<E>(a, 1);
    let n = match E::kind(data) {
        Val::Undefined | Val::Null => 0,
        Val::Object(_) if d.web_body_len(data).is_some() => d.web_body_len(data).unwrap_or(0),
        Val::Object(o) if it.h_bytes(o).is_some() => it.h_bytes(o).map_or(0, <[u8]>::len),
        _ => {
            let s = it.to_js_string(data)?;
            it.h_to_string_lossy(s)?.len()
        }
    };
    if d.queued_body_bytes().saturating_add(n) > QUOTA {
        return Ok(false);
    }
    let init = d.obj(it, it.object_prototype())?;
    for (k, v) in [("method", d.js_str(it, "POST")?), ("credentials", d.js_str(it, "include")?), ("keepalive", E::bool_raw(true))] {
        let k = d.key(it, k)?;
        it.define_data(init, k, v, true, true, true)?;
    }
    if n > 0 || !matches!(E::kind(data), Val::Undefined | Val::Null) {
        let k = d.key(it, "body")?;
        it.define_data(init, k, data, true, true, true)?;
    }
    // Make the promise handled: a beacon has no reply, so a failure is silent.
    let p = crate::browser_fetch::fetch_args(d, it, &[a[0], E::obj_raw(init)])?;
    let noop = match d.basics.noop.get() {
        Some(f) => f,
        None => {
            let f = d.add_fn_s_obj(it, nothing, "", 0, HostFn::Method)?;
            d.fixed.borrow_mut().push(f);
            d.basics.noop.set(Some(f));
            f
        }
    };
    it.host_promise_then(p, E::undef(), E::obj_raw(noop))?;
    Ok(true)
}
