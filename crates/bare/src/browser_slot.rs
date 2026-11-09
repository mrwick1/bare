//! `HTMLSlotElement` (`name`, `assignedNodes`, `assignedElements`) and `assignedSlot` of elements and text (DOM Standard, "Slottable").
//! Slot assignment is `Document::assigned_nodes` and `assigned_slot` (`crates/html/src/dom/slots.rs`); this module only shows it to scripts.
//! `assignedNodes({flatten: true})` follows "find flattened slottables": a nested slot is replaced by what it takes, and a slot that takes
//! nothing gives its fallback children (elements and text). `assignedSlot` is `null` when the shadow root is closed.
//! Not built: `assign()` and manual slot assignment (`attachShadow` refuses `slotAssignment: "manual"`), and `slotchange` events.

use crate::browser::*;
use crate::browser_dom as nd;
use crate::browser_platform::{V, arg};
use bare_html::dom::{NodeData, NodeId};
use bare_js::eval::Interp;
use bare_js::value::{ObjId, Value};

pub(crate) fn members(d: &Dom, it: &mut Interp<'_>, o: ObjId) -> R<()> {
    d.def_get(it, o, "name", name, Some(set_name))?;
    d.def_method(it, o, "assignedNodes", 0, |d, it, t, a| assigned(d, it, t, a, false))?;
    d.def_method(it, o, "assignedElements", 0, |d, it, t, a| assigned(d, it, t, a, true))?;
    Ok(())
}

/// The `<slot>` of `this`, else `Illegal invocation`.
fn slot(d: &Dom, it: &mut Interp<'_>, this: Value) -> R<NodeId> {
    let n = d.el(it, this)?;
    if d.doc.borrow().is_html(n, "slot") { Ok(n) } else { Err(it.type_error("Illegal invocation")) }
}

fn name(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let n = slot(d, it, this)?;
    let s = {
        let doc = d.doc.borrow();
        doc.find_atom("name").and_then(|a| doc.attr(n, a)).unwrap_or("").to_string()
    };
    d.js_str(it, &s)
}

fn set_name(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let n = slot(d, it, this)?;
    let s = d.string_arg(it, arg(a, 0))?;
    nd::attr_set(d, it, n, "name", &s)?;
    Ok(Value::Undefined)
}

/// `assignedNodes` and `assignedElements` (`only_elements`).
fn assigned(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value], only_elements: bool) -> V {
    let n = slot(d, it, this)?;
    let flatten = match arg(a, 0) {
        Value::Undefined | Value::Null => false,
        o @ Value::Object(_) => {
            let k = d.key(it, "flatten")?;
            let v = it.get_value(o, k)?;
            it.to_js_bool(v)?
        }
        _ => return Err(it.type_error("Failed to execute 'assignedNodes' on 'HTMLSlotElement': The provided value is not of type 'AssignedNodesOptions'.")),
    };
    let out = {
        let doc = d.doc.borrow();
        let in_shadow = |s: NodeId| doc.shadow_host(doc.tree_root(s)).is_some();
        // What a slot gives when flattened: what it takes, else its fallback children.
        let own = |s: NodeId| {
            let nodes = doc.assigned_nodes(s);
            if flatten && nodes.is_empty() && in_shadow(s) {
                return doc.children(s).filter(|&c| matches!(doc.data(c), NodeData::Element { .. } | NodeData::Text(_))).collect();
            }
            nodes
        };
        let (mut out, mut work) = (Vec::new(), own(n));
        work.reverse();
        while let Some(c) = work.pop() {
            it.charge(1)?;
            if flatten && doc.is_html(c, "slot") && in_shadow(c) {
                work.extend(own(c).into_iter().rev());
            } else if !only_elements || matches!(doc.data(c), NodeData::Element { .. }) {
                out.push(c);
            }
        }
        out
    };
    nd::wrap_all(d, it, &out)
}

/// `Element.assignedSlot` and `Text.assignedSlot`: the slot that takes the node, `null` for a closed shadow root and for a node that no slot takes.
pub(crate) fn assigned_slot(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let n = d.nd(it, this)?;
    let s = {
        let doc = d.doc.borrow();
        doc.assigned_slot(n).filter(|&s| doc.shadow_is_open(doc.tree_root(s)))
    };
    d.wrap_opt(it, s)
}
