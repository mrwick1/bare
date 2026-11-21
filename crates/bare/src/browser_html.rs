//! The `HTML*Element` interfaces of the tags that have no module of their own (`HTMLSpanElement`, `HTMLDivElement`, `HTMLParagraphElement`,
//! `HTMLTableElement` and the rest): `el instanceof HTMLSpanElement`, `el.constructor` and `Object.prototype.toString`.
//!
//! Cause: pages test `x instanceof HTMLSpanElement` or read the global, and a missing global threw `ReferenceError` (GitLab).
//!
//! - The globals have no accessor: a name `HTML<x>Element` that the window does not have goes to the host (the lazy ask of `browser_handlers.rs`,
//!   `define_global`), so a realm that never names one pays nothing at start. Deviation: `Object.getOwnPropertyNames(window)` lists them only after
//!   they were read.
//! - The prototypes are switched on by the first such read (`html_on`). Before it, these tags keep `HTMLElement.prototype`: an extra prototype for
//!   each wrapper made every member read of every element one step longer (+0.6% on the `mutation` cost row), and only a page that names the
//!   interface can tell. At the switch, each existing wrapper of these tags gets its prototype, and each new one has it from the start. Deviation:
//!   before the first read of such a global, `el.constructor` is `HTMLElement` and `Object.prototype.toString.call(el)` is `[object HTMLElement]`.
//! - The prototype inherits from `HTMLElement.prototype`. The interfaces of `has_members` have members of their own (`browser_table.rs`,
//!   `browser_html_members.rs`, `canvas`, `meta`); these tags (except `li`) get their prototype from the first wrapper, also before the switch, because a
//!   page reads `table.rows` without naming the interface. The others (`div`, `span`, `p`, `ul`, ...) have no members of their own and keep the
//!   switch. Deviation: `li.value` is `undefined` until a page reads an `HTML*Element` global.
//! - `meta` reflects `name`, `content`, `httpEquiv` (`http-equiv`) and `media`. Pages read `meta.content` without naming the interface
//!   (GitHub reads a base64 payload from it after a soft navigation).
//!   The constructor is illegal and inherits from `HTMLElement`.
//! - The tags whose interface is `HTMLElement` itself (`section`, `nav`, `b`, ...) keep `HTMLElement.prototype`. A tag with a module of its own
//!   (`a`, `img`, `input`, `select`, ...) is not handled here.

use crate::eng::{Eng, Ops, Val};
use crate::browser::*;
use crate::browser_scoped::*;
use bare_js_api::{Args, Local};
use bare_html::dom::{NodeData, NodeId, Ns};

const NAMES: [&str; 49] = [
    "HTMLBaseElement",
    "HTMLQuoteElement",
    "HTMLBRElement",
    "HTMLCanvasElement",
    "HTMLTableCaptionElement",
    "HTMLTableColElement",
    "HTMLDataElement",
    "HTMLDataListElement",
    "HTMLModElement",
    "HTMLDetailsElement",
    "HTMLDialogElement",
    "HTMLDirectoryElement",
    "HTMLDivElement",
    "HTMLDListElement",
    "HTMLEmbedElement",
    "HTMLFieldSetElement",
    "HTMLFontElement",
    "HTMLFrameElement",
    "HTMLHeadingElement",
    "HTMLHeadElement",
    "HTMLHRElement",
    "HTMLHtmlElement",
    "HTMLLegendElement",
    "HTMLLIElement",
    "HTMLMapElement",
    "HTMLMarqueeElement",
    "HTMLMenuElement",
    "HTMLMetaElement",
    "HTMLMeterElement",
    "HTMLObjectElement",
    "HTMLOListElement",
    "HTMLOutputElement",
    "HTMLParagraphElement",
    "HTMLParamElement",
    "HTMLPictureElement",
    "HTMLPreElement",
    "HTMLProgressElement",
    "HTMLSlotElement",
    "HTMLSourceElement",
    "HTMLSpanElement",
    "HTMLStyleElement",
    "HTMLTableElement",
    "HTMLTableSectionElement",
    "HTMLTableCellElement",
    "HTMLTimeElement",
    "HTMLTitleElement",
    "HTMLTableRowElement",
    "HTMLTrackElement",
    "HTMLUListElement",
];

pub(crate) const INTERFACE_COUNT: usize = NAMES.len();

/// The index in `NAMES` of the interface of the tag `t`.
#[inline]
fn index(t: &str) -> Option<usize> {
    Some(match t {
        "base" => 0,
        "blockquote" | "q" => 1,
        "br" => 2,
        "canvas" => 3,
        "caption" => 4,
        "col" | "colgroup" => 5,
        "data" => 6,
        "datalist" => 7,
        "del" | "ins" => 8,
        "details" => 9,
        "dialog" => 10,
        "dir" => 11,
        "div" => 12,
        "dl" => 13,
        "embed" => 14,
        "fieldset" => 15,
        "font" => 16,
        "frame" => 17,
        "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => 18,
        "head" => 19,
        "hr" => 20,
        "html" => 21,
        "legend" => 22,
        "li" => 23,
        "map" => 24,
        "marquee" => 25,
        "menu" => 26,
        "meta" => 27,
        "meter" => 28,
        "object" => 29,
        "ol" => 30,
        "output" => 31,
        "p" => 32,
        "param" => 33,
        "picture" => 34,
        "pre" | "listing" | "xmp" => 35,
        "progress" => 36,
        "slot" => 37,
        "source" => 38,
        "span" => 39,
        "style" => 40,
        "table" => 41,
        "tbody" | "thead" | "tfoot" => 42,
        "td" | "th" => 43,
        "time" => 44,
        "title" => 45,
        "tr" => 46,
        "track" => 47,
        "ul" => 48,
        _ => return None,
    })
}

/// Does the interface `NAMES[i]` have members of its own (`browser_html_members.rs`, `browser_table.rs`, `canvas`)? Such a tag gets its
/// prototype from its first wrapper, so a page that reads `table.rows` without naming `HTMLTableElement` finds it.
/// The list: base, canvas, data, details, dialog, fieldset, html, meta, meter, ol, output, progress, source, table, thead/tbody/tfoot, td/th,
/// time, tr. A test checks it against the prototypes (`browser_web_api_tests.rs`). `li` has a member (`value`) but keeps the switch: an `li`
/// prototype from the first wrapper cost +0.24% on the `mutation` cost row (it makes `li` elements), and pages rarely read `li.value`.
/// Without `li`, the rule costs +0.08% on `mutation` and `event` (the tag lookup for each new wrapper).
#[inline]
fn has_members(i: usize) -> bool {
    matches!(i, 0 | 3 | 6 | 9 | 10 | 15 | 21 | 27 | 28 | 30 | 31 | 36 | 38 | 41 | 42 | 43 | 44 | 46)
}

/// The prototype of the HTML element `tag` that no other module handles: its interface, made now if it is the first use.
pub(crate) fn proto<E: Eng>(d: &Dom<E>, it: &mut E::Vm, tag: &str) -> R<E, E::Obj> {
    match index(tag) {
        Some(i) if d.html_on.get() || has_members(i) => proto_i(d, it, i),
        _ => Ok(d.p().html),
    }
}

fn proto_i<E: Eng>(d: &Dom<E>, it: &mut E::Vm, i: usize) -> R<E, E::Obj> {
    if let Some(Some((p, _))) = d.html_ifaces.borrow().get(i) {
        return Ok(*p);
    }
    make(d, it, i).map(|x| x.0)
}

#[cold]
#[inline(never)]
fn make<E: Eng>(d: &Dom<E>, it: &mut E::Vm, i: usize) -> R<E, (E::Obj, E::Obj)> {
    let (kp, kc) = (d.key(it, "prototype")?, d.key(it, "constructor")?);
    let proto = if NAMES[i] == "HTMLStyleElement" { crate::browser_cssom::style_proto(d, it)? } else { d.obj(it, d.p().html)? };
    let f = crate::browser_custom::interface_ctor(d, it, NAMES[i], true)?;
    it.define_data(f, kp, E::obj_raw(proto), false, false, false)?;
    it.define_data(proto, kc, E::obj_raw(f), true, false, true)?;
    crate::browser_lazy::tag(d, it, proto, NAMES[i])?;
    if i == 3 { crate::browser_canvas::members(d, it, proto)?; }
    if i == 27 { meta_members(d, it, proto)?; }
    let own = crate::browser_html_members::members(d, it, proto, NAMES[i])?;
    debug_assert!((own || i == 3 || i == 27) == has_members(i) || NAMES[i] == "HTMLLIElement", "{}", NAMES[i]);
    // Like every HTML interface, its constructor inherits from `HTMLElement`.
    let g = it.global_object();
    let k = d.key(it, "HTMLElement")?;
    if let Val::Object(h) = E::kind(it.get_value(E::obj_raw(g), k)?) {
        it.h_set_proto(f, Some(h))?;
    }
    d.fixed.borrow_mut().extend([proto, f]);
    let mut l = d.html_ifaces.borrow_mut();
    if l.len() < NAMES.len() {
        l.resize(NAMES.len(), None);
    }
    l[i] = Some((proto, f));
    Ok((proto, f))
}

/// Getter and setter of the string attribute `$attr` of `meta`, as the property `$name`. A closure that captures nothing is a plain function
/// pointer in Rust (like a JS function that uses no outer variable), so it fits the `HS` type of the handlers.
macro_rules! meta_attr {
    ($d:expr, $it:expr, $o:expr, $name:literal, $attr:literal) => {
        $d.def_get_s($it, $o, $name, |d, s, t, _| meta_get(d, s, t, $attr), Some(|d, s, t, a| meta_set(d, s, t, a, $attr)))?;
    };
}

fn meta_members<E: Eng>(d: &Dom<E>, it: &mut E::Vm, o: E::Obj) -> R<E, ()> {
    meta_attr!(d, it, o, "name", "name");
    meta_attr!(d, it, o, "content", "content");
    meta_attr!(d, it, o, "httpEquiv", "http-equiv");
    meta_attr!(d, it, o, "media", "media");
    Ok(())
}

/// The `meta` element of `this`, or `TypeError: Illegal invocation`.
fn meta_el<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>) -> Res<'s, NodeId> {
    let n = d.el_s(s, this)?;
    if d.doc.borrow().is_html(n, "meta") { Ok(n) } else { Err(s.type_error("Illegal invocation")) }
}

fn meta_get<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, name: &str) -> Res<'s, Local<'s>> {
    let n = meta_el(d, s, this)?;
    let v = { let doc = d.doc.borrow(); doc.find_atom(name).and_then(|k| doc.attr(n, k)).unwrap_or("").to_string() };
    d.js_str_s(s, &v)
}

fn meta_set<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>, name: &str) -> Res<'s, Local<'s>> {
    let n = meta_el(d, s, this)?;
    let arg0 = arg_s::<E>(s, a, 0);
    let v = d.string_arg_s(s, arg0)?;
    crate::browser_dom::attr_set_s(d, s, n, name, &v)?;
    Ok(s.undefined())
}

/// The ask of the heap for the global `name` (`Heap::set_lazy_chain`, see `browser_handlers.rs`): define it on the window when it is one of the
/// interfaces here. `false`: it is not.
pub(crate) fn define_global<E: Eng>(d: &Dom<E>, it: &mut E::Vm, name: &str) -> R<E, bool> {
    let Some(i) = NAMES.iter().position(|&n| n == name) else { return Ok(false) };
    switch_on(d, it)?;
    let made = d.html_ifaces.borrow().get(i).copied().flatten();
    let (_, f) = match made {
        Some(x) => x,
        None => make(d, it, i)?,
    };
    let (g, k) = (*d.global.get().expect("installed"), d.key(it, name)?);
    it.define_data(g, k, E::obj_raw(f), true, false, true)?;
    Ok(true)
}

/// The first read of a global of this file: give each wrapper that exists the prototype of its tag.
#[cold]
fn switch_on<E: Eng>(d: &Dom<E>, it: &mut E::Vm) -> R<E, ()> {
    if d.html_on.replace(true) {
        return Ok(());
    }
    let html = d.p().html;
    let nodes: Vec<(NodeId, E::Obj)> = d.st.borrow().wrappers().collect();
    it.charge_o(nodes.len() as u64)?;
    for (n, o) in nodes {
        let i = {
            let doc = d.doc.borrow();
            match *doc.data(n) {
                NodeData::Element { name, .. } if !matches!(doc.element_ns(n), Ns::Svg | Ns::MathMl) => index(doc.atom_str(name)),
                _ => None,
            }
        };
        let Some(i) = i else { continue };
        if it.h_get_proto(o)? == Some(html) {
            let p = proto_i(d, it, i)?;
            it.h_set_proto(o, Some(p))?;
        }
    }
    Ok(())
}
