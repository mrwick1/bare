//! `document.implementation` (`DOMImplementation`): `createHTMLDocument(title)` and `hasFeature()`.
//!
//! Cause: jQuery makes a document at load (`document.implementation.createHTMLDocument("")`, `support.createHTMLDocument`) and again in
//! `parseHTML`. Without `implementation` the `jquery` module of Wikipedia threw at load and the page lost every module that needs it.
//!
//! - The object is made on the first read of `document.implementation` (an accessor of `Document.prototype`), and it is the same object after.
//! - `createHTMLDocument(title)` makes a `Document` node outside the tree, with `html`, `head`, an optional `title` and `body`. The `Document`
//!   getters (`documentElement`, `head`, `body`, `createElement`, ...) work on it. It lives while a script holds its wrapper.
//! - Deviations: the new document has no doctype, `defaultView` and `location`, and a node made by its `createElement` has `ownerDocument`
//!   `document`, not this one (the arena has one owner document, as for the contents of a `template`). The initial document of a blank
//!   iframe is the exception: `browser_frames` records its window and its created nodes. Scripts in the new document never run.
//!   `createDocumentType` is absent.
//! - `createDocument(ns, qname, doctype)` makes the same kind of `Document` (no `XMLDocument`) and appends the element of `createElementNS(ns, qname)`.
//!   It ignores `doctype` and `contentType` stays "text/html".

use crate::eng::{Eng, Ops};
use crate::browser::*;
use crate::browser_scoped::*;
use bare_html::dom::NodeData;
use bare_js_api::{Args, Attrs, HostFn, Local, Type};

/// `implementation.createHTMLDocument(title)`.
fn create_html_document<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    receiver(d, s, this)?;
    let v = arg_s::<E>(s, a, 0);
    let title = if s.type_of(v) == Type::Undefined { None } else { Some(d.string_arg_s(s, v)?) };
    d.html_document(s, title)
}

/// `implementation.createDocument(namespace, qualifiedName, doctype)`. The result is a `Document` like the one of `createHTMLDocument`
/// (no `XMLDocument`), with no `html`, `head` or `body`. `doctype` is ignored: there is no `createDocumentType`. `contentType` stays "text/html".
fn create_document<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    receiver(d, s, this)?;
    if a.len() < 2 { return Err(s.type_error("Failed to execute 'createDocument' on 'DOMImplementation': 2 arguments required")); }
    let arg0 = arg_s::<E>(s, a, 0);
    let ns = crate::browser_namespace::ns_arg(d, s, arg0)?;
    let qname = d.arg_str_s(s, a, 1)?;
    let el = if qname.is_empty() { None } else { Some(crate::browser_namespace::element_ns(d, s, ns, &qname, None)?) };
    let n = {
        let mut doc = d.doc.borrow_mut();
        d.room_s(s, &doc, 1, 0)?;
        doc.create_document()
    };
    if let Some(el) = el {
        let e = d.el_s(s, el)?;
        d.doc.borrow_mut().move_to(e, n, None);
    }
    d.wrap_s(s, n)
}

impl<E: Eng> Dom<E> {
    /// A document with `html`, `head` and `body` outside the tree. An iframe with an initial `about:blank` document gets one too.
    pub(crate) fn new_blank_document(&self, it: &mut E::Vm) -> R<E, E::Raw> {
        run_v::<E>(it, |s| self.html_document(s, None))
    }
    fn html_document<'s>(&self, s: &mut Sc<'s, E>, title: Option<String>) -> Res<'s, Local<'s>> {
    let d = self;
    let n = {
        let mut doc = d.doc.borrow_mut();
        d.room_s(s, &doc, 5, title.as_ref().map_or(0, |t| t.len()))?;
        let none: &[(&str, &str)] = &[];
        let n = doc.create_document();
        let html = doc.insert_element(n, None, "html", none);
        let head = doc.insert_element(html, None, "head", none);
        if let Some(t) = &title {
            let el = doc.insert_element(head, None, "title", none);
            doc.append_text(el, t);
        }
        doc.insert_element(html, None, "body", none);
        n
    };
    d.wrap_s(s, n)
    }
}

fn receiver<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>) -> Res<'s, ()> {
    if obj_id::<E>(s, this).is_some_and(|o| d.implementation.get() == Some(o)) { return Ok(()); }
    Err(s.type_error("Illegal invocation"))
}

fn has_feature<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    receiver(d, s, this)?;
    Ok(s.boolean(true))
}

/// The getter of `document.implementation`.
fn implementation<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, _: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let o = match d.implementation.get() {
        Some(o) => o,
        None => old::<E, _>(s, |it| make_implementation(d, it))?,
    };
    Ok(d.obj_local(s, o))
}

fn make_implementation<E: Eng>(d: &Dom<E>, it: &mut E::Vm) -> R<E, E::Obj> {
    let proto = d.obj(it, it.object_prototype())?;
    d.def_method_s(it, proto, "createHTMLDocument", 0, create_html_document)?;
    d.def_method_s(it, proto, "createDocument", 2, create_document)?;
    d.def_method_s(it, proto, "hasFeature", 0, has_feature)?;
    crate::browser_lazy::tag(d, it, proto, "DOMImplementation")?;
    let ctor = d.add_fn_s_obj(it, illegal_constructor, "DOMImplementation", 0, HostFn::Ctor)?;
    let (kp, kc) = (d.key(it, "prototype")?, d.key(it, "constructor")?);
    it.define_data(ctor, kp, E::obj_raw(proto), false, false, false)?;
    it.define_data(proto, kc, E::obj_raw(ctor), true, false, true)?;
    let kg = d.key(it, "DOMImplementation")?;
    let g = it.global_object();
    if it.h_peek_own(g, kg)?.is_some_and(|p| matches!(p, crate::eng::Prop::Accessor { .. })) {
        it.define_data(g, kg, E::obj_raw(ctor), true, false, true)?;
    }
    let o = d.obj(it, proto)?;
    d.implementation.set(Some(o));
    d.fixed.borrow_mut().push(o);
    Ok(o)
}

fn implementation_global<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let v = match a.get(0) {
        Some(v) => v,
        None => {
            let o = implementation(d, s, this, a)?;
            let p = s.get_prototype_of(o)?.expect("implementation prototype");
            s.get_str(p, "constructor")?
        }
    };
    let g = s.global();
    s.define_str(g, "DOMImplementation", v, Attrs { writable: true, enumerable: false, configurable: true })?;
    Ok(v)
}

/// Installed with the late members of `Document` (`browser_dom::install_late`).
pub(crate) fn install<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>) -> Res<'s, ()> {
    d.def_get_at(s, d.p().document, "implementation", implementation, None)?;
    table!(lazy = [
        Row::both(1, "DOMImplementation", implementation_global),
        Row::both(1, "DOMParser", parser_global),
        Row::both(1, "XMLSerializer", serializer_global),
    ]);
    let g = s.global();
    d.install_rows(s, &[g], &lazy::T::<E>::V)
}

// XML MIME types use the HTML parser. Case, namespaces, errors and serialization
// follow HTML rules. This host does not implement an XML parser.
fn parse_from_string<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, _: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let text = d.arg_str_s(s, a, 0)?;
    let mime = d.arg_str_s(s, a, 1)?;
    if !["text/html", "text/xml", "application/xml", "application/xhtml+xml", "image/svg+xml"].contains(&mime.as_str()) {
        return Err(s.type_error("Invalid MIME type for DOMParser.parseFromString"));
    }
    if text.len() > d.lim.max_string_bytes { return Err(d.lim_s(s, "dom-string")); }
    s.charge(text.len() as u64 + 1)?;
    let parsed = bare_html::tree::parse(&text);
    {
        let doc = d.doc.borrow();
        d.room_s(s, &doc, parsed.len(), parsed.text_bytes())?;
        if parsed.attr_slots() > d.lim.max_attr_slots.saturating_sub(doc.live_attr_slots()) { return Err(d.lim_s(s, "dom-attrs")); }
        if doc.name_count() + parsed.new_names_in(&doc) > d.lim.max_names { return Err(d.lim_s(s, "dom-names")); }
    }
    s.charge(parsed.len() as u64)?;
    // The nodes of the copy stay in the arena until a reclaim frees them: they count towards the next collect (`Interp::gc_due`).
    s.engine().add_external(parsed.arena_bytes());
    let mut pending = vec![(parsed.root(), 0)];
    while let Some((root, base)) = pending.pop() {
        let (mut cur, mut depth) = (root, 0);
        while let Some((c, cd)) = crate::browser_dom::next_in(&parsed, root, cur, depth) {
            if base + cd > d.lim.max_tree_depth { return Err(d.lim_s(s, "dom-depth")); }
            (cur, depth) = (c, cd);
            if let Some(t) = parsed.template_content(c) { pending.push((t, base + cd)); }
        }
    }
    let mut doc = d.doc.borrow_mut();
    let root = doc.create_document();
    let mut jobs = vec![(parsed.root(), root)];
    while let Some((src, dst)) = jobs.pop() {
        let mut path = vec![dst];
        let (mut cur, mut depth) = (src, 0);
        while let Some((c, cd)) = crate::browser_dom::next_in(&parsed, src, cur, depth) {
            (cur, depth) = (c, cd);
            let n = doc.import_node(&parsed, c).expect("parsed node");
            doc.move_to(n, path[cd - 1], None);
            path.truncate(cd);
            path.push(n);
            if doc.is_html(n, "script") { d.mark_started(n); }
            if let Some(t) = parsed.template_content(c) { jobs.push((t, doc.make_template_content(n))); }
        }
    }
    drop(doc);
    d.wrap_s(s, root)
}

fn serialize_to_string<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, _: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let v = arg_s::<E>(s, a, 0);
    let n = d.nd_s(s, v)?;
    let doc = d.doc.borrow();
    let whole = !matches!(doc.data(n), bare_html::dom::NodeData::Document);
    let text = crate::browser_fragment::serialize(d, s, &doc, n, whole)?;
    d.js_str_s(s, &text)
}

/// A new object of the interface whose prototype is `protos[i]` (the `new.target` prototype when a subclass makes it).
fn construct<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, i: usize) -> Res<'s, Local<'s>> {
    let default = d.obj_local(s, d.parsing_protos.get()[i].expect("interface prototype"));
    let proto = s.proto_from_new_target(default)?;
    let proto = obj_id::<E>(s, proto).expect("a prototype is an object");
    d.obj_with_s(s, proto)
}

fn parser_construct<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, _: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    construct(d, s, 0)
}

fn serializer_construct<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, _: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    construct(d, s, 1)
}

fn interface<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, name: &str, h: HS<E>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let value = match a.get(0) {
        Some(v) => v,
        None => {
            let ctor = old::<E, _>(s, |it| make_interface(d, it, name, h))?;
            d.obj_local(s, ctor)
        }
    };
    let g = s.global();
    s.define_str(g, name, value, Attrs { writable: true, enumerable: false, configurable: true })?;
    Ok(value)
}

fn make_interface<E: Eng>(d: &Dom<E>, it: &mut E::Vm, name: &str, h: HS<E>) -> R<E, E::Obj> {
    let i = usize::from(name == "XMLSerializer");
    let p = d.obj(it, it.object_prototype())?;
    let ctor = d.add_fn_s_obj(it, h, name, 0, HostFn::CtorNewTarget)?;
    let (kp, kc) = (d.key(it, "prototype")?, d.key(it, "constructor")?);
    it.define_data(ctor, kp, E::obj_raw(p), false, false, false)?;
    it.define_data(p, kc, E::obj_raw(ctor), true, false, true)?;
    crate::browser_lazy::tag(d, it, p, name)?;
    if i == 0 { d.def_method_s(it, p, "parseFromString", 2, parse_from_string)?; }
    else { d.def_method_s(it, p, "serializeToString", 1, serialize_to_string)?; }
    let mut protos = d.parsing_protos.get();
    protos[i] = Some(p);
    d.parsing_protos.set(protos);
    d.fixed.borrow_mut().push(p);
    Ok(ctor)
}

fn parser_global<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, _: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    interface(d, s, "DOMParser", parser_construct, a)
}

fn serializer_global<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, _: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    interface(d, s, "XMLSerializer", serializer_construct, a)
}

#[cold]
pub(crate) fn doctype_proto<E: Eng>(d: &Dom<E>, it: &mut E::Vm) -> R<E, E::Obj> {
    if let Some(p) = d.parsing_protos.get()[2] { return Ok(p); }
    let p = d.obj(it, d.p().node)?;
    let ctor = d.add_fn_s_obj(it, illegal_constructor, "DocumentType", 0, HostFn::Ctor)?;
    let (kp, kc) = (d.key(it, "prototype")?, d.key(it, "constructor")?);
    it.define_data(ctor, kp, E::obj_raw(p), false, false, false)?;
    it.define_data(p, kc, E::obj_raw(ctor), true, false, true)?;
    crate::browser_lazy::tag(d, it, p, "DocumentType")?;
    d.def_get_s(it, p, "name", doctype_name, None)?;
    d.def_get_s(it, p, "publicId", doctype_empty, None)?;
    d.def_get_s(it, p, "systemId", doctype_empty, None)?;
    let mut protos = d.parsing_protos.get();
    protos[2] = Some(p);
    d.parsing_protos.set(protos);
    d.fixed.borrow_mut().push(p);
    Ok(p)
}
/// The global `DocumentType`: `v` when a script assigns it, else the constructor of the interface.
pub(crate) fn doctype_define<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, v: Option<Local<'s>>) -> Res<'s, Local<'s>> {
    let v = match v {
        Some(v) => v,
        None => {
            let p = old::<E, _>(s, |it| doctype_proto(d, it))?;
            let p = d.obj_local(s, p);
            s.get_str(p, "constructor")?
        }
    };
    let g = s.global();
    s.define_str(g, "DocumentType", v, Attrs { writable: true, enumerable: false, configurable: true })?;
    Ok(v)
}
fn doctype_name<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let n = d.nd_s(s, this)?;
    let name = match *d.doc.borrow().data(n) {
        NodeData::Doctype(t) => d.doc.borrow().text(t).to_string(),
        _ => return Err(s.type_error("Illegal invocation")),
    };
    d.js_str_s(s, &name)
}
fn doctype_empty<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    doctype_name(d, s, this, a)?;
    d.js_str_s(s, "")
}
