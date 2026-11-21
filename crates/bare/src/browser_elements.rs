//! `HTMLAnchorElement`, `HTMLAreaElement`, `HTMLTemplateElement`, `HTMLImageElement` and `HTMLSlotElement`.
//!
//! Lazy: the four globals are accessors until the first read (`browser_lazy.rs`). The four prototypes are made once, by the first read of a
//! global or by the first wrapper of an `<a>`, `<area>`, `<template>` or `<img>` (`proto`, called by `Dom::wrap`). A realm that has none pays for
//! four accessor functions.
//!
//! `HTMLImageElement`: `src` (the attribute resolved against the base URL, else the attribute text), `currentSrc` (the same, `""` without `src`),
//! `alt`, `srcset`, `sizes`, `loading`, `decoding` (reflected strings), `naturalWidth`, `naturalHeight` (the picture that `bare/src/images.rs`
//! decoded, else 0), `width`, `height` (the attribute as an unsigned number, else the natural size: deviation, Chrome gives the rendered size),
//! `complete` (false while a download is on the way, see `browser_img.rs`). `new Image()`, `decode()`, `onload` and `onerror` are in
//! `browser_img.rs`. `crossOrigin`, `useMap`, `isMap` and `referrerPolicy` are reflected (`browser_reflect.rs`). Absent: `x`, `y`.
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
//! Reflected (`browser_reflect.rs`): `rel`, `target`, `referrerPolicy`, `ping` (on `a` and `area`), `hreflang`, `type` (on `a`). `a.text` is the
//! text content. Absent: `username`, `password` (the parser gates user info), `area.download`.
//!
//! `HTMLTemplateElement.content` is a `DocumentFragment` (a node with the data of a shadow root and no host: `Document::create_fragment`). The tree
//! builder moves the children of each parsed `<template>` into it, so `template.childNodes` is empty, as in Chrome. A `<template>` made by a script gets
//! an empty fragment on the first read. Deviation: the contents have no inert document of their own, so `content.ownerDocument` is `document`.
//! `cloneNode(true)` of a template copies its contents. `content` has no setter. The inserted-script rules apply to a script that moves out of the
//! contents, as for any inserted script.

use crate::browser_sources::Roots;
use crate::browser::*;
use crate::browser_scoped::*;
use bare_js_api::{Args, Local};
use crate::browser_dom as nd;
use crate::browser_lazy::lazy_global;
use crate::browser_navigation_url::{Part, Rec, UErr, clean, parse, scheme_of};
use crate::browser_reflect::{reflect, this_of};
use bare_html::dom::NodeId;
use bare_js::eval::{Interp};
use bare_js::value::{ObjId, Value};
use std::cell::Cell;

/// `[prototype of a, area, template, img, slot; constructor of the same]`, once made. All are roots.
#[derive(Default)]
pub(crate) struct Els {
    objs: Cell<Option<[ObjId; 10]>>,
    pub(crate) tagged: Cell<u8>,
}

pub(crate) fn roots(d: &Dom, out: &mut Roots<'_, '_>) {
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
        "img" => 3,
        "slot" => 4,
        _ => 2,
    }])
}

/// The `Symbol.toStringTag` of `DocumentFragment` and `ShadowRoot`, set once, by the first fragment that a page makes (`content`, `createDocumentFragment`,
/// `new DocumentFragment()`): a realm with none pays nothing.
pub(crate) fn tag_fragments(d: &Dom, it: &mut Interp<'_>) -> R<()> {
    if d.els.tagged.get() & 1 == 0 {
        d.els.tagged.set(d.els.tagged.get() | 1);
        crate::browser_lazy::tag(d, it, d.p().fragment, "DocumentFragment")?;
        crate::browser_lazy::tag(d, it, d.p().shadow, "ShadowRoot")?;
    }
    Ok(())
}

fn ensure(d: &Dom, it: &mut Interp<'_>) -> R<()> {
    if d.els.objs.get().is_none() {
        make(d, it)?;
    }
    Ok(())
}

lazy_global!(lazy_a, "HTMLAnchorElement", ensure, |d| d.els.objs.get().expect("made")[5]);
lazy_global!(lazy_area, "HTMLAreaElement", ensure, |d| d.els.objs.get().expect("made")[6]);
lazy_global!(lazy_template, "HTMLTemplateElement", ensure, |d| d.els.objs.get().expect("made")[7]);
lazy_global!(lazy_img, "HTMLImageElement", ensure, |d| d.els.objs.get().expect("made")[8]);
lazy_global!(lazy_slot, "HTMLSlotElement", ensure, |d| d.els.objs.get().expect("made")[9]);

pub(crate) fn install(d: &Dom, it: &mut Interp<'_>) -> R<()> {
    crate::browser_lazy::install(d, it, &[("HTMLAnchorElement", lazy_a as HS), ("HTMLAreaElement", lazy_area), ("HTMLTemplateElement", lazy_template), ("HTMLImageElement", lazy_img), ("HTMLSlotElement", lazy_slot)])
}

#[cold]
#[inline(never)]
fn make(d: &Dom, it: &mut Interp<'_>) -> R<[ObjId; 10]> {
    let (kp, kc) = (d.key(it, "prototype")?, d.key(it, "constructor")?);
    let mut o = [d.p().html; 10];
    for (i, name) in ["HTMLAnchorElement", "HTMLAreaElement", "HTMLTemplateElement", "HTMLImageElement", "HTMLSlotElement"].into_iter().enumerate() {
        let proto = d.obj(it, d.p().html)?;
        let f = crate::browser_custom::interface_ctor(d, it, name, false)?;
        it.define_data(f, kp, Value::Object(proto), false, false, false)?;
        it.define_data(proto, kc, Value::Object(f), true, false, true)?;
        crate::browser_lazy::tag(d, it, proto, name)?;
        (o[i], o[i + 5]) = (proto, f);
    }
    tag_fragments(d, it)?;
    d.def_get_s(it, o[0], "relList", crate::browser_platform_class::rel_list::<1>, Some(crate::browser_platform_class::set_rel_list::<1>))?;
    d.def_get_s(it, o[1], "relList", crate::browser_platform_class::rel_list::<2>, Some(crate::browser_platform_class::set_rel_list::<2>))?;
    members::<false>(d, it, o[0])?;
    d.def_get_s(it, o[0], "download", download, Some(set_download))?;
    members::<true>(d, it, o[1])?;
    for p in [o[0], o[1]] {
        reflect!(d, it, p, &["a", "area"], {
            "rel" => "rel": Str, "target" => "target": Str, "referrerPolicy" => "referrerpolicy": Str, "ping" => "ping": Str,
        });
    }
    reflect!(d, it, o[0], &["a"], { "hreflang" => "hreflang": Str, "type" => "type": Str });
    d.def_get_s(it, o[0], "text", |d, s, t, a| { this_of(d, s, t, &["a"])?; nd::text_content_s(d, s, t, a) },
        Some(|d, s, t, a| { this_of(d, s, t, &["a"])?; nd::set_text_content_s(d, s, t, a) }))?;
    d.def_get_s(it, o[2], "content", content, None)?;
    image_members(d, it, o[3])?;
    crate::browser_slot::members(d, it, o[4])?;
    d.els.objs.set(Some(o));
    Ok(o)
}

/// Getter and setter of one part, for `a` (`AREA` false) or `area`.
macro_rules! part {
    ($d:expr, $it:expr, $o:expr, $name:literal, $p:expr) => {
        $d.def_get_s($it, $o, $name, |d, s, t, _| get(d, s, t, AREA, Some($p)), Some(|d, s, t, a| set(d, s, t, a, AREA, $p)))?;
    };
}

fn members<const AREA: bool>(d: &Dom, it: &mut Interp<'_>, o: ObjId) -> R<()> {
    d.def_get_s(it, o, "href", |d, it, t, _| get(d, it, t, AREA, Some(Part::Href)), Some(set_href::<AREA>))?;
    part!(d, it, o, "protocol", Part::Protocol);
    part!(d, it, o, "host", Part::Host);
    part!(d, it, o, "hostname", Part::Hostname);
    part!(d, it, o, "port", Part::Port);
    part!(d, it, o, "pathname", Part::Pathname);
    part!(d, it, o, "search", Part::Search);
    part!(d, it, o, "hash", Part::Hash);
    d.def_get_s(it, o, "origin", |d, s, t, _| get(d, s, t, AREA, None), None)?;
    d.def_method_s(it, o, "toString", 0, |d, s, t, _| get(d, s, t, AREA, Some(Part::Href)))?;
    Ok(())
}

/// The native `a` (or `area`) element of `this`, else `Illegal invocation`. An SVG `a` is not one.
fn element<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, area: bool) -> Res<'s, NodeId> {
    let n = d.el_s(s, this)?;
    if d.doc.borrow().is_html(n, if area { "area" } else { "a" }) { Ok(n) } else { Err(s.type_error("Illegal invocation")) }
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
pub(crate) fn base(d: &Dom) -> Option<Rec> {
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
        Part::Host | Part::Hostname | Part::Port | Part::Username | Part::Password => String::new(),
    }
}

/// The part `p` of the URL, or its `origin` (`None`).
fn get<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, area: bool, p: Option<Part>) -> Res<'s, Local<'s>> {
    let n = element(d, s, this, area)?;
    let st = match old(s, |it| link(d, it, n))? {
        None => if p == Some(Part::Protocol) { ":".into() } else { String::new() },
        Some((_, Url::Hier(r))) => p.map_or_else(|| r.origin(), |p| p.get(&r)),
        Some((_, Url::Opaque(st, rest))) => p.map_or_else(|| "null".into(), |p| opaque(p, &st, &rest)),
        Some((raw, Url::Null | Url::Gate(_))) if p == Some(Part::Href) => raw,
        Some((_, Url::Null)) => if p == Some(Part::Protocol) { ":".into() } else { String::new() },
        Some((_, Url::Gate(w))) => return Err(unsupp(w)),
    };
    d.js_str_s(s, &st)
}

/// `a.download`: the `download` attribute (the suggested file name), an empty string without it.
fn download<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let n = element(d, s, this, false)?;
    let v = { let doc = d.doc.borrow(); doc.find_atom("download").and_then(|a| doc.attr(n, a)).unwrap_or("").to_string() };
    d.js_str_s(s, &v)
}

fn set_download<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let n = element(d, s, this, false)?;
    let st = d.arg_str_s(s, a, 0)?;
    nd::attr_set_s(d, s, n, "download", &st)?;
    Ok(s.undefined())
}

fn set_href<'s, const AREA: bool>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let n = element(d, s, this, AREA)?;
    let st = d.arg_str_s(s, a, 0)?;
    nd::attr_set_s(d, s, n, "href", &st)?;
    Ok(s.undefined())
}

/// Change one part of the resolved URL and write the whole URL to `href`. Nothing happens without a URL.
fn set<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, a: Args<'s>, area: bool, p: Part) -> Res<'s, Local<'s>> {
    let n = element(d, s, this, area)?;
    let v = d.arg_str_s(s, a, 0)?;
    match old(s, |it| link(d, it, n))? {
        Some((_, Url::Hier(mut r))) => match p.set(&mut r, &v) {
            Ok(()) => nd::attr_set_s(d, s, n, "href", &r.href())?,
            Err(UErr::Gate(w)) => return Err(unsupp(w)),
            Err(_) => {}
        },
        Some((_, Url::Gate(w))) => return Err(unsupp(w)),
        _ => {}
    }
    Ok(s.undefined())
}

/// `HTMLTemplateElement.content`: the contents fragment, made empty on the first read of a template that has none.
fn content<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let n = d.el_s(s, this)?;
    let c = {
        let mut doc = d.doc.borrow_mut();
        if !doc.is_html(n, "template") {
            drop(doc);
            return Err(s.type_error("Illegal invocation"));
        }
        match doc.template_content(n) {
            Some(c) => c,
            None => {
                d.room_s(s, &doc, 1, 0)?;
                doc.make_template_content(n)
            }
        }
    };
    d.wrap_s(s, c)
}

// ---------------------------------------------------------------------------------------------- HTMLImageElement

/// The native `img` element of `this`, else `Illegal invocation`.
fn image<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>) -> Res<'s, NodeId> {
    let n = d.el_s(s, this)?;
    if d.doc.borrow().is_html(n, "img") { Ok(n) } else { Err(s.type_error("Illegal invocation")) }
}

fn attr_text(d: &Dom, n: NodeId, name: &str) -> Option<String> {
    let doc = d.doc.borrow();
    doc.find_atom(name).and_then(|a| doc.attr(n, a)).map(str::to_string)
}

fn img_str<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, name: &str) -> Res<'s, Local<'s>> {
    let n = image(d, s, this)?;
    let st = attr_text(d, n, name).unwrap_or_default();
    d.js_str_s(s, &st)
}

fn img_set<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, a: Args<'s>, name: &str) -> Res<'s, Local<'s>> {
    let n = image(d, s, this)?;
    let st = d.arg_str_s(s, a, 0)?;
    nd::attr_set_s(d, s, n, name, &st)?;
    Ok(s.undefined())
}

/// The `src` attribute resolved against the base URL; the attribute text when it does not parse; `""` without the attribute.
fn img_url<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, current: bool) -> Res<'s, Local<'s>> {
    let n = image(d, s, this)?;
    let Some(raw) = attr_text(d, n, "src") else { return d.js_str_s(s, "") };
    s.charge(raw.len() as u64 / 32)?;
    let st = match parse(&raw, base(d).as_ref()) {
        Ok(r) => r.href(),
        Err(_) if current => String::new(),
        Err(_) => raw,
    };
    d.js_str_s(s, &st)
}

/// The size of the decoded picture, `(0, 0)` while there is none.
fn natural(d: &Dom, n: NodeId) -> (u32, u32) {
    d.doc.borrow().image(n).map_or((0, 0), |i| (i.0, i.1))
}

/// `width` (`HEIGHT` false) or `height`: the attribute as an unsigned number, else the natural size.
fn img_size<'s, const HEIGHT: bool>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let n = image(d, s, this)?;
    let attr = attr_text(d, n, if HEIGHT { "height" } else { "width" });
    let v = attr.and_then(|st| st.trim().parse::<u32>().ok()).unwrap_or_else(|| if HEIGHT { natural(d, n).1 } else { natural(d, n).0 });
    Ok(s.number(v as f64))
}

fn img_set_size<'s, const HEIGHT: bool>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let n = image(d, s, this)?;
    let arg0 = arg_s(s, a, 0);
    let v = s.to_number(arg0)?;
    let v = if v.is_finite() && v >= 0.0 { (v.trunc() % 4294967296.0) as u32 } else { 0 };
    nd::attr_set_s(d, s, n, if HEIGHT { "height" } else { "width" }, &v.to_string())?;
    Ok(s.undefined())
}

/// Getter and setter of a reflected string attribute of `img`.
macro_rules! img_attr {
    ($d:expr, $it:expr, $o:expr, $name:literal) => {
        $d.def_get_s($it, $o, $name, |d, s, t, _| img_str(d, s, t, $name), Some(|d, s, t, a| img_set(d, s, t, a, $name)))?;
    };
}

fn image_members(d: &Dom, it: &mut Interp<'_>, o: ObjId) -> R<()> {
    d.def_get_s(it, o, "src", |d, s, t, _| img_url(d, s, t, false), Some(|d, s, t, a| {
        let r = img_set(d, s, t, a, "src");
        crate::browser_img::src_changed(d, image(d, s, t)?);
        r
    }))?;
    d.def_get_s(it, o, "currentSrc", |d, s, t, _| img_url(d, s, t, true), None)?;
    img_attr!(d, it, o, "alt");
    img_attr!(d, it, o, "srcset");
    img_attr!(d, it, o, "sizes");
    img_attr!(d, it, o, "loading");
    img_attr!(d, it, o, "decoding");
    reflect!(d, it, o, &["img"], {
        "crossOrigin" => "crossorigin": Cors, "useMap" => "usemap": Str, "isMap" => "ismap": Bool, "referrerPolicy" => "referrerpolicy": Str,
    });
    d.def_get_s(it, o, "width", img_size::<false>, Some(img_set_size::<false>))?;
    d.def_get_s(it, o, "height", img_size::<true>, Some(img_set_size::<true>))?;
    d.def_get_s(it, o, "naturalWidth", |d, s, t, _| { let n = natural(d, image(d, s, t)?).0 as f64; Ok(s.number(n)) }, None)?;
    d.def_get_s(it, o, "naturalHeight", |d, s, t, _| { let n = natural(d, image(d, s, t)?).1 as f64; Ok(s.number(n)) }, None)?;
    d.def_get_s(it, o, "complete", |d, s, t, _| {
        let n = image(d, s, t)?;
        Ok(s.boolean(!d.img.pending.borrow().contains(&n)))
    }, None)?;
    crate::browser_img::members(d, it, o)
}
