//! `document.implementation` (`DOMImplementation`): `createHTMLDocument(title)` and `hasFeature()`.
//!
//! Cause: jQuery makes a document at load (`document.implementation.createHTMLDocument("")`, `support.createHTMLDocument`) and again in
//! `parseHTML`. Without `implementation` the `jquery` module of Wikipedia threw at load and the page lost every module that needs it.
//!
//! - The object is made on the first read of `document.implementation` (an accessor of `Document.prototype`), and it is the same object after.
//! - `createHTMLDocument(title)` makes a `Document` node outside the tree, with `html`, `head`, an optional `title` and `body`. The `Document`
//!   getters (`documentElement`, `head`, `body`, `createElement`, ...) work on it. It lives while a script holds its wrapper.
//! - Deviations: the new document has no doctype, `defaultView` and `location`, and a node made by its `createElement` has `ownerDocument`
//!   `document`, not this one (the arena has one owner document, as for the contents of a `template`). Scripts in the new document never run.
//!   `createDocument` and `createDocumentType` are absent.

use crate::browser::*;
use bare_js::eval::Interp;
use bare_js::value::{ObjId, Value};
use bare_html::dom::NodeData;

fn arg(a: &[Value], i: usize) -> Value {
    a.get(i).copied().unwrap_or(Value::Undefined)
}

/// `implementation.createHTMLDocument(title)`.
fn create_html_document(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> R<Value> {
    receiver(d, it, this)?;
    let title = match arg(a, 0) {
        Value::Undefined => None,
        v => Some(d.string_arg(it, v)?),
    };
    let n = {
        let mut doc = d.doc.borrow_mut();
        d.room(&doc, 5, title.as_ref().map_or(0, |t| t.len()))?;
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
    d.wrap(it, n)
}

fn receiver(d: &Dom, it: &mut Interp<'_>, this: Value) -> R<()> {
    if matches!(this, Value::Object(o) if d.implementation.get() == Some(o)) { return Ok(()); }
    Err(it.type_error("Illegal invocation"))
}

fn has_feature(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> R<Value> {
    receiver(d, it, this)?;
    Ok(Value::Bool(true))
}

/// The getter of `document.implementation`.
fn implementation(d: &Dom, it: &mut Interp<'_>, _: Value, _: &[Value]) -> R<Value> {
    if let Some(o) = d.implementation.get() {
        return Ok(Value::Object(o));
    }
    let proto = d.obj(it, it.object_prototype())?;
    d.def_method(it, proto, "createHTMLDocument", 0, create_html_document)?;
    d.def_method(it, proto, "hasFeature", 0, has_feature)?;
    crate::browser_lazy::tag(d, it, proto, "DOMImplementation")?;
    let ctor = d.add_fn(it, illegal_constructor, "DOMImplementation", 0, true)?;
    let (kp, kc) = (d.key(it, "prototype")?, d.key(it, "constructor")?);
    it.define_data(ctor, kp, Value::Object(proto), false, false, false)?;
    it.define_data(proto, kc, Value::Object(ctor), true, false, true)?;
    let kg = d.key(it, "DOMImplementation")?;
    let g = it.global_object();
    if it.heap.peek_own_property(g, kg).map_err(|e| it.heap_error(e))?.is_some_and(|p| matches!(p, bare_js::heap::Property::Accessor { .. })) {
        it.define_data(g, kg, Value::Object(ctor), true, false, true)?;
    }
    let o = d.obj(it, proto)?;
    d.implementation.set(Some(o));
    d.fixed.borrow_mut().push(o);
    Ok(Value::Object(o))
}

fn implementation_global(d: &Dom, it: &mut Interp<'_>, _: Value, a: &[Value]) -> R<Value> {
    let v = if let Some(v) = a.first() { *v } else {
        let Value::Object(o) = implementation(d, it, Value::Undefined, &[])? else { unreachable!() };
        let p = it.heap.get_prototype_of(o).map_err(|e| it.heap_error(e))?.expect("implementation prototype");
        let k = d.key(it, "constructor")?;
        it.get_value(Value::Object(p), k)?
    };
    let k = d.key(it, "DOMImplementation")?;
    it.define_data(it.global_object(), k, v, true, false, true)?;
    Ok(v)
}

/// Installed with the late members of `Document` (`browser_dom::install_late`).
pub(crate) fn install(d: &Dom, it: &mut Interp<'_>) -> R<()> {
    d.def_get(it, d.p().document, "implementation", implementation, None)?;
    crate::browser_lazy::install(d, it, &[("DOMImplementation", implementation_global), ("DOMParser", parser_global), ("XMLSerializer", serializer_global)])
}

// XML MIME types use the HTML parser. Case, namespaces, errors and serialization
// follow HTML rules. This host does not implement an XML parser.
fn parse_from_string(d: &Dom, it: &mut Interp<'_>, _: Value, a: &[Value]) -> R<Value> {
    let text = d.string_arg(it, arg(a, 0))?;
    let mime = d.string_arg(it, arg(a, 1))?;
    if !["text/html", "text/xml", "application/xml", "image/svg+xml"].contains(&mime.as_str()) {
        return Err(it.type_error("Invalid MIME type for DOMParser.parseFromString"));
    }
    if text.len() > d.lim.max_string_bytes { return Err(d.lim("dom-string")); }
    it.charge(text.len() as u64 + 1)?;
    let parsed = bare_html::tree::parse(&text);
    {
        let doc = d.doc.borrow();
        d.room(&doc, parsed.len(), parsed.text_bytes())?;
        if parsed.attr_slots() > d.lim.max_attr_slots.saturating_sub(doc.attr_slots()) { return Err(d.lim("dom-attrs")); }
        if doc.name_count() + parsed.new_names_in(&doc) > d.lim.max_names { return Err(d.lim("dom-names")); }
    }
    it.charge(parsed.len() as u64)?;
    let mut pending = vec![(parsed.root(), 0)];
    while let Some((root, base)) = pending.pop() {
        let (mut cur, mut depth) = (root, 0);
        while let Some((c, cd)) = crate::browser_dom::next_in(&parsed, root, cur, depth) {
            if base + cd > d.lim.max_tree_depth { return Err(d.lim("dom-depth")); }
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
    d.wrap(it, root)
}

fn serialize_to_string(d: &Dom, it: &mut Interp<'_>, _: Value, a: &[Value]) -> R<Value> {
    let n = d.nd(it, arg(a, 0))?;
    let doc = d.doc.borrow();
    let text = crate::browser_fragment::serialize(d, it, &doc, n, !matches!(doc.data(n), bare_html::dom::NodeData::Document))?;
    d.js_str(it, &text)
}

fn parser_construct(d: &Dom, it: &mut Interp<'_>, _: Value, _: &[Value]) -> R<Value> {
    let proto = it.host_proto_from_new_target(d.parsing_protos.get()[0].expect("parser prototype"))?;
    Ok(Value::Object(d.obj(it, proto)?))
}

fn serializer_construct(d: &Dom, it: &mut Interp<'_>, _: Value, _: &[Value]) -> R<Value> {
    let proto = it.host_proto_from_new_target(d.parsing_protos.get()[1].expect("serializer prototype"))?;
    Ok(Value::Object(d.obj(it, proto)?))
}

fn interface(d: &Dom, it: &mut Interp<'_>, name: &str, h: H, a: &[Value]) -> R<Value> {
    let value = if let Some(v) = a.first() { *v } else {
        let i = usize::from(name == "XMLSerializer");
        let p = d.obj(it, it.object_prototype())?;
        let ctor = d.add_ctor_nt(it, h, name, 0)?;
        let (kp, kc) = (d.key(it, "prototype")?, d.key(it, "constructor")?);
        it.define_data(ctor, kp, Value::Object(p), false, false, false)?;
        it.define_data(p, kc, Value::Object(ctor), true, false, true)?;
        crate::browser_lazy::tag(d, it, p, name)?;
        if i == 0 { d.def_method(it, p, "parseFromString", 2, parse_from_string)?; }
        else { d.def_method(it, p, "serializeToString", 1, serialize_to_string)?; }
        let mut protos = d.parsing_protos.get();
        protos[i] = Some(p);
        d.parsing_protos.set(protos);
        d.fixed.borrow_mut().push(p);
        Value::Object(ctor)
    };
    let k = d.key(it, name)?;
    it.define_data(it.global_object(), k, value, true, false, true)?;
    Ok(value)
}

fn parser_global(d: &Dom, it: &mut Interp<'_>, _: Value, a: &[Value]) -> R<Value> {
    interface(d, it, "DOMParser", parser_construct, a)
}

fn serializer_global(d: &Dom, it: &mut Interp<'_>, _: Value, a: &[Value]) -> R<Value> {
    interface(d, it, "XMLSerializer", serializer_construct, a)
}

#[cold]
pub(crate) fn doctype_proto(d: &Dom, it: &mut Interp<'_>) -> R<ObjId> {
    if let Some(p) = d.parsing_protos.get()[2] { return Ok(p); }
    let p = d.obj(it, d.p().node)?;
    let ctor = d.add_fn(it, illegal_constructor, "DocumentType", 0, true)?;
    let (kp, kc) = (d.key(it, "prototype")?, d.key(it, "constructor")?);
    it.define_data(ctor, kp, Value::Object(p), false, false, false)?;
    it.define_data(p, kc, Value::Object(ctor), true, false, true)?;
    crate::browser_lazy::tag(d, it, p, "DocumentType")?;
    d.def_get(it, p, "name", doctype_name, None)?;
    d.def_get(it, p, "publicId", doctype_empty, None)?;
    d.def_get(it, p, "systemId", doctype_empty, None)?;
    let mut protos = d.parsing_protos.get();
    protos[2] = Some(p);
    d.parsing_protos.set(protos);
    d.fixed.borrow_mut().push(p);
    Ok(p)
}
pub(crate) fn doctype_global(d: &Dom, it: &mut Interp<'_>, _: Value, a: &[Value]) -> R<Value> {
    let v = if let Some(v) = a.first() { *v } else {
        let p = doctype_proto(d, it)?;
        let k = d.key(it, "constructor")?;
        it.get_value(Value::Object(p), k)?
    };
    let k = d.key(it, "DocumentType")?;
    it.define_data(it.global_object(), k, v, true, false, true)?;
    Ok(v)
}
fn doctype_name(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> R<Value> {
    let n = d.nd(it, this)?;
    let name = match *d.doc.borrow().data(n) {
        NodeData::Doctype(s) => d.doc.borrow().text(s).to_string(),
        _ => return Err(it.type_error("Illegal invocation")),
    };
    d.js_str(it, &name)
}
fn doctype_empty(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> R<Value> {
    doctype_name(d, it, this, a)?;
    d.js_str(it, "")
}
