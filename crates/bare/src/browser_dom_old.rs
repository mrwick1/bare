//! The old form (`Interp`, `Value`) of the helpers of `browser_dom.rs`, for the callers that have not moved to `Scope`.
//! Each wrapper runs the helper of the new form. The two forms of a hot helper (`arg`, `attr_get`, `attr_set`) are separate bodies, so a caller of the old form pays nothing.
//! A file that moves to `Scope` stops calling its wrapper here, and the wrapper goes when no file calls it.

use crate::eng::{Eng, Ops};
use crate::browser::*;
use crate::browser_dom::*;
use crate::browser_scoped::*;
use bare_html::dom::NodeId;
use bare_js_api::Attrs;

type V<E: Eng> = R<E, E::Raw>;

pub(crate) fn attr_set<E: Eng>(d: &Dom<E>, it: &mut E::Vm, n: NodeId, name: &str, value: &str) -> R<E, ()> {
    let mut doc = d.doc.borrow_mut();
    d.attr_room(&doc, n, name, value)?;
    it.charge_o(doc.attr_count(n) as u64 + 1)?;
    doc.set_attr(n, name, value);
    Ok(())
}

impl<E: Eng> Dom<E> {
    /// Set the dirty value. A value that differs puts the caret at the end (HTML: setting `value`), and when that
    /// moves the selection of a text control, schedules its `selectionchange`. `it` is the JS caller that pays for
    /// the scans; the test driver has none (see `Dom::selection_after_value`).
    pub(crate) fn put_input_value(&self, it: Option<&mut E::Vm>, n: NodeId, v: String) -> R<E, ()> {
        let changed = self.with_input_value(n, |old| old != v) != Some(false);
        let moved = if changed { self.selection_after_value(it, n, &v)? } else { false };
        self.value_room(n, v.len())?;
        self.selection_room(n, moved, false)?; // every cap is checked before the first write
        self.queue_selection_tasks(n, moved, false, false)?;
        self.write_value(n, v, changed)
    }

    pub(crate) fn query(&self, it: &mut E::Vm, scope: NodeId, text: &str, all: bool) -> R<E, Vec<NodeId>> {
        run::<E, _>(it, |s| self.query_s(s, scope, text, all))
    }

    pub(crate) fn depth_of(&self, it: &mut E::Vm, n: NodeId) -> R<E, usize> {
        run::<E, _>(it, |s| self.depth_of_s(s, n))
    }

    /// `def_method` for a function of the new form.
    pub(crate) fn def_method_s(&self, it: &mut E::Vm, target: E::Obj, name: &str, len: u32, h: HS<E>) -> R<E, E::Obj> {
        let key = self.key(it, name)?;
        let f = self.add_fn_s_obj(it, h, name, len, bare_js_api::HostFn::Method)?;
        it.define_data(target, key, E::obj_raw(f), true, false, true)?;
        Ok(f)
    }
}

impl<E: Eng> Dom<E> {
    /// `def_get` for functions of the new form.
    pub(crate) fn def_get_s(&self, it: &mut E::Vm, target: E::Obj, name: &str, get: HS<E>, set: Option<HS<E>>) -> R<E, ()> {
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
pub(crate) fn link_proto<E: Eng>(d: &Dom<E>, it: &mut E::Vm) -> R<E, E::Obj> {
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
pub(crate) fn install_late<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>) -> Res<'s, ()> {
    d.def_method_at(s, d.p().document, "createDocumentFragment", 0, create_document_fragment)?;
    let hs: [(&str, HS<E>); 3] = [("before", |d, s, t, a| child_node(d, s, t, a, 0)), ("after", |d, s, t, a| child_node(d, s, t, a, 1)), ("replaceWith", |d, s, t, a| child_node(d, s, t, a, 2))];
    for (name, h) in hs {
        let f = d.def_method_at(s, d.p().element, name, 0, h)?;
        let chardata = d.obj_local(s, d.p().chardata);
        s.define_str(chardata, name, f, Attrs::METHOD)?;
    }
    // `replaceChildren` is one function object on Element, DocumentFragment and Document (a deviation, for the cost of a realm: Chrome has two).
    let f = d.def_method_at(s, d.p().element, "replaceChildren", 0, replace_children)?;
    for o in [d.p().fragment, d.p().document] {
        let o = d.obj_local(s, o);
        s.define_str(o, "replaceChildren", f, Attrs::METHOD)?;
    }
    table!(rows1 = [
        Row::method(1, "insertAdjacentElement", 2, insert_adjacent_element),
        Row::method(1, "insertAdjacentText", 2, insert_adjacent_text),
        Row::method(1, "insertAdjacentHTML", 2, crate::browser_fragment::insert_adjacent_html),
    ]);
    let targets = [d.obj_local(s, d.p().element)];
    d.install_rows(s, &targets, &rows1::T::<E>::V)?;
    crate::browser_implementation::install(d, s)
}

pub(crate) fn new_fragment<E: Eng>(d: &Dom<E>, it: &mut E::Vm, proto: Option<E::Obj>) -> V<E> {
    run_v(it, |s| {
        let p = proto.map(|p| d.obj_local(s, p));
        new_fragment_s(d, s, p)
    })
}
