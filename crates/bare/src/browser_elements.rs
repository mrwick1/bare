//! `HTMLAnchorElement`, `HTMLAreaElement` and `HTMLTemplateElement`.
//!
//! Lazy: the three globals are accessors until the first read (`browser_lazy.rs`). The three prototypes are made once, by the first read of a
//! global or by the first wrapper of an `<a>`, `<area>` or `<template>` (`proto`, called by `Dom::wrap`). A realm that has neither pays for three
//! accessor functions.
//!
//! Hyperlink members (`HTMLHyperlinkElementUtils`, on `a` and `area`): `href`, `protocol`, `host`, `hostname`, `port`, `pathname`, `search`,
//! `hash` (all with setters), the read-only `origin`, and `toString` (the `href`). The getter of `href` is the `href` attribute resolved against
//! the base URL. The setter of `href` writes the attribute as given. The other setters parse the resolved URL, change one part and write the result
//! back to `href`; a value that the URL parser refuses is ignored, as in a browser. The base URL is the one of the page loader (it follows `<base>`),
//! else the current document URL. Deviation: the loader's base does not follow `history.pushState`. `Link.href` has the same base.
//!
//! The parser is `browser_navigation_url.rs` (http and https only). A URL that it refuses reads like this:
//! - not a URL (`http://`, a relative URL with no base): `href` is the attribute text, `protocol` is `:`, every other part is empty;
//! - an opaque URL (`mailto:a@b.c?s#h`, `javascript:`, `tel:`, `data:`): `protocol`, `pathname`, `search`, `hash` and `href` are read from the text, the
//!   other parts are empty and `origin` is `null`;
//! - a form that the parser gates (user info, `\`, a scheme other than http and https with `//`, IDNA hosts): `href` is the attribute text, any other
//!   part is `Unsupported`. A setter on such a URL is `Unsupported` too.
//!
//! Absent: `username`, `password`, `target`, `rel`, `download`, `text`, `relList`, `ping` and the other attribute reflections.
//!
//! `HTMLTemplateElement.content` is a `DocumentFragment` (a node with the data of a shadow root and no host: `Document::create_fragment`). The tree
//! builder moves the children of each parsed `<template>` into it, so `template.childNodes` is empty, as in Chrome. A `<template>` made by a script gets
//! an empty fragment on the first read. Deviation: the contents have no inert document of their own, so `content.ownerDocument` is `document`.
//! `cloneNode(true)` of a template copies its contents. `content` has no setter. The inserted-script rules apply to a script that moves out of the
//! contents, as for any inserted script.

use crate::browser::*;
use crate::browser_dom as nd;
use crate::browser_lazy::lazy_global;
use crate::browser_navigation_url::{Part, Rec, UErr, clean, parse, scheme_of};
use crate::browser_platform::{V, arg};
use bare_html::dom::NodeId;
use bare_js::eval::{Error, Interp};
use bare_js::value::{ObjId, Value};
use std::cell::Cell;

/// `[prototype of a, area, template; constructor of a, area, template]`, once made. All are roots.
#[derive(Default)]
pub(crate) struct Els {
    objs: Cell<Option<[ObjId; 6]>>,
}

pub(crate) fn roots(d: &Dom, out: &mut Vec<Value>) {
    if let Some(o) = d.els.objs.get() {
        out.extend(o.map(Value::Object));
    }
}

/// The prototype of the wrapper of an HTML `a`, `area` or `template` (`tag`).
pub(crate) fn proto(d: &Dom, it: &mut Interp<'_>, tag: &str) -> R<ObjId> {
    let o = match d.els.objs.get() {
        Some(o) => o,
        None => make(d, it)?,
    };
    Ok(o[match tag {
        "a" => 0,
        "area" => 1,
        _ => 2,
    }])
}

fn ensure(d: &Dom, it: &mut Interp<'_>) -> R<()> {
    if d.els.objs.get().is_none() {
        make(d, it)?;
    }
    Ok(())
}

lazy_global!(lazy_a, "HTMLAnchorElement", ensure, |d| d.els.objs.get().expect("made")[3]);
lazy_global!(lazy_area, "HTMLAreaElement", ensure, |d| d.els.objs.get().expect("made")[4]);
lazy_global!(lazy_template, "HTMLTemplateElement", ensure, |d| d.els.objs.get().expect("made")[5]);

pub(crate) fn install(d: &Dom, it: &mut Interp<'_>) -> R<()> {
    crate::browser_lazy::install(d, it, &[("HTMLAnchorElement", lazy_a as H), ("HTMLAreaElement", lazy_area), ("HTMLTemplateElement", lazy_template)])
}

#[cold]
#[inline(never)]
fn make(d: &Dom, it: &mut Interp<'_>) -> R<[ObjId; 6]> {
    let (kp, kc) = (d.key(it, "prototype")?, d.key(it, "constructor")?);
    let mut o = [d.p().html; 6];
    for (i, name) in ["HTMLAnchorElement", "HTMLAreaElement", "HTMLTemplateElement"].into_iter().enumerate() {
        let proto = d.obj(it, d.p().html)?;
        let f = d.add_fn(it, illegal_constructor, name, 0, false)?;
        it.define_data(f, kp, Value::Object(proto), false, false, false)?;
        it.define_data(proto, kc, Value::Object(f), true, false, true)?;
        crate::browser_lazy::tag(d, it, proto, name)?;
        (o[i], o[i + 3]) = (proto, f);
    }
    // The only fragments of a realm without a shadow root come from `content`, so their tag is set here (the shadow root inherits the fragment prototype).
    crate::browser_lazy::tag(d, it, d.p().fragment, "DocumentFragment")?;
    crate::browser_lazy::tag(d, it, d.p().shadow, "ShadowRoot")?;
    members::<false>(d, it, o[0])?;
    members::<true>(d, it, o[1])?;
    d.def_get(it, o[2], "content", content, None)?;
    d.els.objs.set(Some(o));
    Ok(o)
}

/// Getter and setter of one part, for `a` (`AREA` false) or `area`.
macro_rules! part {
    ($d:expr, $it:expr, $o:expr, $name:literal, $p:expr) => {
        $d.def_get($it, $o, $name, |d, it, t, _| get(d, it, t, AREA, Some($p)), Some(|d, it, t, a| set(d, it, t, a, AREA, $p)))?;
    };
}

fn members<const AREA: bool>(d: &Dom, it: &mut Interp<'_>, o: ObjId) -> R<()> {
    d.def_get(it, o, "href", |d, it, t, _| get(d, it, t, AREA, Some(Part::Href)), Some(set_href::<AREA>))?;
    part!(d, it, o, "protocol", Part::Protocol);
    part!(d, it, o, "host", Part::Host);
    part!(d, it, o, "hostname", Part::Hostname);
    part!(d, it, o, "port", Part::Port);
    part!(d, it, o, "pathname", Part::Pathname);
    part!(d, it, o, "search", Part::Search);
    part!(d, it, o, "hash", Part::Hash);
    d.def_get(it, o, "origin", |d, it, t, _| get(d, it, t, AREA, None), None)?;
    d.def_method(it, o, "toString", 0, |d, it, t, _| get(d, it, t, AREA, Some(Part::Href)))?;
    Ok(())
}

/// The native `a` (or `area`) element of `this`, else `Illegal invocation`. An SVG `a` is not one.
fn element(d: &Dom, it: &mut Interp<'_>, this: Value, area: bool) -> R<NodeId> {
    let n = d.el(it, this)?;
    if d.doc.borrow().is_html(n, if area { "area" } else { "a" }) { Ok(n) } else { Err(it.type_error("Illegal invocation")) }
}

/// What the `href` attribute parses to.
enum Url {
    /// Not a URL.
    Null,
    /// `(lower case scheme, the text after the colon)`.
    Opaque(String, String),
    Hier(Rec),
    /// A form that the URL parser does not model.
    Gate(&'static str),
}

/// The base URL: the loader's (it follows `<base>`), else the document URL.
fn base(d: &Dom) -> Option<Rec> {
    match d.scripts.base.borrow().as_deref().filter(|b| !b.is_empty()) {
        Some(b) => parse(b, None).ok(),
        None => d.cur_rec().ok(),
    }
}

/// The `href` attribute of `n` and what it parses to; `None` without the attribute.
fn link(d: &Dom, it: &mut Interp<'_>, n: NodeId) -> R<Option<(String, Url)>> {
    let raw = {
        let doc = d.doc.borrow();
        let Some(raw) = doc.find_atom("href").and_then(|a| doc.attr(n, a)) else { return Ok(None) };
        raw.to_string()
    };
    it.charge(raw.len() as u64 / 32)?;
    let u = match parse(&raw, base(d).as_ref()) {
        Ok(r) => Url::Hier(r),
        Err(UErr::Invalid) => Url::Null,
        Err(UErr::Gate(w)) => Url::Gate(w),
        Err(UErr::Scheme) => {
            let text = clean(&raw);
            match scheme_of(text.trim_matches(|c: char| c <= ' ')) {
                Some((s, rest)) if !rest.starts_with('/') => Url::Opaque(s.to_ascii_lowercase(), rest.to_string()),
                _ => Url::Gate("a URL with a scheme other than http and https and a host or path"),
            }
        }
    };
    Ok(Some((raw, u)))
}

/// A part of an opaque URL (`rest` is what follows the scheme and its colon).
fn opaque(p: Part, scheme: &str, rest: &str) -> String {
    let (rest_q, hash) = rest.split_once('#').unwrap_or((rest, ""));
    let (path, query) = rest_q.split_once('?').unwrap_or((rest_q, ""));
    let with = |c: &str, s: &str| if s.is_empty() { String::new() } else { format!("{c}{s}") };
    match p {
        Part::Href => format!("{scheme}:{rest}"),
        Part::Protocol => format!("{scheme}:"),
        Part::Pathname => path.to_string(),
        Part::Search => with("?", query),
        Part::Hash => with("#", hash),
        Part::Host | Part::Hostname | Part::Port => String::new(),
    }
}

/// The part `p` of the URL, or its `origin` (`None`).
fn get(d: &Dom, it: &mut Interp<'_>, this: Value, area: bool, p: Option<Part>) -> V {
    let n = element(d, it, this, area)?;
    let s = match link(d, it, n)? {
        None => if p == Some(Part::Protocol) { ":".into() } else { String::new() },
        Some((_, Url::Hier(r))) => p.map_or_else(|| r.origin(), |p| p.get(&r)),
        Some((_, Url::Opaque(s, rest))) => p.map_or_else(|| "null".into(), |p| opaque(p, &s, &rest)),
        Some((raw, Url::Null | Url::Gate(_))) if p == Some(Part::Href) => raw,
        Some((_, Url::Null)) => if p == Some(Part::Protocol) { ":".into() } else { String::new() },
        Some((_, Url::Gate(w))) => return Err(Error::Unsupported(unsupported(w))),
    };
    d.js_str(it, &s)
}

fn set_href<const AREA: bool>(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let n = element(d, it, this, AREA)?;
    let s = d.string_arg(it, arg(a, 0))?;
    nd::attr_set(d, it, n, "href", &s)?;
    Ok(Value::Undefined)
}

/// Change one part of the resolved URL and write the whole URL to `href`. Nothing happens without a URL.
fn set(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value], area: bool, p: Part) -> V {
    let n = element(d, it, this, area)?;
    let v = d.string_arg(it, arg(a, 0))?;
    match link(d, it, n)? {
        Some((_, Url::Hier(mut r))) => match p.set(&mut r, &v) {
            Ok(()) => nd::attr_set(d, it, n, "href", &r.href())?,
            Err(UErr::Gate(w)) => return Err(Error::Unsupported(unsupported(w))),
            Err(_) => {}
        },
        Some((_, Url::Gate(w))) => return Err(Error::Unsupported(unsupported(w))),
        _ => {}
    }
    Ok(Value::Undefined)
}

/// `HTMLTemplateElement.content`: the contents fragment, made empty on the first read of a template that has none.
fn content(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let n = d.el(it, this)?;
    let c = {
        let mut doc = d.doc.borrow_mut();
        if !doc.is_html(n, "template") {
            drop(doc);
            return Err(it.type_error("Illegal invocation"));
        }
        match doc.template_content(n) {
            Some(c) => c,
            None => {
                d.room(&doc, 1, 0)?;
                doc.make_template_content(n)
            }
        }
    };
    d.wrap(it, c)
}
