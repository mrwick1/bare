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
//! `outerHTML`: the getter writes the element itself and its subtree with the same serializer. The setter (DOM Parsing standard) converts the value
//! (`null` is empty), then reads the parent: no parent does nothing, a Document parent throws `NoModificationAllowedError`, a ShadowRoot (the
//! only DocumentFragment here) gives the context `body`, and any other parent is the context element. It goes through the same parse, measure and
//! commit as `innerHTML`; the new nodes take the place of the element, which is detached (never freed). As in Chrome (the standard is silent), a
//! text node before or after the result joins its neighbour.
//!
//! Getter: the fragment serialization algorithm for the children of the element, iterative (no recursion, any depth), capped by `max_string_bytes`.
//! Text is escaped (`&`, no-break space, `<`, `>`) except in `style`, `script`, `xmp`, `iframe`, `noembed`, `noframes`, `plaintext` and `noscript`
//! (scripting is on, so its text is raw; in the contents of a `template`, an inert document, it is escaped). An attribute
//! value is escaped (`&`, no-break space, `"`, `<`, `>`; the last two follow the current standard and are to verify against a browser). The void
//! elements have no end tag and no children. An attribute in a namespace is written `xml:`, `xmlns:` or `xlink:` plus the local name.
//!
//! Gates (`Error::Unsupported`, before anything changes; nothing is faked with another context):
//! - Contexts: `table thead tbody tfoot tr td th caption colgroup col select html frameset frame`, and any element that is not in
//!   the HTML namespace (an SVG or MathML element).
//! - Markup: a `frameset` or `frame` start tag in the value. A `template` start tag is read as the page parser reads it: its children go into its contents.
//!
//! `template`: the context `template.innerHTML = ...` parses like `div` and puts the nodes in the contents of the template (made when needed), not in
//! its children; `insertAdjacentHTML` on a template puts them in its children. The builder has no "in template" mode, so `tr` and `td` are dropped (a page is the same).
//! `noscript`: the scripting flag is on for a context in the page (raw text for `noscript`, as in Chrome), and off for a `template` context and
//! for any context in the contents of a template (an inert document): there `noscript` is an element. An HTML `template` is written with its
//! contents as children, and `template.innerHTML` reads the contents.
//! - `document.write` is absent.
//!
//! Known deviations: nested `table` and `select` are read by the page rules of the tree builder (no table or select insertion modes). A temporary
//! text buffer that grew by joining texts counts the wasted bytes against the text cap. Cloning preserves the "already started" flag.

use crate::browser::*;
use crate::browser_dom::{arg, next_in};
use bare_html::dom::{Document, NodeData, NodeId, Ns};
use bare_html::tree::{FragmentError, FragmentLimits, parse_fragment};
use bare_js::eval::{Error, Interp};
use bare_js::value::Value;

type V = R<Value>;

/// Elements whose text is written as it is.
const RAW_PARENTS: &[&str] = &["style", "script", "xmp", "iframe", "noembed", "noframes", "plaintext", "noscript"];

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

/// The children of `n` as HTML text, or `n` itself when `outer`. Walks with the parent links, so the depth is not a limit.
pub(crate) fn serialize(d: &Dom, it: &mut Interp<'_>, doc: &Document, n: NodeId, outer: bool) -> R<String> {
    let mut out = String::new();
    // The children of a `template` are its contents. `tpls` holds the templates that are open, to close them on the way up.
    let mut tpls = Vec::new();
    // A `noscript` has raw text when scripting is on, and the contents of a template are in an inert document: there it is escaped.
    let inert = |tpls: &Vec<NodeId>| !tpls.is_empty() || (!outer && doc.is_html(n, "template")) || doc.in_template_content(n);
    let n_kids = if !outer && doc.is_html(n, "template") { doc.template_content(n) } else { Some(n) };
    let Some(mut node) = (if outer { Some(n) } else { n_kids.and_then(|k| doc.children(k).next()) }) else { return Ok(out) };
    loop {
        it.charge(1)?;
        let mut open = false;
        match *doc.data(node) {
            NodeData::Text(sp) => {
                it.charge(doc.text(sp).len() as u64 / 16 + 1)?;
                let raw = doc.parent(node).is_some_and(|p| !doc.is_foreign(p) && RAW_PARENTS.contains(&doc.element_name(p)) && (doc.element_name(p) != "noscript" || !inert(&tpls)));
                if raw { put(d, &mut out, doc.text(sp))? } else { escape(d, &mut out, doc.text(sp), false)? }
            }
            NodeData::Comment(sp) => {
                it.charge(doc.text(sp).len() as u64 / 16 + 1)?;
                put(d, &mut out, "<!--")?;
                put(d, &mut out, doc.text(sp))?;
                put(d, &mut out, "-->")?;
            }
            NodeData::Doctype(_) | NodeData::Document | NodeData::ShadowRoot => {}
            NodeData::Element { name, attrs } => {
                let tag = doc.atom_str(name);
                let html = !doc.is_foreign(node);
                put(d, &mut out, "<")?;
                put(d, &mut out, tag)?;
                for a in doc.attrs(attrs) {
                    it.charge(1 + doc.text(a.value).len() as u64 / 16)?;
                    put(d, &mut out, " ")?;
                    put(d, &mut out, &attr_name(doc, a.name))?;
                    put(d, &mut out, "=\"")?;
                    escape(d, &mut out, doc.text(a.value), true)?;
                    put(d, &mut out, "\"")?;
                }
                put(d, &mut out, ">")?;
                if !(html && VOID.contains(&tag)) {
                    let tpl = html && tag == "template";
                    let holder = if tpl { doc.template_content(node) } else { Some(node) };
                    if let Some(first) = holder.and_then(|h| doc.children(h).next()) {
                        if tpl {
                            tpls.push(node);
                        }
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
            if outer && node == n {
                return Ok(out);
            }
            if let Some(s) = doc.next_sibling(node) {
                node = s;
                break;
            }
            let Some(mut p) = doc.parent(node) else { return Ok(out) };
            if Some(p) == n_kids && !outer {
                return Ok(out);
            }
            // The parent is the contents of an open template: close the template.
            if matches!(doc.data(p), NodeData::ShadowRoot) {
                p = tpls.pop().expect("an open template");
            }
            put(d, &mut out, "</")?;
            put(d, &mut out, doc.element_name(p))?;
            put(d, &mut out, ">")?;
            node = p;
        }
    }
}

/// The target of `innerHTML`: an element, or a shadow root (the context of its parse is the host).
fn inner_target(d: &Dom, it: &mut Interp<'_>, this: Value) -> R<(NodeId, Option<NodeId>)> {
    let n = d.nd(it, this)?;
    let doc = d.doc.borrow();
    match (doc.data(n), doc.shadow_host(n)) {
        (NodeData::Element { .. }, _) => Ok((n, None)),
        (NodeData::ShadowRoot, Some(h)) => Ok((n, Some(h))),
        _ => Err(it.type_error("Illegal invocation")),
    }
}

pub(crate) fn inner_html(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let (n, _) = inner_target(d, it, this)?;
    let s = serialize(d, it, &d.doc.borrow(), n, false)?;
    d.js_str(it, &s)
}

pub(crate) fn outer_html(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let n = d.el(it, this)?;
    let s = serialize(d, it, &d.doc.borrow(), n, true)?;
    d.js_str(it, &s)
}

/// `el.outerHTML = value`: parse in the context of the parent, then put the result in the place of `el`.
/// No parent: nothing happens. A Document parent: `NoModificationAllowedError`. A DocumentFragment parent: the context is `body`.
pub(crate) fn set_outer_html(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let n = d.el(it, this)?;
    let v = arg(a, 0);
    let html = if matches!(v, Value::Null) { String::new() } else { d.string_arg(it, v)? };
    let parent = {
        let doc = d.doc.borrow();
        doc.parent(n).map(|p| (p, matches!(doc.data(p), NodeData::Document), matches!(doc.data(p), NodeData::ShadowRoot)))
    };
    match parent {
        None => Ok(Value::Undefined),
        Some((_, true, _)) => Err(d.dom_err(it, "NoModificationAllowedError", "Failed to set the 'outerHTML' property on 'Element': This element's parent is of type '#document'.")),
        Some((p, _, frag)) => replace(d, it, &html, p, frag, Put::Over(n), None),
    }
}

pub(crate) fn set_inner_html(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let (n, ctx) = inner_target(d, it, this)?;
    let v = arg(a, 0);
    // ToString may run page code; the target is read after it.
    let html = if matches!(v, Value::Null) { String::new() } else { d.string_arg(it, v)? };
    replace(d, it, &html, n, false, Put::All, ctx)
}

/// Where the parsed nodes go: instead of all the children of `n`, instead of one child of `n`, or in `n` before a node (None: last), keeping the rest.
#[derive(Clone, Copy)]
enum Put {
    All,
    Over(NodeId),
    At(Option<NodeId>),
}

/// `insertAdjacentHTML(where, html)`: parse in the context of the element (`afterbegin`, `beforeend`) or of its parent, then put the nodes in place. No text joins.
pub(crate) fn insert_adjacent_html(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    const NAME: &str = "insertAdjacentHTML";
    let (me, ps) = crate::browser_dom::adjacent_start(d, it, this, a, NAME)?;
    let html = d.string_arg(it, a[1])?;
    let pos = crate::browser_dom::adjacent_pos(d, it, NAME, &ps)?;
    // A parent that is a Document gives no context, as no parent.
    let at = crate::browser_dom::adjacent_place(d, me, pos).filter(|&(n, _)| !matches!(d.data(n), NodeData::Document));
    let Some((n, before)) = at else {
        return Err(d.dom_err(it, "NoModificationAllowedError", "Failed to execute 'insertAdjacentHTML' on 'Element': The element has no parent."));
    };
    replace(d, it, &html, n, matches!(d.data(n), NodeData::ShadowRoot), Put::At(before), None)
}

/// The size of the contents of the templates in the tree under `root`, nested ones too: (nodes, bytes, attributes, height). A template's contents hang
/// one level below it. The tree of `root` itself is measured by the caller.
#[cold]
fn measure_templates(it: &mut Interp<'_>, doc: &Document, root: NodeId) -> R<(usize, usize, usize, usize)> {
    let (mut count, mut bytes, mut attrs, mut height) = (0usize, 0usize, 0usize, 0usize);
    let mut jobs = vec![(root, 0usize)];
    while let Some((top, below)) = jobs.pop() {
        let (mut cur, mut depth) = (top, 0usize);
        while let Some((c, cd)) = next_in(doc, top, cur, depth) {
            (cur, depth) = (c, cd);
            let own = top != root;
            if own {
                it.charge(1)?;
                count += 1;
                height = height.max(below + cd);
            }
            match *doc.data(c) {
                NodeData::Text(sp) | NodeData::Comment(sp) if own => bytes += doc.text(sp).len(),
                NodeData::Element { attrs: list, .. } => {
                    if own {
                        let list = doc.attrs(list);
                        attrs += list.len();
                        bytes += list.iter().map(|x| doc.text(x.value).len()).sum::<usize>();
                    }
                    if let Some(f) = doc.template_content(c) {
                        count += 1;
                        jobs.push((f, below + cd));
                    }
                }
                _ => {}
            }
        }
    }
    Ok((count, bytes, attrs, height))
}

/// Parse `html` in the context of `n` (`body` when `in_frag`: `n` is a DocumentFragment). `put`: where the nodes go. `ctx` (`createContextualFragment`): the
/// context is that node, not `n`, which only holds the result; the scripts of the result are not marked as started, so they run when they go in a page.
fn replace(d: &Dom, it: &mut Interp<'_>, html: &str, n: NodeId, in_frag: bool, put: Put, ctx: Option<NodeId>) -> V {
    let c0 = ctx.unwrap_or(n);
    let this = if let Put::Over(t) = put { Some(t) } else { None };
    let all = matches!(put, Put::All);
    let (context, ns, in_form) = {
        let doc = d.doc.borrow();
        let mut in_form = false;
        let mut cur = if in_frag { None } else { Some(c0) };
        while let Some(c) = cur {
            it.charge(1)?;
            if doc.is_html(c, "form") {
                in_form = true;
                break;
            }
            cur = doc.parent(c);
        }
        if in_frag { ("body".to_string(), Ns::Html, false) } else { (doc.element_name(c0).to_string(), doc.element_ns(c0), in_form) }
    };
    // `template.innerHTML = ...` fills the contents of the template. Scripting is off for the template and for the nodes in its contents.
    let tpl_ctx = !in_frag && ns == Ns::Html && context == "template";
    let scripting = !(tpl_ctx || d.doc.borrow().in_template_content(c0));
    // The contents of a template hang one level below it.
    let base = d.depth_of(it, n)? + (all && tpl_ctx) as usize;
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
    let parsed = parse_fragment(html, &context, ns, in_form, scripting, &lim, &mut |units| match it.charge(units) {
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

    let tpl = frag.doc.has_template_content();
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
    if tpl {
        let m = measure_templates(it, &frag.doc, frag.root)?;
        (count, bytes, attrs, height) = (count + m.0, bytes + m.1, attrs + m.2, height.max(m.3));
    }
    let old = {
        let doc = d.doc.borrow();
        let mut old = 0u64;
        let holder = if all && tpl_ctx { doc.template_content(n) } else { Some(n) };
        for _ in holder.into_iter().flat_map(|h| doc.children(h)).take(if all { usize::MAX } else { 0 }) {
            it.charge(1)?;
            old += 1;
        }
        old += u64::from(this.is_some());
        // Text next to the result joins it (below): that copies the two texts.
        let seam: usize = this.iter().flat_map(|&t| [doc.prev_sibling(t), doc.next_sibling(t)]).flatten().map(|x| if let NodeData::Text(sp) = *doc.data(x) { doc.text(sp).len() } else { 0 }).sum();
        d.room(&doc, count, bytes + seam)?;
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
    let group = if all && tpl_ctx { d.doc.borrow().template_content(n) } else { all.then_some(n) };
    // Chrome upgrades the elements of the new nodes before it removes the old ones: the mark says where they go in the queue of reactions.
    let ce = d.ce.on.get() && scripting;
    let (mark, mut made) = (if ce { d.ce_mark() } else { 0 }, Vec::new());
    d.mo_group(group, || {
    let mut doc = d.doc.borrow_mut();
    let n = if all && tpl_ctx { doc.make_template_content(n) } else { n };
    let (prev, before) = match put {
        Put::At(b) => (None, b),
        _ => this.map_or((None, None), |t| (doc.prev_sibling(t), doc.next_sibling(t))),
    };
    match put {
        Put::Over(t) => doc.detach(t),
        Put::At(_) => {}
        Put::All => {
            d.iter_told_all(&doc, n);
            while let Some(c) = doc.children(n).next() {
                doc.detach(c);
            }
            d.iter_told_end();
        }
    }
    // Each job copies a tree of the parsed fragment under a node: the fragment itself, then the contents of each template.
    let mut jobs = Vec::new();
    let (mut src, mut dst, mut first) = (frag.root, n, before);
    loop {
        let mut path = vec![dst];
        let (mut cur, mut depth) = (src, 0usize);
        while let Some((c, cd)) = next_in(&frag.doc, src, cur, depth) {
            (cur, depth) = (c, cd);
            let t = doc.import_node(&frag.doc, c).expect("a node of the fragment");
            doc.move_to(t, path[cd - 1], if cd == 1 { first } else { None });
            if ce && src == frag.root && matches!(*doc.data(t), NodeData::Element { .. }) {
                made.push(t);
            }
            path.truncate(cd);
            path.push(t);
            if ctx.is_none() && doc.is_html(t, "script") {
                d.mark_started(t);
            }
            if tpl && let Some(f) = frag.doc.template_content(c) {
                jobs.push((f, doc.make_template_content(t)));
            }
        }
        let Some(j) = jobs.pop() else { break };
        (src, dst, first) = (j.0, j.1, None);
    }
    if ce {
        d.fragment_upgrades(&doc, mark, &made);
    }
    // As in Chrome, the text before and after the new nodes joins them. The DOM Parsing standard does not say it.
    if this.is_some() {
        let last = before.and_then(|b| doc.prev_sibling(b));
        for a in [last, prev].into_iter().flatten() {
            join_text(&mut doc, a);
        }
    }
    Ok(())
    })?;
    if frag.doc.find_atom("iframe").is_some() { crate::browser_frames::activate(d, it)?; }
    Ok(Value::Undefined)
}

/// If `a` and its next sibling are both text, put the text of the second in the first and remove the second.
fn join_text(doc: &mut Document, a: NodeId) {
    let Some(b) = doc.next_sibling(a) else { return };
    if let (NodeData::Text(x), NodeData::Text(y)) = (*doc.data(a), *doc.data(b)) {
        let s = format!("{}{}", doc.text(x), doc.text(y));
        doc.set_text(a, &s);
        doc.detach(b);
    }
}

/// `Range.prototype.createContextualFragment(markup)` (DOM Parsing standard): parse `markup` in the context of the start container of the range, into a new
/// fragment. The context is the start node when it is an element, its parent for a Text or a Comment, and a new `body` when that is not an element or
/// is `html`. The scripts of the fragment are not marked as started (Chrome runs them when they go in a page). No record, no range moves.
pub(crate) fn contextual_fragment(d: &Dom, it: &mut Interp<'_>, start: NodeId, html: &str) -> V {
    let ctx = {
        let doc = d.doc.borrow();
        let up = if matches!(doc.data(start), NodeData::Element { .. }) { Some(start) } else if matches!(doc.data(start), NodeData::Text(_) | NodeData::Comment(_)) { doc.parent(start) } else { None };
        up.filter(|&e| matches!(doc.data(e), NodeData::Element { .. }) && !doc.is_html(e, "html"))
    };
    let frag = crate::browser_dom::new_fragment(d, it, None)?;
    let f = d.node_of(frag).expect("a fragment");
    replace(d, it, html, f, ctx.is_none(), Put::At(None), Some(ctx.unwrap_or(f)))?;
    Ok(frag)
}


pub(crate) fn write_at(d: &Dom,it: &mut Interp<'_>,text: &str,parent: NodeId,before: Option<NodeId>)->V {
    replace(d,it,text,parent,false,Put::At(before),None)
}
