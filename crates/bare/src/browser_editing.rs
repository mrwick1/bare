//! `isContentEditable`, `contentEditable` and `document.execCommand` with the text commands. Ported, simplified, from Ladybird
//! (BSD-2-Clause, commit 2e2af3549a27a06369f72f0af4cfaefef4201b3c; notice in `crates/bare/LICENSE-LADYBIRD`), which follows the W3C
//! execCommand draft (https://w3c.github.io/editing/docs/execCommand/):
//! - `Libraries/LibWeb/Editing/ExecCommand.cpp`: the entry points `execCommand` and `queryCommand*`.
//! - `Libraries/LibWeb/Editing/CommandNames.cpp`: the command names, matched without case.
//! - `Libraries/LibWeb/Editing/Commands.cpp`: the actions of `insertText`, `delete`, `forwardDelete`, `insertParagraph`,
//!   `insertLineBreak` and `selectAll`.
//! - `Libraries/LibWeb/Editing/Internal/Algorithms.cpp`: "editing host" and "active range".
//!
//! What is built
//! - `HTMLElement.prototype.isContentEditable` and `contentEditable` (`"true"`, `"false"`, `"plaintext-only"`, `"inherit"`).
//! - `document.execCommand(name, showUI, value)`, `queryCommandSupported`, `queryCommandEnabled`, `queryCommandState` (always false),
//!   `queryCommandValue` (always `""`) and `queryCommandIndeterm` (always false).
//! - The commands act on the first range of `getSelection()` when both ends are in one editing host. Each one that changes the tree
//!   fires an `input` `InputEvent` (bubbles, not cancelable) at the host. `beforeinput` is not fired for a script call, as in Chrome.
//!   `selectAll` fires nothing. A command that has nothing to do (Backspace at the start of the host) returns false.
//! - The edits are made with the DOM methods that a page can call (`insertData`, `deleteData`, `splitText`, `insertBefore`,
//!   `extractContents`), so the live ranges and the mutation records are the ones of the DOM code.
//! - Parts b and c call `insert_text`, `delete_back`, `delete_forward`, `insert_paragraph`, `insert_line_break` and `select_all` of
//!   `Dom` (`pub(crate)`). Each one returns whether it changed the tree.
//!
//! Gaps (on purpose, listed)
//! - `document.designMode` stays "off" (`browser_basics.rs`); the setter ignores "on".
//! - `insertText` puts the text in as given: no `&nbsp;` for a space at an edge, and a line feed in the text is a line feed.
//! - `delete` and `forwardDelete` do not merge two blocks at a block boundary; they return false there. A range across blocks is cut
//!   with `deleteContents`, which keeps both blocks.
//! - `insertParagraph` makes a `div` in the host and clones the block it splits (a `p` gives `p`). Lists and tables are not special.
//! - No undo history, no `beforeinput`, no `styleWithCSS`, no `defaultParagraphSeparator`.

use crate::browser::*;
use crate::browser_ui::Ui;
use bare_html::dom::{Document, NodeData, NodeId};
use bare_js::eval::Interp;
use bare_js::value::Value;

type V = R<Value>;

fn arg(a: &[Value], i: usize) -> Value {
    a.get(i).copied().unwrap_or(Value::Undefined)
}

fn num(n: u32) -> Value {
    Value::Number(n as f64)
}

// ------------------------------------------------------------------------------------ reading the tree

fn attr<'a>(doc: &'a Document, n: NodeId, name: &str) -> Option<&'a str> {
    doc.find_atom(name).and_then(|a| doc.attr(n, a))
}

fn is_element(doc: &Document, n: NodeId) -> bool {
    matches!(doc.data(n), NodeData::Element { .. })
}

/// The element of `n`: `n` itself, or the parent of a Text or Comment.
fn element_of(doc: &Document, n: NodeId) -> Option<NodeId> {
    if is_element(doc, n) { Some(n) } else { doc.parent(n).filter(|&p| is_element(doc, p)) }
}

/// Is `n` editable? The nearest element with a valid `contenteditable` decides ("false" is no).
fn editable(doc: &Document, n: NodeId) -> bool {
    let mut cur = element_of(doc, n);
    while let Some(e) = cur {
        match attr(doc, e, "contenteditable").map(str::to_ascii_lowercase).as_deref() {
            Some("" | "true" | "plaintext-only") => return true,
            Some("false") => return false,
            _ => {}
        }
        cur = doc.parent(e).filter(|&p| is_element(doc, p));
    }
    false
}

/// The editing host of `n`: the topmost editable element above it.
fn host_of(doc: &Document, n: NodeId) -> Option<NodeId> {
    if !editable(doc, n) {
        return None;
    }
    let mut h = element_of(doc, n)?;
    while let Some(p) = doc.parent(h).filter(|&p| is_element(doc, p) && editable(doc, p)) {
        h = p;
    }
    Some(h)
}

fn tag(doc: &Document, n: NodeId) -> &str {
    match *doc.data(n) {
        NodeData::Element { name, .. } => doc.atom_str(name),
        _ => "",
    }
}

fn is_void(doc: &Document, n: NodeId) -> bool {
    !is_element(doc, n) || matches!(tag(doc, n), "br" | "img" | "hr" | "input" | "wbr")
}

fn is_block(doc: &Document, n: NodeId) -> bool {
    matches!(tag(doc, n), "div" | "p" | "li" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "blockquote" | "pre" | "ul" | "ol" | "td" | "th" | "section" | "article")
}

fn units(doc: &Document, n: NodeId) -> Option<Vec<u16>> {
    match *doc.data(n) {
        NodeData::Text(s) => Some(doc.text(s).encode_utf16().collect()),
        _ => None,
    }
}

fn index(doc: &Document, n: NodeId) -> u32 {
    let mut i = 0;
    let mut cur = doc.prev_sibling(n);
    while let Some(p) = cur {
        i += 1;
        cur = doc.prev_sibling(p);
    }
    i
}

/// The first position inside `n` where text goes: in its first Text, going down through inline elements.
fn deep_start(doc: &Document, mut n: NodeId) -> (NodeId, u32) {
    while let Some(f) = doc.children(n).next() {
        match doc.data(f) {
            NodeData::Text(_) => return (f, 0),
            _ if !is_void(doc, f) && !is_block(doc, f) => n = f,
            _ => break,
        }
    }
    (n, 0)
}

/// What a Backspace or Delete takes away.
enum Del {
    Chars(NodeId, u32, u32),
    Remove(NodeId),
}

/// The thing one step from the caret `(c, o)` inside `host`, backward or forward. Walks out of inline elements; stops at a block edge.
fn find_del(doc: &Document, host: NodeId, mut c: NodeId, mut o: u32, back: bool) -> Option<Del> {
    loop {
        if let Some(u) = units(doc, c) {
            let len = u.len() as u32;
            let at = o.min(len);
            if back && at > 0 {
                let two = at >= 2 && (0xDC00..0xE000).contains(&u[at as usize - 1]) && (0xD800..0xDC00).contains(&u[at as usize - 2]);
                return Some(Del::Chars(c, at - if two { 2 } else { 1 }, at));
            }
            if !back && at < len {
                let two = at + 1 < len && (0xD800..0xDC00).contains(&u[at as usize]) && (0xDC00..0xE000).contains(&u[at as usize + 1]);
                return Some(Del::Chars(c, at, at + if two { 2 } else { 1 }));
            }
            let p = doc.parent(c)?;
            (o, c) = (index(doc, c) + !back as u32, p);
            continue;
        }
        let kids: Vec<NodeId> = doc.children(c).collect();
        let n = kids.len() as u32;
        if (back && o > 0) || (!back && o < n) {
            let ch = kids[if back { o - 1 } else { o } as usize];
            if let Some(u) = units(doc, ch) {
                (c, o) = (ch, if back { u.len() as u32 } else { 0 });
            } else if is_void(doc, ch) {
                return Some(Del::Remove(ch));
            } else {
                (c, o) = (ch, if back { doc.children(ch).count() as u32 } else { 0 });
            }
        } else {
            if c == host || is_block(doc, c) {
                return None;
            }
            let p = doc.parent(c)?;
            (o, c) = (index(doc, c) + !back as u32, p);
        }
    }
}

// ------------------------------------------------------------------------------------ calling the DOM

fn call(d: &Dom, it: &mut Interp<'_>, recv: Value, name: &str, args: &[Value]) -> V {
    let k = d.key(it, name)?;
    let f = it.get_value(recv, k)?;
    it.call(f, recv, args)
}

/// The caret as a position of the selection.
fn caret(d: &Dom, it: &mut Interp<'_>, sel: Value, n: NodeId, off: u32) -> R<()> {
    let w = d.wrap(it, n)?;
    call(d, it, sel, "collapse", &[w, num(off)]).map(drop)
}

/// The first range of the selection and where it is.
struct Act {
    sel: Value,
    range: Value,
    host: Option<NodeId>,
    start: (NodeId, u32),
    end: (NodeId, u32),
}

fn active(d: &Dom, it: &mut Interp<'_>, docv: Value) -> R<Option<Act>> {
    let sel = call(d, it, docv, "getSelection", &[])?;
    let k = d.key(it, "rangeCount")?;
    if !matches!(it.get_value(sel, k)?, Value::Number(n) if n >= 1.0) {
        return Ok(None);
    }
    let range = call(d, it, sel, "getRangeAt", &[num(0)])?;
    let (sc, so, ec, eo) = crate::browser_range::bounds(d, it, range)?;
    let doc = d.doc.borrow();
    let host = host_of(&doc, sc).filter(|&h| host_of(&doc, ec) == Some(h));
    Ok(Some(Act { sel, range, host, start: (sc, so), end: (ec, eo) }))
}

fn doc_value(d: &Dom) -> Value {
    Value::Object(*d.doc_wrapper.get().expect("installed"))
}

impl Dom {
    /// The editing host that `n` is in, if any.
    pub(crate) fn editing_host(&self, n: NodeId) -> Option<NodeId> {
        host_of(&self.doc.borrow(), n)
    }

    /// Put a collapsed caret at the end of the host of `n`, unless the selection is already in that host.
    pub(crate) fn caret_into_host(&self, it: &mut Interp<'_>, n: NodeId) -> R<()> {
        let Some(host) = self.editing_host(n) else { return Ok(()) };
        if self.editing(it)?.is_some_and(|(_, h)| h == host) {
            return Ok(());
        }
        let sel = call(self, it, doc_value(self), "getSelection", &[])?;
        let end = self.doc.borrow().children(host).count() as u32;
        caret(self, it, sel, host, end)
    }

    /// The active range, when it is in an editing host.
    fn editing(&self, it: &mut Interp<'_>) -> R<Option<(Act, NodeId)>> {
        Ok(active(self, it, doc_value(self))?.and_then(|a| a.host.map(|h| (a, h))))
    }

    /// Delete a non-collapsed selection. Gives the caret.
    fn cut_selection(&self, it: &mut Interp<'_>, a: &Act) -> R<(NodeId, u32)> {
        if a.start != a.end {
            call(self, it, a.range, "deleteContents", &[])?;
        }
        let (sc, so, _, _) = crate::browser_range::bounds(self, it, a.range)?;
        Ok((sc, so))
    }

    fn input_event(&self, it: &mut Interp<'_>, host: NodeId, kind: &str, data: Option<&str>) -> R<()> {
        let ui = Ui { data: data.map(str::to_string), input_type: kind.to_string(), ..Ui::default() };
        self.fire_ui(it, host, "input", self.p().inputev, true, false, ui, None).map(drop)
    }

    /// `insertText`: replace the selection with `text`, the caret after it. False when the selection is not in an editing host.
    pub(crate) fn insert_text(&self, it: &mut Interp<'_>, text: &str) -> R<bool> {
        let Some((a, host)) = self.editing(it)? else { return Ok(false) };
        if text.is_empty() {
            return Ok(false);
        }
        let (c, o) = self.cut_selection(it, &a)?;
        let add = self.js_str(it, text)?;
        let len = text.encode_utf16().count() as u32;
        // The Text to put it in: the one the caret is in, or a neighbour of the caret, or a new one.
        let (target, at) = {
            let doc = self.doc.borrow();
            if units(&doc, c).is_some() {
                (Some(c), o)
            } else {
                let kids: Vec<NodeId> = doc.children(c).collect();
                let prev = o.checked_sub(1).and_then(|i| kids.get(i as usize)).copied().filter(|&k| units(&doc, k).is_some());
                let next = kids.get(o as usize).copied().filter(|&k| units(&doc, k).is_some());
                match (prev, next) {
                    (Some(p), _) => (Some(p), units(&doc, p).map_or(0, |u| u.len() as u32)),
                    (None, Some(n)) => (Some(n), 0),
                    _ => (None, 0),
                }
            }
        };
        let (node, at) = match target {
            Some(t) => {
                let w = self.wrap(it, t)?;
                call(self, it, w, "insertData", &[num(at), add])?;
                (t, at)
            }
            None => {
                let t = call(self, it, doc_value(self), "createTextNode", &[add])?;
                let before = self.doc.borrow().children(c).nth(o as usize);
                let before = self.wrap_opt(it, before)?;
                let cw = self.wrap(it, c)?;
                call(self, it, cw, "insertBefore", &[t, before])?;
                (self.nd(it, t)?, 0)
            }
        };
        caret(self, it, a.sel, node, at + len)?;
        self.input_event(it, host, "insertText", Some(text))?;
        Ok(true)
    }

    fn delete_step(&self, it: &mut Interp<'_>, back: bool) -> R<bool> {
        let Some((a, host)) = self.editing(it)? else { return Ok(false) };
        let kind = if back { "deleteContentBackward" } else { "deleteContentForward" };
        if a.start != a.end {
            self.cut_selection(it, &a)?;
            self.input_event(it, host, kind, None)?;
            return Ok(true);
        }
        let found = find_del(&self.doc.borrow(), host, a.start.0, a.start.1, back);
        let Some(found) = found else { return Ok(false) };
        match found {
            Del::Chars(n, from, to) => {
                let w = self.wrap(it, n)?;
                call(self, it, w, "deleteData", &[num(from), num(to - from)])?;
                let (empty, up) = {
                    let doc = self.doc.borrow();
                    (units(&doc, n).is_some_and(|u| u.is_empty()), doc.parent(n).map(|p| (p, index(&doc, n))))
                };
                match up {
                    Some((p, i)) if empty => {
                        let pw = self.wrap(it, p)?;
                        call(self, it, pw, "removeChild", &[w])?;
                        caret(self, it, a.sel, p, i)?;
                    }
                    _ => caret(self, it, a.sel, n, from)?,
                }
            }
            Del::Remove(n) => {
                let (p, i) = {
                    let doc = self.doc.borrow();
                    (doc.parent(n).expect("in the host"), index(&doc, n))
                };
                let (pw, w) = (self.wrap(it, p)?, self.wrap(it, n)?);
                call(self, it, pw, "removeChild", &[w])?;
                caret(self, it, a.sel, p, i)?;
            }
        }
        self.input_event(it, host, kind, None)?;
        Ok(true)
    }

    /// `delete` (Backspace).
    pub(crate) fn delete_back(&self, it: &mut Interp<'_>) -> R<bool> {
        self.delete_step(it, true)
    }

    /// `forwardDelete` (Delete).
    pub(crate) fn delete_forward(&self, it: &mut Interp<'_>) -> R<bool> {
        self.delete_step(it, false)
    }

    /// A `br` when the block has nothing to show, so that the empty line has a height.
    fn fill_empty(&self, it: &mut Interp<'_>, block: NodeId) -> R<()> {
        let empty = {
            let doc = self.doc.borrow();
            doc.children(block).all(|k| units(&doc, k).is_some_and(|u| u.is_empty()))
        };
        if empty {
            let tag = self.js_str(it, "br")?;
        let br = call(self, it, doc_value(self), "createElement", &[tag])?;
            let w = self.wrap(it, block)?;
            call(self, it, w, "appendChild", &[br])?;
        }
        Ok(())
    }

    /// `insertParagraph`: split the block of the caret in two. Text straight in the host goes into a new `div` first.
    pub(crate) fn insert_paragraph(&self, it: &mut Interp<'_>) -> R<bool> {
        let Some((a, host)) = self.editing(it)? else { return Ok(false) };
        let (mut c, o) = self.cut_selection(it, &a)?;
        let mut block = {
            let doc = self.doc.borrow();
            let mut b = element_of(&doc, c).expect("in the host");
            while b != host && !is_block(&doc, b) {
                b = doc.parent(b).expect("in the host");
            }
            b
        };
        if block == host {
            let tag = self.js_str(it, "div")?;
            let div = call(self, it, doc_value(self), "createElement", &[tag])?;
            let hw = self.wrap(it, host)?;
            loop {
                let first = self.doc.borrow().children(host).next();
                let Some(f) = first else { break };
                let fw = self.wrap(it, f)?;
                call(self, it, div, "appendChild", &[fw])?;
            }
            call(self, it, hw, "appendChild", &[div])?;
            block = self.nd(it, div)?;
            if c == host {
                c = block;
            }
            caret(self, it, a.sel, c, o)?;
        }
        // The tail of the block goes to a shallow clone of it, which comes after it.
        let (cw, bw) = (self.wrap(it, c)?, self.wrap(it, block)?);
        let r = call(self, it, doc_value(self), "createRange", &[])?;
        let n = self.doc.borrow().children(block).count() as u32;
        call(self, it, r, "setStart", &[cw, num(o)])?;
        call(self, it, r, "setEnd", &[bw, num(n)])?;
        let tail = call(self, it, r, "extractContents", &[])?;
        let fresh = call(self, it, bw, "cloneNode", &[Value::Bool(false)])?;
        call(self, it, fresh, "appendChild", &[tail])?;
        let after = self.doc.borrow().next_sibling(block);
        let after = self.wrap_opt(it, after)?;
        let pw = self.wrap(it, self.doc.borrow().parent(block).expect("in the host"))?;
        call(self, it, pw, "insertBefore", &[fresh, after])?;
        let fresh = self.nd(it, fresh)?;
        self.fill_empty(it, block)?;
        self.fill_empty(it, fresh)?;
        let (t, off) = deep_start(&self.doc.borrow(), fresh);
        caret(self, it, a.sel, t, off)?;
        self.input_event(it, host, "insertParagraph", None)?;
        Ok(true)
    }

    /// `insertLineBreak`: a `br` at the caret. A `br` at the end of a block gets a second one, so that the new line shows.
    pub(crate) fn insert_line_break(&self, it: &mut Interp<'_>) -> R<bool> {
        let Some((a, host)) = self.editing(it)? else { return Ok(false) };
        let (c, o) = self.cut_selection(it, &a)?;
        let tag = self.js_str(it, "br")?;
        let br = call(self, it, doc_value(self), "createElement", &[tag])?;
        let text_len = units(&self.doc.borrow(), c).map(|u| u.len() as u32);
        let parent = if text_len.is_some() { self.doc.borrow().parent(c).expect("in the host") } else { c };
        let before = if let Some(len) = text_len {
            if o == 0 {
                Some(c)
            } else if o >= len {
                self.doc.borrow().next_sibling(c)
            } else {
                let w = self.wrap(it, c)?;
                let tail = call(self, it, w, "splitText", &[num(o)])?;
                self.node_of(tail)
            }
        } else {
            self.doc.borrow().children(c).nth(o as usize)
        };
        let (pw, bw) = (self.wrap(it, parent)?, self.wrap_opt(it, before)?);
        call(self, it, pw, "insertBefore", &[br, bw])?;
        let at = {
            let doc = self.doc.borrow();
            let b = self.node_of(br).expect("a node");
            (doc.next_sibling(b).is_none(), index(&doc, b) + 1)
        };
        if at.0 {
            let again = call(self, it, br, "cloneNode", &[Value::Bool(false)])?;
            call(self, it, pw, "appendChild", &[again])?;
        }
        caret(self, it, a.sel, parent, at.1)?;
        self.input_event(it, host, "insertLineBreak", None)?;
        Ok(true)
    }

    /// `selectAll`: the contents of the editing host, or of the body when the selection is not in one.
    pub(crate) fn select_all(&self, it: &mut Interp<'_>) -> R<bool> {
        let docv = doc_value(self);
        let root = match self.editing(it)? {
            Some((_, h)) => self.wrap(it, h)?,
            None => {
                let k = self.key(it, "body")?;
                match it.get_value(docv, k)? {
                    Value::Object(b) => Value::Object(b),
                    _ => {
                        let k = self.key(it, "documentElement")?;
                        it.get_value(docv, k)?
                    }
                }
            }
        };
        let sel = call(self, it, docv, "getSelection", &[])?;
        call(self, it, sel, "selectAllChildren", &[root])?;
        Ok(true)
    }
}

// ------------------------------------------------------------------------------------ document methods

const COMMANDS: [&str; 6] = ["inserttext", "delete", "forwarddelete", "insertparagraph", "insertlinebreak", "selectall"];

fn name_arg(d: &Dom, it: &mut Interp<'_>, a: &[Value]) -> R<String> {
    Ok(d.string_arg(it, arg(a, 0))?.to_ascii_lowercase())
}

fn exec_command(d: &Dom, it: &mut Interp<'_>, _: Value, a: &[Value]) -> V {
    let name = name_arg(d, it, a)?;
    let ok = match name.as_str() {
        "inserttext" => {
            let text = if matches!(arg(a, 2), Value::Undefined) { String::new() } else { d.string_arg(it, arg(a, 2))? };
            d.insert_text(it, &text)?
        }
        "delete" => d.delete_back(it)?,
        "forwarddelete" => d.delete_forward(it)?,
        "insertparagraph" => d.insert_paragraph(it)?,
        "insertlinebreak" => d.insert_line_break(it)?,
        "selectall" => d.select_all(it)?,
        _ => false,
    };
    Ok(Value::Bool(ok))
}

fn query_supported(d: &Dom, it: &mut Interp<'_>, _: Value, a: &[Value]) -> V {
    let name = name_arg(d, it, a)?;
    Ok(Value::Bool(COMMANDS.contains(&name.as_str())))
}

fn query_enabled(d: &Dom, it: &mut Interp<'_>, _: Value, a: &[Value]) -> V {
    let name = name_arg(d, it, a)?;
    Ok(Value::Bool(match name.as_str() {
        "selectall" => true,
        n if COMMANDS.contains(&n) => d.editing(it)?.is_some(),
        _ => false,
    }))
}

fn query_state(_: &Dom, _: &mut Interp<'_>, _: Value, _: &[Value]) -> V {
    Ok(Value::Bool(false))
}

fn query_value(d: &Dom, it: &mut Interp<'_>, _: Value, _: &[Value]) -> V {
    d.js_str(it, "")
}

// ------------------------------------------------------------------------------------ HTMLElement

fn is_content_editable(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let n = d.nd(it, this)?;
    Ok(Value::Bool(editable(&d.doc.borrow(), n)))
}

fn content_editable(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let n = d.nd(it, this)?;
    if a.is_empty() {
        let v = attr(&d.doc.borrow(), n, "contenteditable").map(str::to_ascii_lowercase);
        return d.js_str(it, match v.as_deref() {
            Some("" | "true") => "true",
            Some("false") => "false",
            Some("plaintext-only") => "plaintext-only",
            _ => "inherit",
        });
    }
    let v = d.string_arg(it, a[0])?.to_ascii_lowercase();
    match v.as_str() {
        "inherit" => {
            let k = d.js_str(it, "contenteditable")?;
            call(d, it, this, "removeAttribute", &[k])?
        }
        "true" | "false" | "plaintext-only" => {
            let (k, s) = (d.js_str(it, "contenteditable")?, d.js_str(it, &v)?);
            call(d, it, this, "setAttribute", &[k, s])?
        }
        _ => return Err(d.dom_err(it, "SyntaxError", &format!("Failed to set the 'contentEditable' property on 'HTMLElement': The value provided ('{v}') is not one of 'true', 'false', 'plaintext-only', or 'inherit'."))),
    };
    Ok(Value::Undefined)
}

pub(crate) fn install(d: &Dom, it: &mut Interp<'_>) -> R<()> {
    let doc = d.p().document;
    d.def_method(it, doc, "execCommand", 1, exec_command)?;
    d.def_method(it, doc, "queryCommandSupported", 1, query_supported)?;
    d.def_method(it, doc, "queryCommandEnabled", 1, query_enabled)?;
    d.def_method(it, doc, "queryCommandState", 1, query_state)?;
    d.def_method(it, doc, "queryCommandIndeterm", 1, query_state)?;
    d.def_method(it, doc, "queryCommandValue", 1, query_value)?;
    d.def_get(it, d.p().html, "isContentEditable", is_content_editable, None)?;
    d.def_get(it, d.p().html, "contentEditable", content_editable, Some(content_editable))?;
    Ok(())
}
