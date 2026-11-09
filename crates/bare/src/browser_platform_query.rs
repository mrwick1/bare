//! `getElementsByTagName` and `getElementsByClassName` on `Document` and `Element`.
//!
//! These return a plain snapshot array of the matching descendants in tree order (not the element itself). They are
//! NOT live `HTMLCollection`s: no `item`, no `namedItem`, and the array does not change when the tree does. The page
//! code the staging login needs only reads `length` and indexes the result at once (`webpack-runtime.js`).
//!
//! - The walk is iterative over the native tree, charges one step for each node (class lookups also charge for the
//!   length of the `class` attribute and the number of names) and checks `max_results` before each result is added
//!   (`Limit(Host("dom-results"))`). No document borrow is held while wrappers are made.
//! - `getElementsByTagName(name)`: `*` matches every element. Any other name is lowercased (ASCII) and compared with the
//!   element name for an HTML element. An SVG or MathML element is compared with its qualified name as given (case-sensitive), as the DOM
//!   Standard says for an HTML document. A name that is not in the document name table cannot match an HTML element, and a page with no
//!   foreign element costs nothing for it.
//! - `getElementsByClassName(names)`: `names` is split on ASCII white space; an element matches when its `class`
//!   attribute has every name (case-sensitive: the document is always in standards mode here, no quirks mode). No names:
//!   empty result. More than `max_class_tokens` distinct names is a Limit.

use crate::browser::*;
use crate::browser_dom::{next_in, wrap_all};
use crate::browser_platform::{V, arg};
use bare_html::dom::NodeData;
use bare_js::eval::Interp;
use bare_js::value::Value;
use std::collections::HashSet;

pub(crate) fn install(d: &Dom, it: &mut Interp<'_>) -> R<()> {
    let p = d.p();
    for target in [p.element, p.document] {
        d.def_method(it, target, "getElementsByTagName", 1, by_tag)?;
        d.def_method(it, target, "getElementsByClassName", 1, by_class)?;
    }
    Ok(())
}

/// The elements under `root` for which `keep` says yes, in tree order. `cost` is charged for each node.
fn collect(d: &Dom, it: &mut Interp<'_>, root: bare_html::dom::NodeId, mut keep: impl FnMut(&bare_html::dom::Document, bare_html::dom::NodeId) -> (bool, u64)) -> R<Vec<bare_html::dom::NodeId>> {
    let doc = d.doc.borrow();
    let mut out = Vec::new();
    let (mut cur, mut depth) = (root, 0usize);
    while let Some((c, cd)) = next_in(&doc, root, cur, depth) {
        (cur, depth) = (c, cd);
        if !matches!(doc.data(c), NodeData::Element { .. }) {
            it.charge(1)?;
            continue;
        }
        let (yes, cost) = keep(&doc, c);
        it.charge(1 + cost)?;
        if yes {
            if out.len() >= d.lim.max_results {
                return Err(d.lim("dom-results"));
            }
            out.push(c);
        }
    }
    Ok(out)
}

fn by_tag(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let root = d.nd(it, this)?;
    if a.is_empty() {
        return Err(it.type_error("Failed to execute 'getElementsByTagName': 1 argument required, but only 0 present."));
    }
    let name = d.string_arg(it, arg(a, 0))?;
    let ids = if name == "*" {
        collect(d, it, root, |_, _| (true, 0))?
    } else {
        // An HTML element is compared with the lowercase name. An SVG or MathML element is compared, with its prefix, with the name as given.
        let (atom, foreign) = {
            let doc = d.doc.borrow();
            (doc.find_atom(&name.to_ascii_lowercase()), doc.has_foreign())
        };
        if atom.is_none() && !foreign {
            Vec::new()
        } else {
            collect(d, it, root, |doc, c| {
                let NodeData::Element { name: local, .. } = *doc.data(c) else { return (false, 0) };
                if !doc.is_foreign(c) {
                    return (Some(local) == atom, 0);
                }
                let own = doc.atom_str(local);
                (
                    match doc.element_prefix(c) {
                        None => name == own,
                        Some(p) => name.strip_prefix(p).and_then(|r| r.strip_prefix(':')) == Some(own),
                    },
                    0,
                )
            })?
        }
    };
    wrap_all(d, it, &ids)
}

/// `document.scripts`: the HTML `script` elements of the document in tree order, as a snapshot array like `getElementsByTagName`.
pub(crate) fn scripts(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    html_of_tag(d, it, this, "script")
}

/// `document.forms`: the same for `form` elements.
pub(crate) fn forms(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    html_of_tag(d, it, this, "form")
}

fn html_of_tag(d: &Dom, it: &mut Interp<'_>, this: Value, tag: &str) -> V {
    let root = d.nd(it, this)?;
    if !matches!(d.data(root), NodeData::Document) {
        return Err(it.type_error("Illegal invocation"));
    }
    let atom = d.doc.borrow().find_atom(tag);
    let ids = if atom.is_none() { Vec::new() } else { collect(d, it, root, |doc, c| (matches!(doc.data(c), NodeData::Element { name, .. } if Some(*name) == atom) && !doc.is_foreign(c), 0))? };
    wrap_all(d, it, &ids)
}

fn by_class(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let root = d.nd(it, this)?;
    if a.is_empty() {
        return Err(it.type_error("Failed to execute 'getElementsByClassName': 1 argument required, but only 0 present."));
    }
    let text = d.string_arg(it, arg(a, 0))?;
    let mut seen = HashSet::new();
    let want: Vec<&str> = text.split_ascii_whitespace().filter(|t| seen.insert(*t)).collect();
    if want.len() > d.pl_limits().max_class_tokens {
        return Err(d.lim("class-tokens"));
    }
    let ids = if want.is_empty() {
        Vec::new()
    } else {
        collect(d, it, root, |doc, c| {
            let len = doc.attr(c, bare_html::dom::Atom::CLASS).map_or(0, str::len);
            (len > 0 && want.iter().all(|w| doc.has_class(c, w)), (want.len() + len / 16) as u64)
        })?
    };
    wrap_all(d, it, &ids)
}
