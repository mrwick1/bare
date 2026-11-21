//! `HTMLSlotElement` (`name`, `assignedNodes`, `assignedElements`) and `assignedSlot` of elements and text (DOM Standard, "Slottable").
//! Slot assignment is `Document::assigned_nodes` and `assigned_slot` (`crates/html/src/dom/slots.rs`); this module only shows it to scripts.
//! `assignedNodes({flatten: true})` follows "find flattened slottables": a nested slot is replaced by what it takes, and a slot that takes
//! nothing gives its fallback children (elements and text). `assignedSlot` is `null` when the shadow root is closed.
//! Not built: `assign()` and manual slot assignment (`attachShadow` refuses `slotAssignment: "manual"`), and `slotchange` events.

use crate::eng::Eng;
use crate::browser::*;
use crate::browser_scoped::*;
use bare_js_api::{Args, Local, Type};
use crate::browser_dom as nd;
use bare_html::dom::{NodeData, NodeId};

pub(crate) fn members<E: Eng>(d: &Dom<E>, it: &mut E::Vm, o: E::Obj) -> R<E, ()> {
    d.def_get_s(it, o, "name", name, Some(set_name))?;
    d.def_method_s(it, o, "assignedNodes", 0, |d, s, t, a| assigned(d, s, t, a, false))?;
    d.def_method_s(it, o, "assignedElements", 0, |d, s, t, a| assigned(d, s, t, a, true))?;
    Ok(())
}

/// The `<slot>` of `this`, else `Illegal invocation`.
fn slot<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>) -> Res<'s, NodeId> {
    let n = d.el_s(s, this)?;
    if d.doc.borrow().is_html(n, "slot") { Ok(n) } else { Err(s.type_error("Illegal invocation")) }
}

fn name<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let n = slot(d, s, this)?;
    let st = {
        let doc = d.doc.borrow();
        doc.find_atom("name").and_then(|a| doc.attr(n, a)).unwrap_or("").to_string()
    };
    d.js_str_s(s, &st)
}

fn set_name<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let n = slot(d, s, this)?;
    let st = d.arg_str_s(s, a, 0)?;
    nd::attr_set_s(d, s, n, "name", &st)?;
    Ok(s.undefined())
}

/// `assignedNodes` and `assignedElements` (`only_elements`).
fn assigned<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>, only_elements: bool) -> Res<'s, Local<'s>> {
    let n = slot(d, s, this)?;
    let opts = arg_s::<E>(s, a, 0);
    let flatten = match s.type_of(opts) {
        Type::Undefined | Type::Null => false,
        Type::Object | Type::Function => {
            let v = s.get_str(opts, "flatten")?;
            s.to_boolean(v)
        }
        _ => return Err(s.type_error("Failed to execute 'assignedNodes' on 'HTMLSlotElement': The provided value is not of type 'AssignedNodesOptions'.")),
    };
    let out = {
        let doc = d.doc.borrow();
        let in_shadow = |st: NodeId| doc.shadow_host(doc.tree_root(st)).is_some();
        // What a slot gives when flattened: what it takes, else its fallback children.
        let own = |st: NodeId| {
            let nodes = doc.assigned_nodes(st);
            if flatten && nodes.is_empty() && in_shadow(st) {
                return doc.children(st).filter(|&c| matches!(doc.data(c), NodeData::Element { .. } | NodeData::Text(_))).collect();
            }
            nodes
        };
        let (mut out, mut work) = (Vec::new(), own(n));
        work.reverse();
        while let Some(c) = work.pop() {
            s.charge(1)?;
            if flatten && doc.is_html(c, "slot") && in_shadow(c) {
                work.extend(own(c).into_iter().rev());
            } else if !only_elements || matches!(doc.data(c), NodeData::Element { .. }) {
                out.push(c);
            }
        }
        out
    };
    nd::wrap_all_s(d, s, &out)
}

/// `Element.assignedSlot` and `Text.assignedSlot`: the slot that takes the node, `null` for a closed shadow root and for a node that no slot takes.
pub(crate) fn assigned_slot<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let n = d.nd_s(s, this)?;
    let st = {
        let doc = d.doc.borrow();
        doc.assigned_slot(n).filter(|&st| doc.shadow_is_open(doc.tree_root(st)))
    };
    d.wrap_opt_s(s, st)
}
