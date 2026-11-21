//! The old form (`Interp`, `Value`) of the helpers of `browser_dom.rs`, for the callers that have not moved to `Scope`.
//! Each wrapper runs the helper of the new form. The two forms of a hot helper (`arg`, `attr_get`, `attr_set`) are separate bodies, so a caller of the old form pays nothing.
//! A file that moves to `Scope` stops calling its wrapper here, and the wrapper goes when no file calls it.

use crate::browser::*;
use crate::browser_dom::*;
use crate::browser_scoped::*;
use bare_html::dom::NodeId;
use bare_js::eval::Interp;
use bare_js::value::{ObjId, Value};

type V = R<Value>;

pub(crate) fn attr_set(d: &Dom, it: &mut Interp<'_>, n: NodeId, name: &str, value: &str) -> R<()> {
    let mut doc = d.doc.borrow_mut();
    d.attr_room(&doc, n, name, value)?;
    it.charge(doc.attr_count(n) as u64 + 1)?;
    doc.set_attr(n, name, value);
    Ok(())
}

impl Dom {
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

    /// `def_method` for a function of the new form.
    pub(crate) fn def_method_s(&self, it: &mut Interp<'_>, target: ObjId, name: &str, len: u32, h: HS) -> R<ObjId> {
        let key = self.key(it, name)?;
        let f = self.add_fn_s_obj(it, h, name, len, bare_js_api::HostFn::Method)?;
        it.define_data(target, key, Value::Object(f), true, false, true)?;
        Ok(f)
    }
}

impl Dom {
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
