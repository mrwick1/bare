//! Shadow roots: `Element.attachShadow`, `Element.shadowRoot`, `ShadowRoot` (`host`, `mode`, and the fixed option flags) and the
//! `DocumentFragment` brand. This is the TREE foundation of the DOM Standard (shadow trees, shadow-including ancestors), not shadow rendering.
//!
//! Native model: a root is a node of the one document arena (`NodeData::ShadowRoot`). It is never a child: it has no parent link. The nodes of
//! its tree have it as their parent, so the light links of the host (`children`, `parentNode`, `querySelector`, `textContent`, `innerHTML`,
//! the page parser, the cascade, the loader's script walk) never see them. The host and the mode are in two sparse tables of the document
//! (`Document::shadow_root`, `Document::shadow_host`). The JS wrappers of a host and its root keep each other alive (`Dom::trace`), so a
//! detached host and root form a cycle that the collector frees together, and an expando on one is not lost while the other is held.
//!
//! `attachShadow(init)` order: brand check (`Illegal invocation`); one required argument; `init` (undefined and null are an empty dictionary,
//! any other primitive is a `TypeError`); the members in the WebIDL order `clonable`, `delegatesFocus`, `mode`, `serializable`,
//! `slotAssignment` (each getter runs once, `mode` and `slotAssignment` go through `ToString` and the enum check as soon as they are read);
//! only then is the host read. No document borrow is held while JS runs. Then, in the order of the DOM Standard "attach a shadow root":
//! not in the HTML namespace, a local name that is not a valid shadow host name, or a host that already has a root: `NotSupportedError`
//! (there is no declarative shadow DOM, so a second root is always an error, also for the same mode). Then the gates below, then the caps
//! (nodes, wrappers, depth), then the root is made. Nothing changes before the last check.
//!
//! Valid shadow host names (HTML Standard): `article aside blockquote body div footer h1-h6 header main nav p section span`, and a valid custom
//! element name (lowercase start, a hyphen, only `PCENChar`, not one of the eight reserved names). Names are exact: `DIV` made with
//! `createElementNS` is not a host. A definition can disable shadow roots (`static disabledFeatures`); this is not built.
//!
//! `clonable`, `serializable` and `delegatesFocus` are stored and read back by their getters, and do nothing else: `cloneNode` of a host does not
//! copy its root, there is no `getHTML`, and a shadow tree has no focus. Gate (`Error::Unsupported`, before anything changes): `slotAssignment: "manual"`;
//! its getter is fixed at `"named"`. Inserting a root as a node
//! (`appendChild(root)` and so on) is `Unsupported` after the other insertion checks (the standard moves the children of a fragment).
//!
//! Style and layout: a shadow tree has its own style scope (`RuleSet::add_scoped`, `bare_css::cascade`) and its slots are in the flat tree
//! (`Document::flat_children`, `crates/html/src/dom/slots.rs`). `getComputedStyle` and the geometry reads work in a connected shadow tree and on slotted nodes.
//! `HTMLSlotElement`, `assignedSlot` and `assignedNodes` are in `browser_slot.rs`. In a script, `:host`, `:host()` and `::slotted()` are good
//! selectors that match nothing.
//!
//! What a shadow tree is NOT here (none of this is faked): no `getHTML`, `activeElement` or
//! `styleSheets` on `ShadowRoot`, no declarative shadow DOM, no `slotchange` event, no focus in a shadow tree (its elements are not focusable areas) and no
//! script run from a shadow tree (the loader prepares only scripts whose light ancestors reach the document). `new ShadowRoot()` is an illegal constructor.
//! Deviation: Chrome inserts a shadow root like a fragment (its children move); here it is `Unsupported`.
//!
//! Constructable style sheets and `adoptedStyleSheets` (document and shadow root) are in `browser_cssom.rs`.
//!
//! Events: a path walks the parent links, so an event that is not composed and starts in a shadow tree ends at its root, which is exactly the
//! standard path. A COMPOSED event whose path would reach a root (the standard continues at the host and retargets `target`) is refused with
//! `Unsupported` before any listener runs. This includes the events of the test driver (`Browser::dispatch`, `click`, ...), which are composed.
//! Events that start in the light tree are unchanged.

use crate::browser::*;
use crate::browser_scoped::*;
use bare_js_api::{Args, Local, Type};
use bare_html::dom::{Document, NodeData, NodeId, Ns};

pub(crate) use bare_html::dom::valid_custom_name;

/// DOM Standard "attach a shadow root", steps 1 and 2: an HTML element with a valid shadow host name.
fn valid_host(doc: &Document, host: NodeId) -> bool {
    if doc.element_ns(host) != Ns::Html {
        return false;
    }
    let name = doc.element_name(host);
    matches!(name, "article" | "aside" | "blockquote" | "body" | "div" | "footer" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "header" | "main" | "nav" | "p" | "section" | "span") || valid_custom_name(name)
}

/// The native root of `this`, or `Illegal invocation`.
fn root_of<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>) -> Res<'s, NodeId> {
    match d.node_of_s(s, this) {
        Some(n) if matches!(d.data(n), NodeData::ShadowRoot) && d.doc.borrow().shadow_host(n).is_some() => Ok(n),
        _ => Err(s.type_error("Illegal invocation")),
    }
}

/// The member `name` of the dictionary `init`, `undefined` when `init` is not an object. Runs the getter.
fn member<'s>(_d: &Dom, s: &mut Sc<'s, '_>, init: Local<'s>, name: &str) -> Res<'s, Local<'s>> {
    if !matches!(s.type_of(init), Type::Object | Type::Function) {
        return Ok(s.undefined());
    }
    s.get_str(init, name)
}

/// A boolean member: the getter runs, then `ToBoolean` (no JS runs in it).
fn flag<'s>(d: &Dom, s: &mut Sc<'s, '_>, init: Local<'s>, name: &str) -> Res<'s, bool> {
    let v = member(d, s, init, name)?;
    Ok(s.to_boolean(v))
}

/// A member that is a WebIDL enumeration: `ToString`, then it must be one of `allowed`. `None` when it is `undefined`.
fn enum_member<'s>(d: &Dom, s: &mut Sc<'s, '_>, init: Local<'s>, name: &str, ty: &str, allowed: &[&'static str]) -> Res<'s, Option<&'static str>> {
    let v = member(d, s, init, name)?;
    if matches!(s.type_of(v), Type::Undefined) {
        return Ok(None);
    }
    let st = d.string_arg_s(s, v)?;
    match allowed.iter().find(|a| **a == st) {
        Some(a) => Ok(Some(a)),
        None => Err(s.type_error(&format!("Failed to execute 'attachShadow' on 'Element': The provided value '{st}' is not a valid enum value of type {ty}."))),
    }
}

pub(crate) fn attach_shadow<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let host = d.el_s(s, this)?;
    if a.is_empty() {
        return Err(s.type_error("Failed to execute 'attachShadow' on 'Element': 1 argument required, but only 0 present."));
    }
    let init = arg_s(s, a, 0);
    if !matches!(s.type_of(init), Type::Undefined | Type::Null | Type::Object | Type::Function) {
        return Err(s.type_error("Failed to execute 'attachShadow' on 'Element': The provided value is not of type 'ShadowRootInit'."));
    }
    // WebIDL dictionary conversion: the members in lexicographic order, each getter once. No borrow is held while a getter or a `toString` runs.
    let clonable = flag(d, s, init, "clonable")?;
    let delegates_focus = flag(d, s, init, "delegatesFocus")?;
    let Some(mode) = enum_member(d, s, init, "mode", "ShadowRootMode", &["open", "closed"])? else {
        return Err(s.type_error("Failed to execute 'attachShadow' on 'Element': Failed to read the 'mode' property from 'ShadowRootInit': Required member is undefined."));
    };
    let serializable = flag(d, s, init, "serializable")?;
    let manual = enum_member(d, s, init, "slotAssignment", "SlotAssignmentMode", &["named", "manual"])? == Some("manual");
    // From here no JS runs. Validity first (as the standard orders it), then what this host does not do, then the caps.
    {
        let doc = d.doc.borrow();
        if !valid_host(&doc, host) {
            drop(doc);
            return Err(d.dom_err_s(s, "NotSupportedError", "This element does not support attachShadow"));
        }
        if doc.shadow_root(host).is_some() {
            drop(doc);
            return Err(d.dom_err_s(s, "NotSupportedError", "Shadow root cannot be created on a host which already hosts a shadow tree."));
        }
    }
    if manual {
        return Err(unsupp("attachShadow with slotAssignment manual"));
    }
    d.room_s(s, &d.doc.borrow(), 1, 0)?;
    {
        let attrs = d.attr.s.borrow();
        if d.st.borrow().by_obj.len() + attrs.attrs.len() + attrs.maps.len() + d.nl.s.borrow().lists.len() >= d.lim.max_wrappers {
            return Err(d.lim_s(s, "dom-wrappers"));
        }
    }
    if d.depth_of_s(s, host)? + 1 > d.lim.max_tree_depth {
        return Err(d.lim_s(s, "dom-depth"));
    }
    // Reserve work and allocate the JS wrapper before changing native shadow ownership.
    s.charge(1)?;
    let wrapper = d.obj_s(s, d.p().shadow)?;
    let root = {
        let mut doc = d.doc.borrow_mut();
        let root = doc.attach_shadow(host, mode == "open");
        doc.set_shadow_flags(root, u8::from(delegates_focus) | u8::from(clonable) << 1 | u8::from(serializable) << 2);
        root
    };
    let it = s.engine();
    d.set_wrapper(&mut d.st.borrow_mut(), it, root, wrapper);
    Ok(d.obj_local(s, wrapper))
}

pub(crate) fn shadow_root<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let host = d.el_s(s, this)?;
    let root = {
        let doc = d.doc.borrow();
        doc.shadow_root(host).filter(|&r| doc.shadow_is_open(r))
    };
    d.wrap_opt_s(s, root)
}

pub(crate) fn host<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let root = root_of(d, s, this)?;
    let host = d.doc.borrow().shadow_host(root);
    d.wrap_opt_s(s, host)
}

pub(crate) fn mode<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let root = root_of(d, s, this)?;
    let open = d.doc.borrow().shadow_is_open(root);
    d.js_str_s(s, if open { "open" } else { "closed" })
}

/// `delegatesFocus` (bit 0), `clonable` (bit 1) and `serializable` (bit 2): the values that `attachShadow` was given. They do nothing else here.
pub(crate) fn flag_bit<'s, const BIT: u8>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let root = root_of(d, s, this)?;
    Ok(s.boolean(d.doc.borrow().shadow_flags(root) & BIT != 0))
}

pub(crate) fn slot_assignment<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    root_of(d, s, this)?;
    d.js_str_s(s, "named")
}
