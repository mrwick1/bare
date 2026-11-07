//! Namespaces in the DOM host: `namespaceURI`, `prefix`, `createElementNS` and the `*AttributeNS` methods.
//!
//! The identity lives in the native `Document` (`Document::element_ns`, `intern_ns`), not in the JS wrappers. It stays with a node
//! when it moves or is cloned. The wrapper brand (`SVGElement`, `HTMLElement`) and `tagName` read the native identity every time.
//!
//! Supported:
//! - Elements in the HTML, SVG and MathML namespaces. An SVG or MathML name keeps its case (`foreignObject`), may have a prefix
//!   (`svg:rect`), and `tagName`/`nodeName` is the qualified name (upper case only for HTML). `createElement` is always HTML.
//!   The HTML namespace through `createElementNS` is only for lowercase names with no prefix, the names the document stores.
//! - Attributes in no namespace, XML, XMLNS and XLink (the ones that React and SVG pages use: `xlink:href`, `xml:lang`, `xmlns:xlink`).
//!   An attribute in a namespace is a different attribute from one with the same qualified name and no namespace
//!   (`setAttribute('xlink:href')` has no namespace). `setAttribute`, `getAttribute`, `hasAttribute` and `removeAttribute` find the
//!   first attribute with that qualified name, in any namespace, as the DOM Standard says. They lowercase the name only for an HTML element.
//! - The checks of the DOM Standard "validate and extract", before any write: a name that is not a QName is `InvalidCharacterError`;
//!   a prefix with no namespace, `xml` outside the XML namespace, `xmlns` outside the XMLNS namespace, and the XMLNS namespace with
//!   another name are `NamespaceError`. Then a namespace that this host does not store is `Unsupported`, with its name in the message.
//!
//! Not supported (explicit, never faked):
//! - Any other namespace, and a null namespace for an element. MathML has no `MathMLElement`: its wrappers are plain `Element`s.
//! - (Moved) `Attr` nodes, `Element.attributes`, `getAttributeNode(NS)`, `setAttributeNode(NS)`, `removeAttributeNode` and `createAttribute(NS)` live in `browser_attr.rs` (read its header). The namespace lookup methods (`lookupNamespaceURI`) are not supported.
//! - SVG drawing, layout and style. An SVG element has no `style`, `className` as `SVGAnimatedString`, `getBBox` or the other SVG DOM.
//!   Style comes from the separate CSSStyleDeclaration work: this file gives it the native identity (`Document::element_ns`) and
//!   nothing else. CSS selectors match the local name in any namespace; `svg|a` namespace selectors do not exist.
//! - The parser makes SVG and MathML (see `bare_html::tree`, file `foreign.rs`, for its gaps). `<script>`, `<link>` and `<base>` of
//!   SVG are not HTML resources: the loader does not run or fetch them.
//!
//! Costs: a foreign element has one entry in a hash map of the document, and an attribute with a namespace has a name of its own in
//! the name table. Both stay in the document arena like every native node and name (it never frees). The caps are `max_names`
//! (the local name and the prefix are two names), `max_nodes`, `max_attr_slots` and `max_text_bytes`, checked before any write.

use crate::browser::*;
use crate::browser_dom::{arg, valid_name};
use bare_html::dom::{Atom, NodeId, Ns};
use bare_js::eval::{Error, Interp};
use bare_js::value::Value;

type V = R<Value>;

/// The qualified name of an element: `prefix:local`, or the local name.
pub(crate) fn qualified(doc: &bare_html::dom::Document, el: NodeId) -> String {
    let local = doc.element_name(el);
    match doc.element_prefix(el) {
        Some(p) => format!("{p}:{local}"),
        None => local.to_string(),
    }
}

/// `tagName` and `nodeName`: the qualified name, in upper case for an HTML element.
pub(crate) fn tag(doc: &bare_html::dom::Document, el: NodeId) -> String {
    let q = qualified(doc, el);
    if doc.element_ns(el) == Ns::Html { q.to_ascii_uppercase() } else { q }
}

/// The name that `setAttribute` and friends look for: lowercase for an HTML element, as typed for the others.
pub(crate) fn attr_key(doc: &bare_html::dom::Document, el: NodeId, name: &str) -> String {
    if doc.element_ns(el) == Ns::Html { name.to_ascii_lowercase() } else { name.to_string() }
}

/// Split a QName into its prefix and local name. `None` if it is not a QName: a name with one colon needs a name on both sides.
fn qname_parts(q: &str) -> Option<(Option<&str>, &str)> {
    if !valid_name(q) {
        return None;
    }
    match q.split_once(':') {
        None => Some((None, q)),
        Some((p, l)) if valid_name(p) && valid_name(l) && !l.contains(':') => Some((Some(p), l)),
        Some(_) => None,
    }
}

/// "Validate and extract" of the DOM Standard. Gives the prefix and the local name. `ns` is the namespace string as the page gave it
/// (`None` for null; an empty string is null too).
pub(crate) fn extract<'a>(d: &Dom, it: &mut Interp<'_>, ns: Option<&str>, qname: &'a str) -> R<(Option<&'a str>, &'a str)> {
    let Some((prefix, local)) = qname_parts(qname) else {
        return Err(d.dom_err(it, "InvalidCharacterError", &format!("The qualified name provided ('{qname}') is not a valid name.")));
    };
    let bad = (prefix.is_some() && ns.is_none())
        || (prefix == Some("xml") && ns != Some(Ns::Xml.uri()))
        || ((qname == "xmlns" || prefix == Some("xmlns")) && ns != Some(Ns::Xmlns.uri()))
        || (ns == Some(Ns::Xmlns.uri()) && qname != "xmlns" && prefix != Some("xmlns"));
    if bad {
        return Err(d.dom_err(it, "NamespaceError", &format!("The namespace URI provided ('{}') is not valid for the qualified name provided ('{qname}').", ns.unwrap_or(""))));
    }
    Ok((prefix, local))
}

/// A namespace argument: `null`, `undefined` and the empty string are no namespace.
pub(crate) fn ns_arg(d: &Dom, it: &mut Interp<'_>, v: Value) -> R<Option<String>> {
    Ok(match v {
        Value::Null | Value::Undefined => None,
        v => Some(d.string_arg(it, v)?).filter(|s| !s.is_empty()),
    })
}

const ATTR_NS: &str = "setAttributeNS with a namespace other than none, XML, XMLNS or XLink";

/// The namespace of an attribute that the host stores. `Err(())` for one that it does not.
pub(crate) fn attr_ns_of(ns: Option<&str>) -> Result<Option<Ns>, ()> {
    match ns {
        None => Ok(None),
        Some(u) => match Ns::from_uri(u) {
            Some(n @ (Ns::Xml | Ns::Xmlns | Ns::Xlink)) => Ok(Some(n)),
            _ => Err(()),
        },
    }
}

pub(crate) fn namespace_uri(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let n = d.el(it, this)?;
    let uri = d.doc.borrow().element_ns(n).uri();
    d.js_str(it, uri)
}

pub(crate) fn prefix(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let n = d.el(it, this)?;
    let p = d.doc.borrow().element_prefix(n).map(str::to_string);
    match p {
        Some(p) => d.js_str(it, &p),
        None => Ok(Value::Null),
    }
}

pub(crate) fn create_element_ns(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    d.nd(it, this)?;
    let ns = ns_arg(d, it, arg(a, 0))?;
    let qname = d.string_arg(it, arg(a, 1))?;
    let (prefix, local) = extract(d, it, ns.as_deref(), &qname)?;
    match ns.as_deref().map(Ns::from_uri) {
        None => Err(Error::Unsupported(unsupported("createElementNS with a null namespace"))),
        Some(Some(Ns::Html)) => {
            if prefix.is_some() || qname.bytes().any(|b| b.is_ascii_uppercase()) {
                return Err(Error::Unsupported(unsupported("createElementNS in the HTML namespace with a prefix or capital letters (names are lowercase in the native document)")));
            }
            if d.ce.on.get() || a.len() > 2 { crate::browser_custom::create(d, it, &qname, arg(a, 2)) } else { crate::browser_dom::make_element(d, it, &qname) }
        }
        Some(Some(ns @ (Ns::Svg | Ns::MathMl))) => {
            let node = {
                let mut doc = d.doc.borrow_mut();
                d.room(&doc, 1, 0)?;
                let mut new = [Some(local), prefix].into_iter().flatten().filter(|n| doc.find_atom(n).is_none()).count();
                if prefix == Some(local) && new == 2 {
                    new = 1;
                }
                if doc.name_count() + new > d.lim.max_names {
                    return Err(d.lim("dom-names"));
                }
                doc.create_element_ns(ns, prefix, local)
            };
            d.wrap(it, node)
        }
        Some(_) => Err(Error::Unsupported(unsupported("createElementNS with a namespace other than HTML, SVG or MathML"))),
    }
}

/// Write an attribute: by its atom if it exists, else by a new name in `ns`. Every cap is checked before the write.
pub(crate) fn put(d: &Dom, it: &mut Interp<'_>, n: NodeId, existing: Option<Atom>, ns: Option<Ns>, qname: &str, value: &str) -> R<()> {
    let mut doc = d.doc.borrow_mut();
    d.room(&doc, 0, value.len())?;
    if doc.attr_slots() + doc.attr_count(n) + 1 > d.lim.max_attr_slots {
        return Err(d.lim("dom-attrs"));
    }
    let atom = match existing {
        Some(a) => a,
        None => {
            if doc.find_ns_atom(ns, qname).is_none() && doc.name_count() + 1 > d.lim.max_names {
                return Err(d.lim("dom-names"));
            }
            doc.intern_ns(ns, qname)
        }
    };
    it.charge(doc.attr_count(n) as u64 + 1)?;
    doc.set_attr_atom(n, atom, value);
    Ok(())
}

pub(crate) fn set_attribute_ns(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let n = d.el(it, this)?;
    let ns = ns_arg(d, it, arg(a, 0))?;
    let qname = d.string_arg(it, arg(a, 1))?;
    let value = d.string_arg(it, arg(a, 2))?;
    let (_, local) = extract(d, it, ns.as_deref(), &qname)?;
    let Ok(kind) = attr_ns_of(ns.as_deref()) else { return Err(Error::Unsupported(unsupported(ATTR_NS))) };
    let existing = d.doc.borrow().find_attr_ns(n, kind, local);
    put(d, it, n, existing, kind, &qname, &value)?;
    if kind.is_none() && local == "src" {
        d.script_src_changed(it, n)?;
    }
    Ok(Value::Undefined)
}

/// The atom of the attribute for the `*AttributeNS` readers. `None` also for a namespace that no attribute can have.
fn find(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> R<(NodeId, Option<Atom>)> {
    let n = d.el(it, this)?;
    let ns = ns_arg(d, it, arg(a, 0))?;
    let local = d.string_arg(it, arg(a, 1))?;
    let doc = d.doc.borrow();
    it.charge(doc.attr_count(n) as u64 + 1)?;
    Ok(match attr_ns_of(ns.as_deref()) {
        Ok(kind) => (n, doc.find_attr_ns(n, kind, &local)),
        Err(()) => (n, None),
    })
}

pub(crate) fn get_attribute_ns(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let (n, atom) = find(d, it, this, a)?;
    let s = atom.and_then(|at| d.doc.borrow().attr(n, at).map(str::to_string));
    match s {
        Some(s) => d.js_str(it, &s),
        None => Ok(Value::Null),
    }
}

pub(crate) fn has_attribute_ns(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    Ok(Value::Bool(find(d, it, this, a)?.1.is_some()))
}

pub(crate) fn remove_attribute_ns(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let (n, atom) = find(d, it, this, a)?;
    if let Some(at) = atom {
        d.doc.borrow_mut().remove_attr_atom(n, at);
    }
    Ok(Value::Undefined)
}
