//! `document.implementation`: `DOMImplementation.createHTMLDocument(title)` and `hasFeature()`.
//!
//! `createHTMLDocument` makes a `Document` node in the arena of the page, with no parent, like the inert document of a `template`
//! (`browser_custom.rs`). It holds a doctype, `html`, `head`, `title` (only when the argument is not `undefined`; its text is the argument) and
//! `body`. Its nodes are not connected, so the page never lays them out, styles them or runs their scripts, and `body.innerHTML = ...`, `querySelector`
//! and `getElementById` work on it as on any detached tree. Scripts: a `script` that a script inserts into it does not run (it is not connected).
//!
//! Deviations from Chrome:
//! - `createElement` on that document is the one of the page document: a new element has the page document as `ownerDocument` until it is inserted.
//! - `defaultView` is `null`, `URL` and `location` are those of the page (Chrome gives `about:blank`-like values), `contentType` is not read.
//! - `createDocument` and `createDocumentType` do not exist, and `DOMImplementation` is not a global.

use crate::browser::*;
use bare_html::dom::NodeId;
use bare_js::eval::Interp;
use bare_js::value::{ObjId, Value};
use std::cell::Cell;

type V = R<Value>;

#[derive(Default)]
pub(crate) struct Impl {
    /// The `DOMImplementation` object, made by the first read of `document.implementation`.
    obj: Cell<Option<ObjId>>,
    /// Did a script make a document? Then `ownerDocument` looks for the top of the tree.
    pub(crate) made: Cell<bool>,
}

pub(crate) fn roots(d: &Dom, out: &mut Vec<Value>) {
    out.extend(d.imp.obj.get().map(Value::Object));
}

/// `document.implementation`: the same object each time, for every document.
pub(crate) fn implementation(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    d.nd(it, this)?;
    if let Some(o) = d.imp.obj.get() {
        return Ok(Value::Object(o));
    }
    let proto = d.obj(it, it.object_prototype())?;
    crate::browser_lazy::tag(d, it, proto, "DOMImplementation")?;
    d.def_method(it, proto, "createHTMLDocument", 0, create_html_document)?;
    d.def_method(it, proto, "hasFeature", 0, has_feature)?;
    let o = d.obj(it, proto)?;
    d.imp.obj.set(Some(o));
    Ok(Value::Object(o))
}

fn check(d: &Dom, it: &mut Interp<'_>, this: Value) -> R<()> {
    match (this, d.imp.obj.get()) {
        (Value::Object(a), Some(b)) if a == b => Ok(()),
        _ => Err(it.type_error("Illegal invocation")),
    }
}

/// Always true, as the standard says.
fn has_feature(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    check(d, it, this)?;
    Ok(Value::Bool(true))
}

fn create_html_document(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    check(d, it, this)?;
    let title = match a.first() {
        None | Some(Value::Undefined) => None,
        Some(&v) => Some(d.string_arg(it, v)?),
    };
    let n = {
        let mut doc = d.doc.borrow_mut();
        d.room(&doc, 7, title.as_ref().map_or(0, String::len))?;
        let root = doc.create_document();
        let dt = doc.create_doctype("html");
        doc.move_to(dt, root, None);
        let html = doc.create_element("html");
        doc.move_to(html, root, None);
        let head = doc.create_element("head");
        doc.move_to(head, html, None);
        if let Some(t) = title {
            let el = doc.create_element("title");
            doc.move_to(el, head, None);
            if !t.is_empty() {
                let text = doc.create_text(&t);
                doc.move_to(text, el, None);
            }
        }
        let body = doc.create_element("body");
        doc.move_to(body, html, None);
        root
    };
    d.imp.made.set(true);
    d.wrap(it, n)
}

impl Dom {
    /// The top of the tree of `n` when it is a `Document` that is not the one of the page (made by `createHTMLDocument`).
    pub(crate) fn detached_document(&self, n: NodeId) -> Option<NodeId> {
        if !self.imp.made.get() {
            return None;
        }
        let doc = self.doc.borrow();
        let mut cur = n;
        while let Some(p) = doc.parent(cur) {
            cur = p;
        }
        (cur != doc.root() && matches!(doc.data(cur), bare_html::dom::NodeData::Document)).then_some(cur)
    }
}
