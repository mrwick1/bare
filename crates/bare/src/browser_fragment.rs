//! `Element.prototype.innerHTML`: the HTML fragment parsing and serialization algorithms on the native document.
//!
//! Setter (`el.innerHTML = value`), in this order:
//! 1. Brand check (`Illegal invocation` for anything that is not an Element wrapper; an `Attr`, a `Text` and the `Document` are not).
//! 2. `null` is the empty string (`[LegacyNullToEmptyString]`); every other value goes through `ToString` (it may run page code and may change
//!    the target). A lone surrogate is `Unsupported` (the native document is UTF-8).
//! 3. Only now is the target read. Its name and namespace are the parser's context element, and whether it is in a `form`.
//!    `bare_html::tree::parse_fragment` parses into a temporary `Document` with the same tokenizer and tree builder as a page. The context
//!    sets the tokenizer state (raw text, RCDATA, script data, PLAINTEXT). Its gates and caps are in `bare_html/src/tree/fragment.rs`.
//! 4. The result is measured with an iterative walk (nodes, text bytes, attribute slots, new names, depth below the target) and checked against the
//!    caps of this browser. All the work is charged to the step budget before it is done (the input before the parse, each node as it is made).
//! 5. Only then do the old children leave (detached, never freed: their wrappers, listeners, `Attr` objects and held references stay valid) and
//!    the new nodes are copied in with `Document::import_node` (namespaces, prefixes, attribute namespaces kept). No JS runs, nothing can fail.
//!    Every HTML `script` of the fragment gets the "already started" flag (`Dom::mark_started`): it never runs and starts no fetch, whatever
//!    happens to it later. No event attribute is wired (this host has no inline handlers), and no second loader exists.
//!
//! Getter: the fragment serialization algorithm for the children of the element, iterative (no recursion, any depth), capped by `max_string_bytes`.
//! Text is escaped (`&`, no-break space, `<`, `>`) except in `style`, `script`, `xmp`, `iframe`, `noembed`, `noframes` and `plaintext`. An attribute
//! value is escaped (`&`, no-break space, `"`, `<`, `>`; the last two follow the current standard and are to verify against a browser). The void
//! elements have no end tag and no children. An attribute in a namespace is written `xml:`, `xmlns:` or `xlink:` plus the local name.
//!
//! Gates (`Error::Unsupported`, before anything changes; nothing is faked with another context):
//! - Contexts: `table thead tbody tfoot tr td th caption colgroup col select template html frameset frame noscript`, and any element that is not in
//!   the HTML namespace (an SVG or MathML element).
//! - Markup: a `template`, `noscript`, `frameset` or `frame` start tag in the value.
//! - The getter, for a tree that holds an HTML `template` or `noscript` element (their serialization depends on template contents and the scripting flag).
//! - `outerHTML`, `insertAdjacentHTML`, `DocumentFragment`, `Range.createContextualFragment`, `DOMParser` and `document.write` are absent.
//!
//! Known deviations: nested `table` and `select` are read by the page rules of the tree builder (no table or select insertion modes). A temporary
//! text buffer that grew by joining texts counts the wasted bytes against the text cap. `cloneNode` does not copy the "already started" flag.

use crate::browser::*;
use crate::browser_dom::{arg, next_in};
use bare_html::dom::{Document, NodeData, NodeId, Ns};
use bare_html::tree::{FragmentError, FragmentLimits, parse_fragment};
use bare_js::eval::{Error, Interp};
use bare_js::value::Value;

type V = R<Value>;

/// Elements whose text is written as it is.
const RAW_PARENTS: &[&str] = &["style", "script", "xmp", "iframe", "noembed", "noframes", "plaintext"];

/// Elements that have no end tag and no children when written.
const VOID: &[&str] = &["area", "base", "basefont", "bgsound", "br", "col", "embed", "frame", "hr", "img", "input", "keygen", "link", "meta", "param", "source", "track", "wbr"];

/// Append `piece` to `out`, if the result stays under `cap`.
fn put(d: &Dom, out: &mut String, piece: &str) -> R<()> {
    if piece.len() > d.lim.max_string_bytes.saturating_sub(out.len()) {
        return Err(d.lim("dom-string"));
    }
    out.push_str(piece);
    Ok(())
}

/// Append `text` with `&`, no-break space, `<` and `>` escaped, and `"` too for an attribute value.
fn escape(d: &Dom, out: &mut String, text: &str, attr: bool) -> R<()> {
    let special = |c: char| matches!(c, '&' | '\u{a0}' | '<' | '>') || (attr && c == '"');
    let mut rest = text;
    while let Some(i) = rest.find(special) {
        put(d, out, &rest[..i])?;
        let c = rest[i..].chars().next().expect("a char");
        put(
            d,
            out,
            match c {
                '&' => "&amp;",
                '\u{a0}' => "&nbsp;",
                '<' => "&lt;",
                '>' => "&gt;",
                _ => "&quot;",
            },
        )?;
        rest = &rest[i + c.len_utf8()..];
    }
    put(d, out, rest)
}

/// The name of an attribute as the serialization algorithm writes it.
fn attr_name(doc: &Document, name: bare_html::dom::Atom) -> String {
    let local = doc.attr_local(name);
    match doc.attr_ns(name) {
        None => local.to_string(),
        Some(Ns::Xml) => format!("xml:{local}"),
        Some(Ns::Xmlns) if local == "xmlns" => "xmlns".to_string(),
        Some(Ns::Xmlns) => format!("xmlns:{local}"),
        Some(Ns::Xlink) => format!("xlink:{local}"),
        Some(_) => local.to_string(),
    }
}

/// The children of `n` as HTML text. Walks with the parent links, so the depth is not a limit.
fn serialize(d: &Dom, it: &mut Interp<'_>, doc: &Document, n: NodeId) -> R<String> {
    let mut out = String::new();
    let Some(mut node) = doc.children(n).next() else { return Ok(out) };
    loop {
        it.charge(1)?;
        let mut open = false;
        match *doc.data(node) {
            NodeData::Text(sp) => {
                let raw = doc.parent(node).is_some_and(|p| !doc.is_foreign(p) && RAW_PARENTS.contains(&doc.element_name(p)));
                if raw { put(d, &mut out, doc.text(sp))? } else { escape(d, &mut out, doc.text(sp), false)? }
            }
            NodeData::Comment(sp) => {
                put(d, &mut out, "<!--")?;
                put(d, &mut out, doc.text(sp))?;
                put(d, &mut out, "-->")?;
            }
            NodeData::Doctype(_) | NodeData::Document => {}
            NodeData::Element { name, attrs } => {
                let tag = doc.atom_str(name);
                let html = !doc.is_foreign(node);
                if html && matches!(tag, "template" | "noscript") {
                    return Err(Error::Unsupported(unsupported("innerHTML of a tree with a template or noscript element (template contents and the scripting flag)")));
                }
                put(d, &mut out, "<")?;
                put(d, &mut out, tag)?;
                for a in doc.attrs(attrs) {
                    it.charge(1)?;
                    put(d, &mut out, " ")?;
                    put(d, &mut out, &attr_name(doc, a.name))?;
                    put(d, &mut out, "=\"")?;
                    escape(d, &mut out, doc.text(a.value), true)?;
                    put(d, &mut out, "\"")?;
                }
                put(d, &mut out, ">")?;
                if !(html && VOID.contains(&tag)) {
                    if let Some(first) = doc.children(node).next() {
                        node = first;
                        open = true;
                    } else {
                        put(d, &mut out, "</")?;
                        put(d, &mut out, tag)?;
                        put(d, &mut out, ">")?;
                    }
                }
            }
        }
        if open {
            continue;
        }
        // Next sibling, or close the parents on the way up.
        loop {
            if let Some(s) = doc.next_sibling(node) {
                node = s;
                break;
            }
            let Some(p) = doc.parent(node) else { return Ok(out) };
            if p == n {
                return Ok(out);
            }
            put(d, &mut out, "</")?;
            put(d, &mut out, doc.element_name(p))?;
            put(d, &mut out, ">")?;
            node = p;
        }
    }
}

pub(crate) fn inner_html(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let n = d.el(it, this)?;
    let s = serialize(d, it, &d.doc.borrow(), n)?;
    d.js_str(it, &s)
}

pub(crate) fn set_inner_html(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let n = d.el(it, this)?;
    let v = arg(a, 0);
    // ToString may run page code; the target is read after it.
    let html = if matches!(v, Value::Null) { String::new() } else { d.string_arg(it, v)? };
    let (context, ns, in_form) = {
        let doc = d.doc.borrow();
        let mut in_form = false;
        let mut cur = Some(n);
        while let Some(c) = cur {
            it.charge(1)?;
            if doc.is_html(c, "form") {
                in_form = true;
                break;
            }
            cur = doc.parent(c);
        }
        (doc.element_name(n).to_string(), doc.element_ns(n), in_form)
    };
    let base = d.depth_of(it, n)?;
    let lim = {
        let doc = d.doc.borrow();
        FragmentLimits {
            max_source: d.lim.max_string_bytes,
            max_nodes: d.lim.max_nodes.saturating_sub(doc.len()),
            max_text_bytes: d.lim.max_text_bytes.saturating_sub(doc.text_bytes()),
            max_attr_slots: d.lim.max_attr_slots.saturating_sub(doc.attr_slots()),
            max_names: d.lim.max_names,
            max_depth: d.lim.max_tree_depth.saturating_sub(base),
        }
    };
    // The parse is native code that cannot be stopped from outside: the input is charged first, and each token's nodes as they are made.
    it.charge(html.len() as u64 / 16 + 1)?;
    let mut stop = None;
    let parsed = parse_fragment(&html, &context, ns, in_form, &lim, &mut |units| match it.charge(units) {
        Ok(()) => true,
        Err(e) => {
            stop = Some(e);
            false
        }
    });
    let frag = match parsed {
        Ok(f) => f,
        Err(FragmentError::Unsupported(what)) => return Err(Error::Unsupported(unsupported(what))),
        Err(FragmentError::Limit(name)) => return Err(d.lim(name)),
        Err(FragmentError::Stopped) => return Err(stop.expect("the budget error")),
    };

    // Measure the result and check it against this browser's caps, before anything changes.
    let (mut count, mut bytes, mut attrs, mut height) = (0usize, 0usize, 0usize, 0usize);
    {
        let (mut cur, mut depth) = (frag.root, 0usize);
        while let Some((c, cd)) = next_in(&frag.doc, frag.root, cur, depth) {
            it.charge(1)?;
            (cur, depth) = (c, cd);
            count += 1;
            height = height.max(cd);
            match *frag.doc.data(c) {
                NodeData::Text(sp) | NodeData::Comment(sp) => bytes += frag.doc.text(sp).len(),
                NodeData::Element { attrs: list, .. } => {
                    let list = frag.doc.attrs(list);
                    attrs += list.len();
                    bytes += list.iter().map(|x| frag.doc.text(x.value).len()).sum::<usize>();
                }
                _ => {}
            }
        }
    }
    let old = {
        let doc = d.doc.borrow();
        let mut old = 0u64;
        for _ in doc.children(n) {
            it.charge(1)?;
            old += 1;
        }
        d.room(&doc, count, bytes)?;
        if attrs > d.lim.max_attr_slots.saturating_sub(doc.attr_slots()) {
            return Err(d.lim("dom-attrs"));
        }
        if doc.name_count() + frag.doc.new_names_in(&doc) > d.lim.max_names {
            return Err(d.lim("dom-names"));
        }
        old
    };
    if count > 0 && base + height > d.lim.max_tree_depth {
        return Err(d.lim("dom-depth"));
    }
    it.charge(old + count as u64)?;

    // Commit. Nothing below runs page code or can fail.
    let mut doc = d.doc.borrow_mut();
    while let Some(c) = doc.children(n).next() {
        doc.detach(c);
    }
    let mut path = vec![n];
    let (mut cur, mut depth) = (frag.root, 0usize);
    while let Some((c, cd)) = next_in(&frag.doc, frag.root, cur, depth) {
        (cur, depth) = (c, cd);
        let t = doc.import_node(&frag.doc, c).expect("a node of the fragment");
        doc.move_to(t, path[cd - 1], None);
        path.truncate(cd);
        path.push(t);
        if doc.is_html(t, "script") {
            d.mark_started(t);
        }
    }
    Ok(Value::Undefined)
}
