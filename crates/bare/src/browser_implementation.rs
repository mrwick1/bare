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
//!   `createDocument`, `createDocumentType` and `DOMParser` are absent.

use crate::browser::*;
use bare_js::eval::Interp;
use bare_js::value::Value;

fn arg(a: &[Value], i: usize) -> Value {
    a.get(i).copied().unwrap_or(Value::Undefined)
}

/// `implementation.createHTMLDocument(title)`.
fn create_html_document(d: &Dom, it: &mut Interp<'_>, _: Value, a: &[Value]) -> R<Value> {
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

fn has_feature(_: &Dom, _: &mut Interp<'_>, _: Value, _: &[Value]) -> R<Value> {
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
    let o = d.obj(it, proto)?;
    d.implementation.set(Some(o));
    d.fixed.borrow_mut().push(o);
    Ok(Value::Object(o))
}

/// Installed with the late members of `Document` (`browser_dom::install_late`).
pub(crate) fn install(d: &Dom, it: &mut Interp<'_>) -> R<()> {
    d.def_get(it, d.p().document, "implementation", implementation, None)
}
