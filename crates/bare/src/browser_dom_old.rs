//! The old form (`Interp`, `Value`) of the helpers of `browser_dom.rs`, for the callers that have not moved to `Scope`.
//! Each wrapper runs the helper of the new form. The two forms of a hot helper (`arg`, `attr_get`, `attr_set`) are separate bodies, so a caller of the old form pays nothing.
//! A file that moves to `Scope` stops calling its wrapper here, and the wrapper goes when no file calls it.

use crate::browser::*;
use crate::browser_dom::*;
use crate::browser_scoped::*;
use bare_html::dom::{NodeData, NodeId};
use bare_js::eval::{Error, Interp};
use bare_js::value::{ObjId, Value};

type V = R<Value>;

pub(crate) fn arg(a: &[Value], i: usize) -> Value {
    a.get(i).copied().unwrap_or(Value::Undefined)
}

pub(crate) fn attr_get(d: &Dom, it: &mut Interp<'_>, n: NodeId, name: &str) -> V {
    let s = {
        let doc = d.doc.borrow();
        doc.find_atom(name).and_then(|a| doc.attr(n, a)).map(str::to_string)
    };
    match s {
        Some(s) => d.js_str(it, &s),
        None => Ok(Value::Null),
    }
}

pub(crate) fn attr_set(d: &Dom, it: &mut Interp<'_>, n: NodeId, name: &str, value: &str) -> R<()> {
    let mut doc = d.doc.borrow_mut();
    d.attr_room(&doc, n, name, value)?;
    it.charge(doc.attr_count(n) as u64 + 1)?;
    doc.set_attr(n, name, value);
    Ok(())
}

impl Dom {
    pub(crate) fn el(&self, it: &mut Interp<'_>, this: Value) -> R<NodeId> {
        let n = self.nd(it, this)?;
        if self.is_element(n) { Ok(n) } else { Err(it.type_error("Illegal invocation")) }
    }

    /// Set the dirty value. A value that differs puts the caret at the end (HTML: setting `value`), and when that
    /// moves the selection of a text control, schedules its `selectionchange`. `it` is the JS caller that pays for
    /// the scans; the test driver has none (see `Dom::selection_after_value`).
    pub(crate) fn put_input_value(&self, it: Option<&mut Interp<'_>>, n: NodeId, v: String) -> R<()> {
        let changed = self.with_input_value(n, |old| old != v) != Some(false);
        let moved = if changed { self.selection_after_value(it, n, &v)? } else { false };
        self.value_room(n, v.len())?;
        self.selection_room(n, moved, false)?; // every cap is checked before the first write
        self.queue_selection_tasks(n, moved, false, false)?;
        self.write_value(n, v, changed)
    }

    pub(crate) fn query(&self, it: &mut Interp<'_>, scope: NodeId, text: &str, all: bool) -> R<Vec<NodeId>> {
        run(it, |s| self.query_s(s, scope, text, all))
    }

    pub(crate) fn depth_of(&self, it: &mut Interp<'_>, n: NodeId) -> R<usize> {
        run(it, |s| self.depth_of_s(s, n))
    }

    pub(crate) fn insert_checked(&self, it: &mut Interp<'_>, parent: NodeId, node: NodeId, before: Option<NodeId>, op: Op) -> R<()> {
        run(it, |s| self.insert_checked_s(s, parent, node, before, op))
    }

    pub(crate) fn check_insert(&self, it: &mut Interp<'_>, parent: NodeId, node: NodeId, before: Option<NodeId>, replacing: Option<NodeId>, op: Op) -> R<bool> {
        run(it, |s| self.check_insert_s(s, parent, node, before, replacing, op))
    }

    pub(crate) fn insert_valid(&self, it: &mut Interp<'_>, parent: NodeId, node: NodeId, before: Option<NodeId>, frag: bool, op: Op) -> R<()> {
        run(it, |s| self.insert_valid_s(s, parent, node, before, frag, op))
    }

    /// The error of a refused insert. `op` puts the name of the call in front, as Chrome does: "Failed to execute 'append' on 'Document': ...".
    pub(crate) fn refuse(&self, it: &mut Interp<'_>, op: Op, parent: NodeId, name: &str, reason: &str) -> Error {
        if op.0.is_empty() {
            return self.dom_err(it, name, reason);
        }
        let iface = match (op.1, self.data(parent)) {
            ("", NodeData::Document) => "Document",
            ("", NodeData::ShadowRoot) => "DocumentFragment",
            ("", _) => "Element",
            (i, _) => i,
        };
        self.dom_err(it, name, &format!("Failed to execute '{}' on '{iface}': {reason}", op.0))
    }

    /// `def_method` for a function of the new form.
    pub(crate) fn def_method_s(&self, it: &mut Interp<'_>, target: ObjId, name: &str, len: u32, h: HS) -> R<ObjId> {
        let key = self.key(it, name)?;
        let f = self.add_fn_s_obj(it, h, name, len, bare_js_api::HostFn::Method)?;
        it.define_data(target, key, Value::Object(f), true, false, true)?;
        Ok(f)
    }
}

impl Dom {
    /// `def_get` for a getter of the new form and a setter of the old form (the setter's file has not moved).
    pub(crate) fn def_get_sh(&self, it: &mut Interp<'_>, target: ObjId, name: &str, get: HS, set: H) -> R<()> {
        let key = self.key(it, name)?;
        let g = self.add_fn_s_obj(it, get, &format!("get {name}"), 0, bare_js_api::HostFn::Method)?;
        let s = self.add_fn(it, set, &format!("set {name}"), 1, false)?;
        self.define_accessor(it, target, key, g, Some(s))
    }

    /// `def_get` for functions of the new form.
    pub(crate) fn def_get_s(&self, it: &mut Interp<'_>, target: ObjId, name: &str, get: HS, set: Option<HS>) -> R<()> {
        let key = self.key(it, name)?;
        let g = self.add_fn_s_obj(it, get, &format!("get {name}"), 0, bare_js_api::HostFn::Method)?;
        let s = match set {
            Some(h) => Some(self.add_fn_s_obj(it, h, &format!("set {name}"), 1, bare_js_api::HostFn::Method)?),
            None => None,
        };
        self.define_accessor(it, target, key, g, s)
    }
}

/// The prototype of a new `<link>` wrapper. The first one adds the reflected members that the start-up table (`DEFS`) does not hold, so a
/// page without a `<link>` does not pay for them.
pub(crate) fn link_proto(d: &Dom, it: &mut Interp<'_>) -> R<ObjId> {
    let p = d.p().link;
    if !d.link_done.replace(true) {
        crate::browser_reflect::reflect!(d, it, p, &["link"], {
            "media" => "media": Str, "type" => "type": Str, "integrity" => "integrity": Str, "hreflang" => "hreflang": Str, "disabled" => "disabled": Bool,
        });
    }
    Ok(p)
}

/// `before`, `after`, `replaceWith`, `replaceChildren`, `insertAdjacent*`, `createDocumentFragment`: installed after every other global, so their function ids do not move a host function that came before.
/// Deviation, for the cost of a realm: `Element` and `CharacterData` share one function object for each ChildNode method (Chrome has two).
pub(crate) fn install_late(d: &Dom, it: &mut Interp<'_>) -> R<()> {
    d.def_method_s(it, d.p().document, "createDocumentFragment", 0, create_document_fragment)?;
    let hs: [(&str, HS); 3] = [("before", |d, s, t, a| child_node(d, s, t, a, 0)), ("after", |d, s, t, a| child_node(d, s, t, a, 1)), ("replaceWith", |d, s, t, a| child_node(d, s, t, a, 2))];
    for (name, h) in hs {
        let f = d.def_method_s(it, d.p().element, name, 0, h)?;
        let k = d.key(it, name)?;
        it.define_data(d.p().chardata, k, Value::Object(f), true, false, true)?;
    }
    // `replaceChildren` is one function object on Element, DocumentFragment and Document (a deviation, for the cost of a realm: Chrome has two).
    let f = d.def_method_s(it, d.p().element, "replaceChildren", 0, replace_children)?;
    let k = d.key(it, "replaceChildren")?;
    it.define_data(d.p().fragment, k, Value::Object(f), true, false, true)?;
    it.define_data(d.p().document, k, Value::Object(f), true, false, true)?;
    d.def_method_s(it, d.p().element, "insertAdjacentElement", 2, insert_adjacent_element)?;
    d.def_method_s(it, d.p().element, "insertAdjacentText", 2, insert_adjacent_text)?;
    d.def_method_s(it, d.p().element, "insertAdjacentHTML", 2, crate::browser_fragment::insert_adjacent_html)?;
    crate::browser_implementation::install(d, it)
}

pub(crate) fn new_fragment(d: &Dom, it: &mut Interp<'_>, proto: Option<ObjId>) -> V {
    run_v(it, |s| {
        let p = proto.map(|p| d.obj_local(s, p));
        new_fragment_s(d, s, p)
    })
}

pub(crate) fn clone_tree(d: &Dom, it: &mut Interp<'_>, n: NodeId, deep: bool, import: bool) -> R<NodeId> {
    run(it, |s| clone_tree_s(d, s, n, deep, import))
}

pub(crate) fn text_content(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let n = d.nd(it, this)?;
    let s = {
        let doc = d.doc.borrow();
        match *doc.data(n) {
            NodeData::Document | NodeData::Doctype(_) => return Ok(Value::Null),
            NodeData::Text(sp) | NodeData::Comment(sp) => doc.text(sp).to_string(),
            NodeData::Element { .. } | NodeData::ShadowRoot => {
                let mut s = String::new();
                let (mut cur, mut depth) = (n, 0);
                while let Some((c, cd)) = next_in(&doc, n, cur, depth) {
                    it.charge(1)?;
                    (cur, depth) = (c, cd);
                    if let NodeData::Text(sp) = *doc.data(c) {
                        let text = doc.text(sp);
                        if text.len() > d.lim.max_string_bytes.saturating_sub(s.len()) {
                            return Err(d.lim("dom-string"));
                        }
                        s.push_str(text);
                    }
                }
                s
            }
        }
    };
    d.js_str(it, &s)
}

pub(crate) fn set_text_content(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let n = d.nd(it, this)?;
    let v = arg(a, 0);
    let s = if matches!(v, Value::Null | Value::Undefined) { String::new() } else { d.string_arg(it, v)? };
    match d.data(n) {
        NodeData::Element { .. } | NodeData::ShadowRoot => {
            let r = d.mo_group(Some(n), || {
                let mut doc = d.doc.borrow_mut();
                d.room(&doc, !s.is_empty() as usize, s.len())?;
                // Charge before the tree changes: a step limit then leaves the old children, never half of them.
                it.charge(doc.children(n).count() as u64)?;
                d.iter_told_all(&doc, n);
                while let Some(c) = doc.children(n).next() {
                    doc.detach(c);
                }
                if !s.is_empty() {
                    let t = doc.create_text(&s);
                    doc.move_to(t, n, None);
                }
                Ok(())
            });
            d.iter_told_end();
            r?;
        }
        NodeData::Text(_) | NodeData::Comment(_) => {
            let mut doc = d.doc.borrow_mut();
            d.room(&doc, 0, s.len())?;
            d.range_set_data(&doc, n, &s);
            doc.set_text(n, &s);
        }
        NodeData::Document | NodeData::Doctype(_) => {}
    }
    Ok(Value::Undefined)
}

pub(crate) fn body(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    html_child(d, it, this, &["body", "frameset"])
}

fn html_child(d: &Dom, it: &mut Interp<'_>, this: Value, names: &[&str]) -> V {
    let root = d.nd(it, this)?;
    let r = {
        let doc = d.doc.borrow();
        doc.children(root).find(|&c| matches!(doc.data(c), NodeData::Element { .. })).and_then(|html| doc.children(html).find(|&c| is_named(&doc, c, names)))
    };
    d.wrap_opt(it, r)
}


pub(crate) fn remove_attribute(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let n = d.el(it, this)?;
    let name = d.string_arg(it, arg(a, 0))?;
    let mut doc = d.doc.borrow_mut();
    it.charge(doc.attr_count(n) as u64 + 1)?;
    let name = crate::browser_namespace::attr_key(&doc, n, &name);
    if let Some(at) = doc.find_attr_qname(n, &name) {
        doc.remove_attr_atom(n, at);
    }
    Ok(Value::Undefined)
}
