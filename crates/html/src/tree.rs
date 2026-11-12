//! The tree builder: turn tokens into a DOM tree.
//!
//! The tokenizer gives tokens. The tree builder decides where each one goes. Its job is to make a
//! good tree out of bad HTML, in the one way that the HTML standard says. For example:
//! - `<title>Hi</title><p>x` has no `<html>`, `<head>` or `<body>`. The builder adds them.
//! - `<p>a<p>b` has no `</p>`. A new `<p>` closes the open one.
//! - `<table>x<tr><td>y` has text in a table, where it is not allowed. The builder moves the
//!   text in front of the table. This is called "foster parenting".
//!
//! The builder keeps three things:
//! - The stack of open elements. When we read `<div>`, the new div is the child of the element at the
//!   top of the stack, and then it goes on the stack. When we read `</div>`, we pop the stack.
//!   The element at the top is the "current node".
//! - The list of active formatting elements: the `<b>`, `<i>`, `<a>` and the like that are open. When an end tag comes in
//!   the wrong place (`<b>1<p>2</b>3</p>`), the adoption agency algorithm fixes the tree with copies of these elements,
//!   and the same list opens them again when new content comes after (`<p><b>1</p>2`). Chrome gives the same trees.
//! - The insertion mode. The standard has about 20 modes, like "before head", "in body" and "in table".
//!   Each mode says what to do with each kind of token. We use a few modes. For tables we look at the
//!   stack instead of keeping modes: if the current node is a `tr`, we are "in a row".
//!   A `<template>` is the one place where a table mode has no element to show it: `<template><tr>` is "in table body" with no `tbody` open.
//!   So the builder has the standard's "in template" mode and its stack of template insertion modes (`Builder::tmpl`), and the table rules
//!   read the top of that stack when the nearest table or template is a template.
//!
//! The standard says that every page, however broken, gives a tree. No error stops the builder.
//!
//! Left out (known gaps):
//! - `<select>` modes for a page (a fragment with a `select` context has "in select"), framesets (and so the frameset-ok flag), and the "in head noscript" mode (fragment parsing for `innerHTML` is in `tree/fragment.rs` and gates these). The scripting flag is on: `<noscript>` has raw text. Foreign content (SVG and MathML) is cut down: see `foreign.rs`.
//! - Quirks mode, and merging the attributes of a second `<html>` or `<body>` tag.
//!
//! Comments compare Rust with JavaScript, because you know JS.

mod foreign;
mod fragment;
#[cfg(test)]
mod context_tests;
#[cfg(test)]
mod html5lib_tests;

pub use fragment::{Fragment, FragmentError, FragmentLimits, context_gate, parse_fragment};
use crate::dom::{Document, NodeData, NodeId, Ns};
use std::borrow::Cow;

use crate::tokenizer::{Attr, Mark, Token, Tokens, raw_text_kind};

/// How many entries of the list of active formatting elements the "Noah's Ark" check reads. The standard has no limit,
/// but then 20,000 nested `<i class=a1><i class=a2>...` cost 200 million comparisons. With a limit, an identical element
/// that is more than 256 entries back is not found, so the list can keep a 4th copy. Only a page like that sees a difference.
const NOAH_SCAN: usize = 256;

/// Parse a page (already decoded to text) into a DOM tree.
pub fn parse(input: &str) -> Document {
    let mut builder = Builder::new();
    drive(&mut builder, &mut Tokens::for_tree(input, Mark::default(), false));
    builder.done()
}

/// Give each token to the tree builder. After each one, tell the tokenizer what the tree builder decided: raw text for the HTML element
/// that it just made, and if CDATA is allowed in the current node. The tokenizer reads ahead only to the next token, so it sees both in time.
fn drive(builder: &mut Builder, tokens: &mut Tokens<'_>) {
    tokens.set_foreign(builder.cdata_allowed());
    while let Some(token) = tokens.next() {
        builder.process(token);
        if let Some(name) = builder.raw.take() {
            tokens.enter_raw(name);
        }
        tokens.set_foreign(builder.cdata_allowed());
    }
}

/// A parser for a page that arrives in pieces. Call `feed` for each piece of text, then `finish`.
/// The tree is the same as `parse` gives for the whole text, wherever the pieces are cut.
/// JS: like `TextDecoderStream` feeding a parser. You push chunks, and you close the stream.
/// It keeps only the part of the page that is not read yet: a tag that is cut in the middle, or a
/// long text or script that waits for its end.
pub struct Parser {
    builder: Builder,
    buf: String,
    // The text that the tokenizer did not use yet.
    mark: Mark,
    retry_at: usize,
    // A cut token that is very long (a 10 MB script) would be read again with each piece: slow.
    // So we wait until `buf` is twice as big as when we last stopped. The work is then linear.
}

impl Parser {
    pub fn new() -> Parser {
        Parser { builder: Builder::new(), buf: String::new(), mark: Mark::default(), retry_at: 0 }
    }

    /// Add the next piece of the page. Each token that is complete goes to the tree at once.
    pub fn feed(&mut self, text: &str) {
        self.buf.push_str(text);
        if self.buf.len() < self.retry_at {
            return;
        }
        let mut tokens = Tokens::for_tree(&self.buf, self.mark, true);
        drive(&mut self.builder, &mut tokens);
        let mark = tokens.mark();
        self.buf.drain(..mark.done());
        // Drop the text that we used. What is left is small, unless a token is very long.
        self.mark = mark.rebased();
        self.retry_at = self.buf.len() * 2;
    }

    /// The page is over. Read what is left, and give the tree.
    pub fn finish(mut self) -> Document {
        drive(&mut self.builder, &mut Tokens::for_tree(&self.buf, self.mark, false));
        self.builder.done()
    }
}

impl Default for Parser {
    fn default() -> Parser {
        Parser::new()
    }
}

/// The insertion modes that we use. The names are the ones in the standard.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Mode {
    Initial,
    BeforeHtml,
    BeforeHead,
    InHead,
    AfterHead,
    InBody,
    Text,
    // Inside `<title>`, `<script>`, `<style>` and the like. We only add text, until the end tag.
    AfterBody,
    AfterAfterBody,
    /// Right after `<template>`, and again when the current node is a template whose insertion mode is "in template".
    InTemplate,
    /// The current node is a template whose insertion mode is "in column group" (`<template><col>`). Only `col` can follow.
    InTemplateColGroup,
    /// The context of a fragment is a `select` ("in select", classic rules). A page keeps the `select` element in the stack and reads it as "in body".
    InSelect,
}

/// An entry of the stack of template insertion modes. The standard keeps the mode that a template was switched to by its first content tag.
/// Only the modes that the template itself can be in are here. The other modes (cell, caption...) are shown by the stack of open elements.
#[derive(Clone, Copy, PartialEq)]
enum Tm {
    Template,
    Table,
    ColGroup,
    TableBody,
    Row,
    Body,
}

/// The insertion mode to use while the current node is a template with this template insertion mode.
fn mode_for(tm: Tm) -> Mode {
    match tm {
        Tm::Template => Mode::InTemplate,
        Tm::ColGroup => Mode::InTemplateColGroup,
        _ => Mode::InBody,
    }
}

/// Elements that close an open `<p>` when they start.
const CLOSES_P: &[&str] = &[
    "address", "article", "aside", "blockquote", "center", "details", "dialog", "dir", "div", "dl", "fieldset", "figcaption", "figure", "footer",
    "header", "hgroup", "main", "menu", "nav", "ol", "p", "search", "section", "summary", "ul",
];

/// The "special" elements of the standard. An end tag with no match never closes past one of them.
/// For example `</span>` cannot close the `div` that holds the span.
const SPECIAL: &[&str] = &[
    "address", "applet", "area", "article", "aside", "base", "basefont", "bgsound", "blockquote", "body", "br", "button", "caption", "center", "col",
    "colgroup", "dd", "details", "dir", "div", "dl", "dt", "embed", "fieldset", "figcaption", "figure", "footer", "form", "frame", "frameset", "h1",
    "h2", "h3", "h4", "h5", "h6", "head", "header", "hgroup", "hr", "html", "iframe", "img", "input", "keygen", "li", "link", "listing", "main",
    "marquee", "menu", "meta", "nav", "noembed", "noframes", "noscript", "object", "ol", "p", "param", "plaintext", "pre", "script", "search",
    "section", "select", "source", "style", "summary", "table", "tbody", "td", "template", "textarea", "tfoot", "th", "thead", "title", "tr",
    "track", "ul", "wbr", "xmp",
];

/// Elements whose end tag may be missing. We close them when we close their parent.
const IMPLIED_END: &[&str] = &["dd", "dt", "li", "optgroup", "option", "p", "rb", "rp", "rt", "rtc"];

/// Elements that stop a search for an element "in scope". `</p>` cannot close a `p` that is
/// outside the table cell that we are in.
const SCOPE: &[&str] = &["applet", "caption", "html", "table", "td", "th", "marquee", "object", "template"];

const HEADINGS: &[&str] = &["h1", "h2", "h3", "h4", "h5", "h6"];

/// Parts of a table. Content that is not a table part cannot be a child of these.
const TABLE_PARTS: &[&str] = &["table", "tbody", "tfoot", "thead", "tr"];

/// The stack of open elements. It is a `Vec` of nodes, and it also counts the open `<p>` elements.
///
/// Why: every block start tag (`<div>`, `<ul>`, ...) must close an open `<p>`, and to know if there is one we
/// look down the whole stack. On a page with 10,000 nested `<div>` that is 10,000 steps for each `<div>`,
/// 50 million in all (the time grows with the square of the depth). With the count, a stack with no `<p>`
/// in it answers at once. Reading is the same as for a `Vec` (`Deref` gives the slice of nodes), but a change must go
/// through these methods, so that the count stays right.
///
/// It also answers "is this node on the stack?" at once (the list of active formatting elements asks this for each tag),
/// and it counts the cells that left (see `Builder::sync_afe`).
#[derive(Default)]
struct OpenStack {
    nodes: Vec<NodeId>,
    flags: Vec<u8>,
    // For each node: `P` if it is a `<p>`, and `OWNS_MARKER` if it has a marker in the list of active formatting elements.
    // We need these when the node leaves, and we have no `Document` here.
    p_count: usize,
    /// For each node (by `NodeId::index`) that we track (see `track`): is it on the stack?
    member: Vec<bool>,
    /// How many elements that own a marker have left. The list of active formatting elements drops one marker for each.
    markers_left: usize,
}

const P: u8 = 1;
const OWNS_MARKER: u8 = 2;

impl std::ops::Deref for OpenStack {
    type Target = [NodeId];
    // JS: like a class that is an array: `stack.length`, `stack[i]` and `for...of` all work.

    fn deref(&self) -> &[NodeId] {
        &self.nodes
    }
}

impl OpenStack {
    fn push(&mut self, id: NodeId, is_p: bool) {
        self.nodes.push(id);
        self.flags.push(if is_p { P } else { 0 });
        self.p_count += usize::from(is_p);
    }

    /// Start to track `id`, which is on the stack: after this, `contains` knows it. Only the nodes that the list of
    /// active formatting elements holds need this, so the other nodes do not pay for it.
    fn track(&mut self, id: NodeId) {
        if self.member.len() <= id.index() {
            self.member.resize(id.index() + 1, false);
        }
        self.member[id.index()] = true;
    }

    fn insert(&mut self, i: usize, id: NodeId, flags: u8) {
        self.nodes.insert(i, id);
        self.flags.insert(i, flags);
        self.p_count += usize::from(flags & P != 0);
    }

    fn pop(&mut self) -> Option<NodeId> {
        let i = self.nodes.len().checked_sub(1)?;
        Some(self.remove(i))
    }

    /// Keep the first `len` nodes.
    fn truncate(&mut self, len: usize) {
        while self.nodes.len() > len {
            self.pop();
        }
    }

    fn remove(&mut self, i: usize) -> NodeId {
        let flags = self.flags.remove(i);
        self.p_count -= usize::from(flags & P != 0);
        self.markers_left += usize::from(flags & OWNS_MARKER != 0);
        let id = self.nodes.remove(i);
        if let Some(m) = self.member.get_mut(id.index()) {
            *m = false;
        }
        id
    }

    /// Put `id` where the node `i` is. The new node is not a `<p>` and has no marker.
    fn replace(&mut self, i: usize, id: NodeId) {
        let old = self.nodes[i];
        if let Some(m) = self.member.get_mut(old.index()) {
            *m = false;
        }
        self.nodes[i] = id;
        self.track(id);
    }

    fn contains(&self, id: NodeId) -> bool {
        self.member.get(id.index()).copied().unwrap_or(false)
    }

    /// The place of `id` on the stack. Looks from the top.
    fn position(&self, id: NodeId) -> Option<usize> {
        self.nodes.iter().rposition(|&n| n == id)
    }

    /// The same for a node that we track: it answers at once when the node is not on the stack.
    fn position_tracked(&self, id: NodeId) -> Option<usize> {
        if self.contains(id) { self.position(id) } else { None }
    }

    /// The top node owns a marker.
    fn mark_top(&mut self) {
        if let Some(f) = self.flags.last_mut() {
            *f |= OWNS_MARKER;
        }
    }
}

/// One item of the list of active formatting elements.
#[derive(Clone, Copy, PartialEq)]
enum Afe {
    /// A border. `<td>`, `<th>`, `<caption>`, `<applet>`, `<marquee>` and `<object>` put one in the list when they start.
    /// An end tag cannot reach past it: a `<b>` that is outside a table cell is not reopened inside the cell.
    Marker,
    El(NodeId),
}

/// The elements of the list of active formatting elements: the ones that the adoption agency algorithm fixes.
fn is_formatting(name: &str) -> bool {
    matches!(name, "a" | "b" | "big" | "code" | "em" | "font" | "i" | "nobr" | "s" | "small" | "strike" | "strong" | "tt" | "u")
}

struct Builder {
    doc: Document,
    afe: Vec<Afe>,
    // The list of active formatting elements: the `<b>`, `<i>`, `<a>`... that are open or that were closed by mistake and must open again.
    open: OpenStack,
    // The stack of open elements. The last one is the current node. JS: an array with `push` and `pop`.
    mode: Mode,
    text_return: Mode,
    // The mode to go back to after the text of a `<title>` or `<script>`.
    head: Option<NodeId>,
    // The `<head>` element, so that we can put an element in it later.
    form: Option<NodeId>,
    // The open `<form>`. A form cannot be inside a form, so a second one is ignored.
    skip_newline: bool,
    // True just after `<pre>` and `<textarea>`: a "\n" right after the tag is ignored.
    foster: bool,
    // True while we move content out of a table. See `place`.
    raw: Option<&'static str>,
    // Set when we made an HTML element with raw text or RCDATA: `drive` tells the tokenizer, and clears it.
    templates: Vec<NodeId>,
    // The `template` elements, in tree order. `done` moves the children of each into its contents.
    scripting: bool,
    // The scripting flag. True for a page (this browser runs scripts): `noscript` has raw text. A fragment for a context in an inert document has it false.
    tmpl: Vec<Tm>,
    // The stack of template insertion modes: one entry for each open `template`, and one at the bottom for the context of a fragment that is a `template`.
    context: Option<NodeId>,
    select_fragment: bool,
    // The context element of a fragment that is in the SVG or MathML namespace (a copy with no parent). See `adjusted`.
}

impl Builder {
    fn new() -> Builder {
        Builder {
            doc: Document::new(),
            open: OpenStack::default(),
            afe: Vec::new(),
            mode: Mode::Initial,
            text_return: Mode::InBody,
            head: None,
            form: None,
            skip_newline: false,
            foster: false,
            raw: None,
            templates: Vec::new(),
            scripting: true,
            tmpl: Vec::new(),
            context: None,
            select_fragment: false,
        }
    }

    /// The input is over: close the tree and give it.
    fn done(mut self) -> Document {
        self.finish();
        self.fill_templates();
        self.doc.shrink_to_fit();
        // A `Vec` that grows keeps spare room (up to twice as much). The page is read, so give the room back.
        self.doc
    }

    /// Move the children of each `template` into its contents.
    fn fill_templates(&mut self) {
        if self.templates.is_empty() {
            return;
        }
        for t in std::mem::take(&mut self.templates) {
            if self.doc.children(t).next().is_some() {
                let contents = self.doc.make_template_content(t);
                self.doc.move_children(t, contents);
            }
        }
    }

    // ---- Small helpers ----

    /// The current node: the top of the stack. Before the first element it is the document.
    fn current(&self) -> NodeId {
        self.open.last().copied().unwrap_or(self.doc.root())
    }

    /// The adjusted current node: the context element of a fragment while only the root of the fragment is open, else the current node.
    /// Only a foreign context is kept (`context`), as the HTML rules read an HTML context through the insertion mode.
    fn adjusted(&self) -> NodeId {
        match self.context {
            Some(c) if self.open.len() == 1 => c,
            _ => self.current(),
        }
    }

    /// Is this node an element with one of these names?
    #[inline(always)]
    fn is(&self, id: NodeId, names: &[&str]) -> bool {
        matches!(self.doc.data(id), NodeData::Element { name, .. } if names.contains(&self.doc.atom_str(*name))) && !self.doc.is_foreign(id)
        // `matches!` with an `if` checks the pattern and then the condition. It gives true or false.
        // An SVG or MathML element is never one of the HTML elements that the rules know by name.
    }

    /// Is this a "special" element of the standard? An HTML element of the list, or an SVG or MathML integration point.
    fn is_special(&self, id: NodeId) -> bool {
        self.is(id, SPECIAL) || (self.doc.has_foreign() && self.foreign_scope_border(id))
    }

    /// The name of an element, or an empty text for other nodes.
    fn name(&self, id: NodeId) -> String {
        match self.doc.data(id) {
            NodeData::Element { name, .. } => self.doc.atom_str(*name).to_string(),
            _ => String::new(),
        }
    }

    /// Where does the next node go? Gives the parent, and the node to put it in front of (or `None` for "at the end").
    /// Normally that is the end of the current node. While we "foster parent", content that cannot be inside
    /// a table goes in front of the table instead.
    fn place(&self) -> (NodeId, Option<NodeId>) {
        let current = self.current();
        if self.foster && self.is(current, TABLE_PARTS) {
            return self.foster_target();
        }
        (current, None)
    }

    /// The place for a node that is moved out of a table part. `rposition` finds the last table or template on the stack: the one we are in.
    /// A template that is nearer than the table keeps the node. With neither (a fragment whose context is a template), the node goes in the root of the fragment.
    #[cold]
    #[inline(never)]
    fn foster_target(&self) -> (NodeId, Option<NodeId>) {
        let names: &[&str] = if self.tmpl.is_empty() { &["table"] } else { &["table", "template"] };
        let Some(i) = self.open.iter().rposition(|&n| self.is(n, names)) else { return (self.open[0], None) };
        let table = self.open[i];
        if !self.tmpl.is_empty() && self.is(table, &["template"]) {
            return (table, None);
        }
        match self.doc.parent(table) {
            Some(parent) => (parent, Some(table)),
            None => (self.open[i - 1], None),
        }
    }

    /// Add an element. `push` says if it also goes on the stack. Void elements (`br`, `img`) do not.
    fn insert_element(&mut self, name: &str, attrs: &[Attr<'_>], push: bool) -> NodeId {
        let (parent, before) = self.place();
        let id = self.doc.insert_element(parent, before, name, attrs);
        if push {
            let is_p = self.is(id, &["p"]);
            self.open.push(id, is_p);
            if name == "template" {
                self.templates.push(id);
            }
        }
        id
    }

    fn insert_text(&mut self, text: &str) {
        let (parent, before) = self.place();
        self.doc.insert_text(parent, before, text);
    }

    /// Add a comment to the current node.
    fn insert_comment(&mut self, text: &str) {
        let current = self.current();
        self.doc.append_comment_to(current, text);
    }

    /// Is an element with one of `names` on the stack, with no scope border in between?
    /// `extra` are more borders. The search goes from the top of the stack down.
    fn in_scope(&self, names: &[&str], extra: &[&str]) -> bool {
        for &node in self.open.iter().rev() {
            if self.is(node, names) {
                return true;
            }
            if self.is(node, SCOPE) || self.is(node, extra) || (self.doc.has_foreign() && self.foreign_scope_border(node)) {
                return false;
            }
        }
        false
    }

    /// The same, for tables. Only `table` and `html` are borders.
    fn in_table_scope(&self, name: &str) -> bool {
        for &node in self.open.iter().rev() {
            if self.is(node, &[name]) {
                return true;
            }
            if self.is(node, &["table", "html", "template"]) {
                return false;
            }
        }
        false
    }

    /// Pop elements until one with one of these names is popped. The caller knows that there is one.
    fn pop_until(&mut self, names: &[&str]) {
        if let Some(i) = self.open.iter().rposition(|&n| self.is(n, names)) {
            self.open.truncate(i);
            // `truncate(i)` keeps the first `i` items. It drops that element and all above it.
        }
    }

    /// Close the elements that may have a missing end tag, like `li` and `p`. `except` is not closed.
    fn close_implied(&mut self, except: &str) {
        while self.is(self.current(), IMPLIED_END) && !self.is(self.current(), &[except]) {
            self.open.pop();
        }
    }

    /// Close an open `<p>`, if one is in button scope.
    fn close_p(&mut self) {
        if self.open.p_count > 0 && self.in_scope(&["p"], &["button"]) {
            self.close_implied("p");
            self.pop_until(&["p"]);
        }
    }

    // ---- The list of active formatting elements, and the adoption agency algorithm ----

    /// Drop the markers of the cells (and `applet`, `marquee`, `object`) that have closed, with everything after them.
    /// The stack counts the elements that left (`OpenStack::markers_left`). We do the work only when the list is used.
    fn sync_afe(&mut self) {
        while self.open.markers_left > 0 {
            self.open.markers_left -= 1;
            while let Some(item) = self.afe.pop() {
                if item == Afe::Marker {
                    break;
                }
            }
        }
    }

    /// The place in the list of the last element named `name` after the last marker.
    fn afe_find(&self, name: &str) -> Option<usize> {
        for (i, item) in self.afe.iter().enumerate().rev() {
            match *item {
                Afe::Marker => return None,
                Afe::El(id) if self.is(id, &[name]) => return Some(i),
                Afe::El(_) => {}
            }
        }
        None
    }

    /// Put a formatting element that we just added to the stack at the end of the list. If the list already has 3 elements
    /// like it (same name, same attributes) after the last marker, the oldest of the 3 goes. (The standard calls this "Noah's Ark":
    /// 2 of each kind. It keeps a page with 10,000 `<b>` from making a list of 10,000.)
    fn push_afe(&mut self, id: NodeId) {
        self.sync_afe();
        let mut same = Vec::new();
        for (i, item) in self.afe.iter().enumerate().rev().take(NOAH_SCAN) {
            match *item {
                Afe::Marker => break,
                Afe::El(other) if self.same_element(id, other) => same.push(i),
                Afe::El(_) => {}
            }
        }
        if same.len() >= 3 {
            self.afe.remove(same[same.len() - 1]);
        }
        self.afe.push(Afe::El(id));
    }

    /// Same name and same attributes (in any order)?
    fn same_element(&self, a: NodeId, b: NodeId) -> bool {
        let (NodeData::Element { name: na, attrs: aa }, NodeData::Element { name: nb, attrs: ab }) = (self.doc.data(a), self.doc.data(b)) else {
            return false;
        };
        let (aa, ab) = (self.doc.attrs(*aa), self.doc.attrs(*ab));
        na == nb
            && aa.len() == ab.len()
            && aa.iter().all(|x| ab.iter().any(|y| x.name == y.name && self.doc.text(x.value) == self.doc.text(y.value)))
    }

    /// Open again the formatting elements that were closed by a mistake, when something new goes in the page. In
    /// `<p><b>x</p>y` the `</p>` closes the `<b>`, and the `y` goes in a new `<b>`. Each one is a copy with the same attributes.
    #[inline]
    fn reconstruct_afe(&mut self) {
        if self.afe.is_empty() {
            // The list is empty, so there is nothing to open (and a marker that is left over has nothing to drop).
            self.open.markers_left = 0;
            return;
        }
        self.sync_afe();
        let Some(&last) = self.afe.last() else { return };
        let open = |this: &Builder, item: Afe| match item {
            Afe::Marker => true,
            Afe::El(id) => this.open.contains(id),
        };
        if open(self, last) {
            return;
        }
        // Go back to the first element that must be opened again: the one after the last that is open (or a marker).
        let mut first = self.afe.len() - 1;
        while first > 0 && !open(self, self.afe[first - 1]) {
            first -= 1;
        }
        for i in first..self.afe.len() {
            let Afe::El(old) = self.afe[i] else { continue };
            let copy = self.doc.clone_element(old);
            let (parent, before) = self.place();
            self.doc.move_to(copy, parent, before);
            self.open.push(copy, false);
            self.open.track(copy);
            self.afe[i] = Afe::El(copy);
        }
    }

    /// Add an element that has a marker in the list: a table cell, `applet`, `marquee` or `object`.
    fn insert_marked(&mut self, name: &str, attrs: &[Attr<'_>]) {
        // The markers of cells that closed go first. If not, we would drop the new marker with them.
        self.sync_afe();
        self.insert_element(name, attrs, true);
        self.open.mark_top();
        self.afe.push(Afe::Marker);
    }

    /// Is `node` on the stack, with no scope border above it?
    fn node_in_scope(&self, node: NodeId) -> bool {
        for &n in self.open.iter().rev() {
            if n == node {
                return true;
            }
            if self.is(n, SCOPE) || (self.doc.has_foreign() && self.foreign_scope_border(n)) {
                return false;
            }
        }
        false
    }

    /// The end tag of a formatting element, like `</b>`. The standard calls this the "adoption agency algorithm": a block
    /// like a `<p>` that is inside the `<b>` is "adopted" by a copy of the `<b>`. `<b>1<p>2</b>3</p>` gives
    /// `<b>1</b><p><b>2</b>3</p>`. Gives `false` when the algorithm says to treat the tag as a normal end tag.
    fn adoption_agency(&mut self, subject: &str) -> bool {
        self.sync_afe();
        let current = self.current();
        if self.is(current, &[subject]) && !self.afe.contains(&Afe::El(current)) {
            self.open.pop();
            return true;
        }
        // The algorithm repeats at most 8 times, so that a bad page cannot make it run for long.
        for _ in 0..8 {
            let Some(mut fe_afe) = self.afe_find(subject) else { return false };
            let Afe::El(fe) = self.afe[fe_afe] else { return false };
            let Some(fe_open) = self.open.position_tracked(fe) else {
                // The element is not open: nothing to fix. It leaves the list.
                self.afe.remove(fe_afe);
                return true;
            };
            if !self.node_in_scope(fe) {
                return true;
            }
            // The furthest block: the first special element (a `<p>`, a `<div>`...) that was opened after the formatting element.
            let Some(block_open) = (fe_open + 1..self.open.len()).find(|&i| self.is_special(self.open[i])) else {
                // No block inside: the element just closes, with all that is inside it.
                self.open.truncate(fe_open);
                self.afe.remove(fe_afe);
                return true;
            };
            let block = self.open[block_open];
            let common = self.open[fe_open - 1];
            // Where the new copy of the element goes in the list. A number in the list, that we keep right when we remove items.
            let mut bookmark = fe_afe;
            let remove_afe = |afe: &mut Vec<Afe>, i: usize, bookmark: &mut usize| {
                afe.remove(i);
                if i < *bookmark {
                    *bookmark -= 1;
                }
            };
            let (mut node_open, mut last) = (block_open, block);
            let mut count = 0;
            loop {
                count += 1;
                node_open -= 1;
                let node = self.open[node_open];
                if node == fe {
                    break;
                }
                let mut in_afe = self.afe.iter().position(|&a| a == Afe::El(node));
                if count > 3
                    && let Some(i) = in_afe
                {
                    remove_afe(&mut self.afe, i, &mut bookmark);
                    in_afe = None;
                }
                let Some(i) = in_afe else {
                    // An element that is not in the list is just taken off the stack.
                    self.open.remove(node_open);
                    continue;
                };
                // The element is copied. The copy takes its place in the list and in the stack, and takes the node that we carry.
                let copy = self.doc.clone_element(node);
                self.afe[i] = Afe::El(copy);
                self.open.replace(node_open, copy);
                if last == block {
                    bookmark = i + 1;
                }
                self.doc.move_to(last, copy, None);
                last = copy;
            }
            // The node that we carry goes to the common ancestor. In a table, it goes before the table (foster parenting).
            let (parent, before) = if self.is(common, TABLE_PARTS) { self.foster_place() } else { (common, None) };
            self.doc.move_to(last, parent, before);
            // A copy of the formatting element takes all the children of the block, and goes in the block.
            let copy = self.doc.clone_element(fe);
            self.doc.move_children(block, copy);
            self.doc.move_to(copy, block, None);
            // The old element leaves the list. The copy takes the place of the bookmark. And in the stack: the copy goes right after the block.
            fe_afe = self.afe.iter().position(|&a| a == Afe::El(fe)).unwrap_or(fe_afe);
            remove_afe(&mut self.afe, fe_afe, &mut bookmark);
            self.afe.insert(bookmark.min(self.afe.len()), Afe::El(copy));
            if let Some(i) = self.open.position_tracked(fe) {
                self.open.remove(i);
            }
            let block_open = self.open.position(block).map_or(self.open.len(), |i| i + 1);
            self.open.insert(block_open, copy, 0);
            self.open.track(copy);
        }
        true
    }

    /// Where a node goes when its parent would be a table part: before the table, or inside the element above the table
    /// when the table has no parent. A template that is nearer than the table keeps the node. With neither, it goes in the root.
    fn foster_place(&self) -> (NodeId, Option<NodeId>) {
        let names: &[&str] = if self.tmpl.is_empty() { &["table"] } else { &["table", "template"] };
        match self.open.iter().rposition(|&n| self.is(n, names)) {
            Some(i) if !self.tmpl.is_empty() && self.is(self.open[i], &["template"]) => (self.open[i], None),
            Some(i) => match self.doc.parent(self.open[i]) {
                Some(parent) => (parent, Some(self.open[i])),
                None => (self.open[i.saturating_sub(1)], None),
            },
            None => (self.open[0], None),
        }
    }

    // ---- The main loop ----

    fn process(&mut self, token: Token) {
        let token = if std::mem::take(&mut self.skip_newline) {
            // `take` reads the flag and sets it back to false in one step.
            match token {
                Token::Text(text) if text.starts_with('\n') => {
                    if text.len() == 1 {
                        return;
                    }
                    Token::Text(match text {
                        Cow::Borrowed(text) => Cow::Borrowed(&text[1..]),
                        Cow::Owned(text) => Cow::Owned(text[1..].to_string()),
                    })
                    // The browser drops the first newline after `<pre>`, so that you can start the text on the next line.
                }
                other => other,
            }
        } else {
            token
        };

        if self.doc.has_foreign() && self.is_foreign_token(&token) {
            self.foreign(token);
        } else {
            self.process_html(token);
        }
    }

    /// Read a token by the rules of the insertion mode (the HTML rules).
    fn process_html(&mut self, token: Token) {
        match self.mode {
            Mode::Initial => self.initial(token),
            Mode::BeforeHtml => self.before_html(token),
            Mode::BeforeHead => self.before_head(token),
            Mode::InHead => self.in_head(token),
            Mode::AfterHead => self.after_head(token),
            Mode::InBody => self.in_body(token),
            Mode::Text => self.in_text(token),
            Mode::AfterBody => self.after_body(token),
            Mode::AfterAfterBody => self.after_after_body(token),
            Mode::InTemplate | Mode::InTemplateColGroup => self.in_template_modes(token),
            Mode::InSelect => self.in_select(token),
        }
    }

    /// The "in select" mode of the standard (13.2.6.4.16, classic rules), for a fragment whose context is a `select`. Rules follow html5ever
    /// (`tree_builder/rules.rs`, MIT OR Apache-2.0) and Ladybird (`HTMLParser::handle_in_select`, BSD-2-Clause). Only text, `option`, `optgroup`, `hr`,
    /// comments, `script` and `template` make nodes. No `select` is open in a fragment, so `select`, `input`, `keygen` and `textarea` are ignored.
    fn in_select(&mut self, token: Token) {
        match token {
            Token::Text(text) => {
                let text = text.replace('\0', "");
                if !text.is_empty() {
                    self.insert_text(&text);
                }
            }
            Token::Comment(text) => self.insert_comment(&text),
            Token::Doctype(_) => {}
            Token::StartTag { name, attrs, .. } => match &*name {
                "html" => self.in_body(Token::StartTag { name, attrs, self_closing: false }),
                "option" | "optgroup" | "hr" => {
                    if self.is(self.current(), &["option"]) {
                        self.open.pop();
                    }
                    if name != "option" && self.is(self.current(), &["optgroup"]) {
                        self.open.pop();
                    }
                    self.insert_element(&name, &attrs, name != "hr");
                }
                "script" | "template" => {
                    // The raw text mode returns to this mode; the end of a template does too (`select_fragment`).
                    self.head_tag(&name, &attrs);
                }
                _ => {}
            },
            Token::EndTag { name } => match &*name {
                "optgroup" => {
                    let cur = self.current();
                    if self.is(cur, &["option"]) && self.open.len() >= 2 && self.is(self.open[self.open.len() - 2], &["optgroup"]) {
                        self.open.pop();
                    }
                    if self.is(self.current(), &["optgroup"]) {
                        self.open.pop();
                    }
                }
                "option" => {
                    if self.is(self.current(), &["option"]) {
                        self.open.pop();
                    }
                }
                "template" => self.template_end(),
                _ => {}
            },
        }
    }

    /// The input is over. A page that stops early still gets its `html`, `head` and `body`.
    fn finish(&mut self) {
        if self.mode == Mode::Text {
            self.open.pop();
            self.mode = self.text_return;
        }
        // End of input in "in template": close each open template, then read the end again in the mode that is left.
        while !self.tmpl.is_empty() && self.open.iter().any(|&n| self.is(n, &["template"])) {
            self.template_end();
        }
        if matches!(self.mode, Mode::Initial | Mode::BeforeHtml | Mode::BeforeHead | Mode::InHead | Mode::AfterHead) {
            self.process(Token::StartTag { name: "body".into(), attrs: vec![], self_closing: false });
            // A `<body>` start tag makes every missing part, in order.
        }
    }

    // ---- The modes before the body ----

    fn initial(&mut self, token: Token) {
        match token {
            Token::Doctype(name) => {
                self.doc.append_doctype(&name);
                self.mode = Mode::BeforeHtml;
            }
            Token::Comment(text) => self.doc.append_comment(&text),
            Token::Text(text) if blank(&text) => {}
            // Spaces before the doctype do not count.
            token => {
                self.mode = Mode::BeforeHtml;
                self.process(token);
                // "Reprocess": the same token, now in the next mode.
            }
        }
    }

    fn before_html(&mut self, token: Token) {
        match token {
            Token::Doctype(_) => {}
            Token::Comment(text) => self.doc.append_comment(&text),
            Token::Text(text) if blank(&text) => {}
            Token::StartTag { name, attrs, .. } if name == "html" => {
                self.insert_element("html", &attrs, true);
                self.mode = Mode::BeforeHead;
            }
            Token::EndTag { name } if !matches!(&*name, "head" | "body" | "html" | "br") => {}
            // Most end tags are ignored here.
            token => {
                self.insert_element("html", &[], true);
                // No `<html>` tag in the page: we make one.
                self.mode = Mode::BeforeHead;
                self.process(token);
            }
        }
    }

    fn before_head(&mut self, token: Token) {
        match token {
            Token::Doctype(_) => {}
            Token::Comment(text) => self.insert_comment(&text),
            Token::Text(text) if blank(&text) => {}
            Token::StartTag { name, .. } if name == "html" => {}
            Token::StartTag { name, attrs, .. } if name == "head" => {
                self.head = Some(self.insert_element("head", &attrs, true));
                self.mode = Mode::InHead;
            }
            Token::EndTag { name } if !matches!(&*name, "head" | "body" | "html" | "br") => {}
            token => {
                self.head = Some(self.insert_element("head", &[], true));
                self.mode = Mode::InHead;
                self.process(token);
            }
        }
    }

    /// Start tags that belong in the head: `title`, `meta`, `script`... Gives true if this was one.
    /// Void elements are added. Raw text elements are added, and we go to `Text` mode.
    fn head_tag(&mut self, name: &str, attrs: &[Attr<'_>]) -> bool {
        match name {
            "base" | "basefont" | "bgsound" | "link" | "meta" => {
                self.insert_element(name, attrs, false);
            }
            "title" | "style" | "script" | "noframes" => self.raw_text_element(name, attrs),
            "noscript" if self.scripting => self.raw_text_element(name, attrs),
            "template" => self.template_start(attrs),
            _ => return false,
        }
        true
    }

    /// `<template>` in the head rules (also used by "in body", "in table", "after head"). The element and a marker go in, and the template has its own insertion mode.
    #[cold]
    #[inline(never)]
    fn template_start(&mut self, attrs: &[Attr<'_>]) {
        self.insert_marked("template", attrs);
        self.tmpl.push(Tm::Template);
        self.mode = Mode::InTemplate;
    }

    /// `</template>`. Nothing happens when no template is open. The markers of the list of active formatting elements go with the elements that own them,
    /// but the standard clears the list only up to the last marker, so a cell that was still open leaves the marker of the template behind.
    #[cold]
    #[inline(never)]
    fn template_end(&mut self) {
        if !self.open.iter().any(|&n| self.is(n, &["template"])) {
            return;
        }
        self.sync_afe();
        self.pop_until(&["template"]);
        self.open.markers_left = self.open.markers_left.min(1);
        self.tmpl.pop();
        self.reset_mode();
    }

    /// The standard's "reset the insertion mode appropriately", for the modes that we have: the first open element from the top that names a mode gives it.
    /// The table, cell and select modes show in the stack, so for them the mode is "in body".
    #[cold]
    #[inline(never)]
    fn reset_mode(&mut self) {
        for &n in self.open.iter().rev() {
            if self.is(n, &["template"]) {
                self.mode = mode_for(*self.tmpl.last().unwrap_or(&Tm::Body));
            } else if self.is(n, &["head"]) {
                self.mode = Mode::InHead;
            } else if self.is(n, &["html"]) {
                // The root of a page has a head by now. The root of a fragment has none: it takes the mode of a template context.
                self.mode = if self.head.is_some() {
                    Mode::AfterHead
                } else if let Some(&t) = self.tmpl.last() {
                    mode_for(t)
                } else if self.select_fragment {
                    Mode::InSelect
                } else {
                    Mode::InBody
                };
            } else if self.is(n, &["select", "td", "th", "tr", "tbody", "thead", "tfoot", "caption", "colgroup", "table", "body"]) {
                self.mode = Mode::InBody;
            } else {
                continue;
            }
            return;
        }
    }

    #[cold]
    #[inline(never)]
    fn in_template_modes(&mut self, token: Token) {
        if self.mode == Mode::InTemplate {
            self.in_template(token);
        } else {
            self.in_template_colgroup(token);
        }
    }

    /// The mode of "in template": the first tag in a template says what the template holds. Its template insertion mode is then that table part (or the body).
    fn in_template(&mut self, token: Token) {
        let tm = match &token {
            Token::Text(_) | Token::Comment(_) | Token::Doctype(_) => return self.in_body(token),
            Token::EndTag { name } => {
                if name == "template" {
                    self.template_end();
                }
                return;
            }
            Token::StartTag { name, .. } => match &**name {
                // The standard also lists `base`, `basefont`, `bgsound`, `noframes` and `title` here. Chrome 155 does not keep "in template" for them:
                // a `<td>` after `<template><title>t</title>` is dropped, as after any other content tag. We do as Chrome does.
                "link" | "meta" | "script" | "style" | "template" => return self.in_body(token),
                "caption" | "colgroup" | "tbody" | "tfoot" | "thead" => Tm::Table,
                "col" => Tm::ColGroup,
                "tr" => Tm::TableBody,
                "td" | "th" => Tm::Row,
                _ => Tm::Body,
            },
        };
        if let Some(top) = self.tmpl.last_mut() {
            *top = tm;
        }
        self.mode = mode_for(tm);
        self.process(token);
    }

    /// "In column group", for a template: spaces, comments, `col` and the template tags. Anything else is ignored, because the current node is not a `colgroup`.
    fn in_template_colgroup(&mut self, token: Token) {
        match token {
            Token::Text(text) => {
                let (spaces, _) = split_spaces(&text);
                if !spaces.is_empty() {
                    self.insert_text(spaces);
                }
            }
            Token::Comment(text) => self.insert_comment(&text),
            Token::StartTag { name, attrs, .. } if name == "col" => {
                self.insert_element("col", &attrs, false);
            }
            Token::StartTag { ref name, .. } if name == "template" => self.in_body(token),
            Token::EndTag { name } if name == "template" => self.template_end(),
            _ => {}
        }
    }

    /// A `table` or `form` start tag when the current node is a template in a table mode. A template has no table to end, so `table` is ignored.
    /// The form rule of the table modes: the standard ignores the tag when a template is open. Chrome 155 adds an empty `form`
    /// (and does not set the form element pointer, as a form in a template never does). We do as Chrome does.
    #[cold]
    #[inline(never)]
    fn template_table_tag(&mut self, token: Token) {
        if let Token::StartTag { name, attrs, .. } = token
            && name == "form"
        {
            self.insert_element("form", &attrs, false);
        }
    }

    /// The template insertion mode, when the current node is a template (or the root of a fragment whose context is a template).
    fn tpl_level(&self) -> Option<Tm> {
        let top = *self.tmpl.last()?;
        (self.open.len() == 1 || self.is(self.current(), &["template"])).then_some(top)
    }

    /// Do the table rules apply to a table part tag? They do when the nearest table or template is a table, or a template in a table mode.
    /// A template that holds other content (or a page with no table) ignores the tag.
    fn table_rules(&self) -> bool {
        if self.tmpl.is_empty() {
            return self.open.iter().any(|&n| self.is(n, &["table"]));
        }
        let tpl = |this: &Builder| matches!(this.tmpl.last(), Some(Tm::Table | Tm::TableBody | Tm::Row));
        for &n in self.open.iter().rev() {
            if self.is(n, &["table", "template"]) {
                return self.is(n, &["table"]) || tpl(self);
            }
        }
        tpl(self)
    }

    /// Add an element whose content is raw text. Go to `Text` mode until its end tag.
    fn raw_text_element(&mut self, name: &str, attrs: &[Attr<'_>]) {
        self.raw = raw_text_kind(name).map(|kind| kind.1);
        self.insert_element(name, attrs, true);
        self.text_return = self.mode;
        self.mode = Mode::Text;
    }

    fn in_head(&mut self, token: Token) {
        match token {
            Token::Text(text) => {
                let (spaces, rest) = split_spaces(&text);
                if !spaces.is_empty() {
                    self.insert_text(spaces);
                }
                if !rest.is_empty() {
                    self.leave_head(Token::Text(Cow::Borrowed(rest)));
                }
            }
            Token::Comment(text) => self.insert_comment(&text),
            Token::Doctype(_) => {}
            Token::StartTag { name, .. } if name == "html" || name == "head" => {}
            Token::StartTag { name, attrs, self_closing } => {
                if !self.head_tag(&name, &attrs) {
                    self.leave_head(Token::StartTag { name, attrs, self_closing });
                }
            }
            Token::EndTag { name } if name == "head" => {
                self.open.pop();
                self.mode = Mode::AfterHead;
            }
            Token::EndTag { name } if !matches!(&*name, "body" | "html" | "br") => {}
            token => self.leave_head(token),
        }
    }

    /// This token cannot be in the head, so the head ends. Then we read the token again.
    fn leave_head(&mut self, token: Token) {
        self.open.pop();
        self.mode = Mode::AfterHead;
        self.process(token);
    }

    fn after_head(&mut self, token: Token) {
        match token {
            Token::Text(text) => {
                let (spaces, rest) = split_spaces(&text);
                if !spaces.is_empty() {
                    self.insert_text(spaces);
                }
                if !rest.is_empty() {
                    self.start_body(Token::Text(Cow::Borrowed(rest)));
                }
            }
            Token::Comment(text) => self.insert_comment(&text),
            Token::Doctype(_) => {}
            Token::StartTag { name, .. } if name == "html" || name == "head" => {}
            Token::StartTag { name, attrs, .. } if name == "body" => {
                self.insert_element("body", &attrs, true);
                self.mode = Mode::InBody;
            }
            Token::StartTag { name, attrs, self_closing } => {
                if matches!(&*name, "base" | "basefont" | "bgsound" | "link" | "meta" | "title" | "style" | "script" | "noframes" | "template") {
                    // A head element after the head has ended. It still goes into the head.
                    let head = self.head.expect("the head exists in this mode");
                    self.open.push(head, false);
                    self.head_tag(&name, &attrs);
                    // Put the head on the stack for a moment, so the element goes into it.
                    if let Some(i) = self.open.iter().rposition(|&n| n == head) {
                        self.open.remove(i);
                    }
                } else {
                    self.start_body(Token::StartTag { name, attrs, self_closing });
                }
            }
            Token::EndTag { name } if !matches!(&*name, "body" | "html" | "br") => {}
            token => self.start_body(token),
        }
    }

    /// No `<body>` tag in the page: we make one, and read the token again.
    fn start_body(&mut self, token: Token) {
        self.insert_element("body", &[], true);
        self.mode = Mode::InBody;
        self.process(token);
    }

    // ---- Text mode ----

    fn in_text(&mut self, token: Token) {
        match token {
            Token::Text(text) => self.insert_text(&text),
            Token::EndTag { .. } => {
                self.open.pop();
                self.mode = self.text_return;
            }
            _ => {}
        }
    }

    // ---- The body ----

    fn in_body(&mut self, token: Token) {
        let current = self.current();
        let in_table_part = self.is(current, TABLE_PARTS);
        match token {
            Token::StartTag { name, attrs, .. } if is_table_structure(&name) => {
                if self.table_rules() {
                    self.table_start(&name, &attrs);
                }
                // Without a table (or a template in a table mode), these tags are ignored.
            }
            Token::EndTag { name } if is_table_structure(&name) || name == "table" => self.table_end(&name),
            Token::StartTag { ref name, .. } if !self.tmpl.is_empty() && matches!(&**name, "table" | "form") && self.tpl_level().is_some_and(|t| matches!(t, Tm::Table | Tm::TableBody | Tm::Row)) => {
                self.template_table_tag(token);
            }
            Token::StartTag { name, attrs, self_closing } if name == "table" && in_table_part => {
                // A table in a table part ends the table that we are in. Then the new one starts.
                // In a template with no table the parts close, and the tag is then ignored (see above).
                match self.open.iter().rposition(|&n| self.is(n, &["table", "template"])) {
                    Some(i) if self.is(self.open[i], &["table"]) => self.open.truncate(i),
                    _ => {
                        while self.is(self.current(), TABLE_PARTS) {
                            self.open.pop();
                        }
                    }
                }
                self.process(Token::StartTag { name, attrs, self_closing });
            }
            Token::StartTag { ref name, .. } if in_table_part && name == "form" => {}
            Token::StartTag { name, attrs, .. } if in_table_part && name == "input" && is_hidden(&attrs) => {
                self.insert_element("input", &attrs, false);
            }
            Token::StartTag { ref name, .. } if self.is(current, &["colgroup"]) && name != "template" => {
                self.open.pop();
                // Only `col` can be in a colgroup. Anything else ends it. (`col` was handled above.)
                self.process(token);
            }
            Token::Text(ref text) if self.is(current, &["colgroup"]) && !blank(text) => {
                self.open.pop();
                self.process(token);
            }
            token => {
                let fosters = in_table_part
                    && match &token {
                        Token::Text(text) => !blank(text),
                        // Spaces between table parts are fine. Other text moves out of the table.
                        Token::StartTag { name, .. } => !matches!(&**name, "style" | "script" | "template"),
                        _ => false,
                    };
                self.foster = fosters;
                self.body_rules(token);
                self.foster = false;
            }
        }
    }

    /// The rules of "in body" for all tokens that are not about tables.
    fn body_rules(&mut self, token: Token) {
        match token {
            Token::Text(text) => {
                // Spaces between the parts of a table do not open the formatting elements again.
                if !self.afe.is_empty() && (self.foster || !self.is(self.current(), TABLE_PARTS)) {
                    self.reconstruct_afe();
                }
                self.insert_text(&text);
            }
            Token::Comment(text) => self.insert_comment(&text),
            Token::Doctype(_) => {}
            Token::StartTag { name, attrs, self_closing } => self.body_start(&name, attrs, self_closing),
            Token::EndTag { name } => self.body_end(&name),
        }
    }

    fn body_start(&mut self, name: &str, attrs: Vec<Attr<'_>>, self_closing: bool) {
        match name {
            "svg" | "math" => {
                self.reconstruct_afe();
                self.insert_foreign(if name == "svg" { Ns::Svg } else { Ns::MathMl }, name, &attrs, self_closing);
            }
            "html" | "body" | "head" | "frame" => {}
            // A second `<html>` or `<body>` is ignored. The standard merges their attributes. We do not yet. A `<frame>` outside a frameset is ignored.
            "base" | "basefont" | "bgsound" | "link" | "meta" | "title" | "style" | "script" | "noframes" | "template" => {
                self.head_tag(name, &attrs);
            }
            _ if CLOSES_P.contains(&name) => {
                self.close_p();
                self.insert_element(name, &attrs, true);
            }
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                self.close_p();
                if self.is(self.current(), HEADINGS) {
                    self.open.pop();
                    // `<h1>a<h2>b`: a heading cannot be in a heading. The first one ends.
                }
                self.insert_element(name, &attrs, true);
            }
            "pre" | "listing" => {
                self.close_p();
                self.insert_element(name, &attrs, true);
                self.skip_newline = true;
            }
            "form" => {
                // Inside a template the form element pointer is neither read nor set.
                let in_template = !self.tmpl.is_empty() && self.open.iter().any(|&n| self.is(n, &["template"]));
                if in_template || self.form.is_none() {
                    self.close_p();
                    let f = self.insert_element(name, &attrs, true);
                    if !in_template {
                        self.form = Some(f);
                    }
                }
            }
            "li" => {
                self.close_list_item(&["li"]);
                self.insert_element(name, &attrs, true);
            }
            "dd" | "dt" => {
                self.close_list_item(&["dd", "dt"]);
                self.insert_element(name, &attrs, true);
            }
            "button" => {
                if self.in_scope(&["button"], &[]) {
                    self.close_implied("");
                    self.pop_until(&["button"]);
                }
                self.reconstruct_afe();
                self.insert_element(name, &attrs, true);
            }
            "a" => {
                // An `<a>` inside an `<a>` closes the first one. The standard runs the adoption agency algorithm for it.
                self.sync_afe();
                if let Some(i) = self.afe_find("a")
                    && let Afe::El(old) = self.afe[i]
                {
                    self.adoption_agency("a");
                    // The old `<a>` may be still there (the algorithm may stop early). It must go.
                    if let Some(i) = self.afe.iter().position(|&a| a == Afe::El(old)) {
                        self.afe.remove(i);
                    }
                    if let Some(i) = self.open.position_tracked(old) {
                        self.open.remove(i);
                    }
                }
                self.reconstruct_afe();
                let id = self.insert_element(name, &attrs, true);
                self.open.track(id);
                self.push_afe(id);
            }
            "nobr" => {
                self.reconstruct_afe();
                if self.in_scope(&["nobr"], &[]) {
                    self.adoption_agency("nobr");
                    self.reconstruct_afe();
                }
                let id = self.insert_element(name, &attrs, true);
                self.open.track(id);
                self.push_afe(id);
            }
            _ if is_formatting(name) => {
                self.reconstruct_afe();
                let id = self.insert_element(name, &attrs, true);
                self.open.track(id);
                self.push_afe(id);
            }
            "applet" | "marquee" | "object" => {
                self.reconstruct_afe();
                self.insert_marked(name, &attrs);
            }
            "table" => {
                self.close_p();
                self.insert_element(name, &attrs, true);
            }
            "area" | "br" | "embed" | "img" | "keygen" | "wbr" | "input" | "param" | "source" | "track" => {
                if !matches!(name, "param" | "source" | "track") {
                    self.reconstruct_afe();
                }
                self.insert_element(name, &attrs, false);
                // Void elements have no end tag and no children.
            }
            "image" => {
                self.reconstruct_afe();
                self.insert_element("img", &attrs, false);
                // Old pages write `<image>`. Browsers read it as `<img>`.
            }
            "hr" => {
                self.close_p();
                self.insert_element(name, &attrs, false);
            }
            "textarea" => {
                self.skip_newline = true;
                self.raw_text_element(name, &attrs);
            }
            "xmp" => {
                self.close_p();
                self.reconstruct_afe();
                self.raw_text_element(name, &attrs);
            }
            "iframe" | "noembed" => self.raw_text_element(name, &attrs),
            "noscript" if self.scripting => self.raw_text_element(name, &attrs),
            "plaintext" => {
                self.close_p();
                self.reconstruct_afe();
                self.insert_element(name, &attrs, true);
                self.raw = Some("plaintext");
            }
            "option" => {
                if self.is(self.current(), &["option"]) {
                    self.open.pop();
                }
                self.reconstruct_afe();
                self.insert_element(name, &attrs, true);
            }
            "optgroup" => {
                if self.is(self.current(), &["option"]) {
                    self.open.pop();
                }
                if self.is(self.current(), &["optgroup"]) {
                    self.open.pop();
                }
                self.reconstruct_afe();
                self.insert_element(name, &attrs, true);
            }
            _ => {
                self.reconstruct_afe();
                self.insert_element(name, &attrs, true);
                // Any other element, even one that we do not know: a normal element.
            }
        }
    }

    /// A new `<li>` (or `<dd>`, `<dt>`) closes the one that is open, but not an outer one.
    /// We look down the stack for an open one. A special element stops the search,
    /// except `div`, `p` and `address`. That is why `<ul><li>a<ul><li>b` has nested lists.
    fn close_list_item(&mut self, names: &[&str]) {
        for i in (0..self.open.len()).rev() {
            let node = self.open[i];
            if self.is(node, names) {
                let name = self.name(node);
                self.close_implied(&name);
                self.open.truncate(i);
                break;
            }
            if self.is_special(node) && !self.is(node, &["address", "div", "p"]) {
                break;
            }
        }
        self.close_p();
    }

    fn body_end(&mut self, name: &str) {
        match name {
            "body" => {
                if self.in_scope(&["body"], &[]) {
                    self.mode = Mode::AfterBody;
                    // The body is not closed. Text after `</body>` still goes in it.
                }
            }
            "html" => {
                if self.in_scope(&["body"], &[]) {
                    self.mode = Mode::AfterBody;
                    self.process(Token::EndTag { name: "html".into() });
                }
            }
            "form" => {
                // With a template open the form element pointer is not used: the end tag closes a form that is in scope.
                let has_form = (!self.tmpl.is_empty() && self.open.iter().any(|&n| self.is(n, &["template"]))) || self.form.take().is_some();
                if has_form && self.in_scope(&["form"], &[]) {
                    self.close_implied("");
                    self.pop_until(&["form"]);
                }
            }
            "p" => {
                if self.open.p_count == 0 || !self.in_scope(&["p"], &["button"]) {
                    self.insert_element("p", &[], true);
                    // `</p>` with no open `<p>` makes an empty one. Browsers do this.
                }
                self.close_p();
            }
            "li" => {
                if self.in_scope(&["li"], &["ol", "ul"]) {
                    self.close_implied("li");
                    self.pop_until(&["li"]);
                }
            }
            "dd" | "dt" => {
                if self.in_scope(&[name], &[]) {
                    self.close_implied(name);
                    self.pop_until(&[name]);
                }
            }
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                if self.in_scope(HEADINGS, &[]) {
                    self.close_implied("");
                    self.pop_until(HEADINGS);
                }
            }
            "br" => self.body_start("br", vec![], false),
            // `</br>` is read as `<br>`.
            _ if CLOSES_P.contains(&name) || matches!(name, "button" | "applet" | "marquee" | "object" | "pre" | "listing") => {
                if self.in_scope(&[name], &[]) {
                    self.close_implied("");
                    self.pop_until(&[name]);
                }
            }
            _ if is_formatting(name) => {
                if !self.adoption_agency(name) {
                    self.any_other_end(name);
                }
            }
            // `template` is here, not an arm of its own: one more name in the `match` made every end tag slower (+0.8% of `bare-bench parse`).
            _ if name == "template" => self.template_end(),
            _ => self.any_other_end(name),
        }
    }

    /// Any other end tag: close the nearest open element with this name.
    /// Stop at a special element: `</span>` cannot close the `div` that holds the span.
    fn any_other_end(&mut self, name: &str) {
        for i in (0..self.open.len()).rev() {
            let node = self.open[i];
            if self.is(node, &[name]) {
                self.close_implied(name);
                self.open.truncate(i);
                return;
            }
            if self.is_special(node) {
                return;
            }
        }
    }

    // ---- Tables ----

    /// A start tag of a table part: `caption`, `col`, `colgroup`, `tbody`, `thead`, `tfoot`, `tr`, `td`, `th`.
    /// First we close what is open inside the table, down to the part that can hold the new one.
    /// Then we add the parts that the page left out: a `tr` in a `table` gets a `tbody`.
    fn table_start(&mut self, name: &str, attrs: &[Attr<'_>]) {
        loop {
            let current = self.name(self.current());
            let mut current = current.as_str();
            // A template (or the root of a fragment of a template) acts as the table part that its insertion mode names. It is never closed.
            let mut border = false;
            if matches!(current, "template" | "html") {
                border = true;
                current = match self.tmpl.last() {
                    Some(Tm::Table) => "table",
                    Some(Tm::TableBody) => "tbody",
                    Some(Tm::Row) => "tr",
                    _ => return,
                };
            }
            match (current, name) {
                ("table", "caption" | "colgroup" | "tbody" | "thead" | "tfoot") => {
                    if name == "caption" {
                        self.insert_marked(name, attrs);
                    } else {
                        self.insert_element(name, attrs, true);
                    }
                    return;
                }
                ("table", "col") => {
                    self.insert_element("colgroup", &[], true);
                }
                ("table", _) => {
                    self.insert_element("tbody", &[], true);
                    // A `tr` or `td` straight in a table: browsers add the `tbody` that the page forgot.
                }
                ("colgroup", "col") => {
                    self.insert_element(name, attrs, false);
                    return;
                }
                ("tbody" | "thead" | "tfoot", "tr") => {
                    self.insert_element(name, attrs, true);
                    return;
                }
                ("tbody" | "thead" | "tfoot", "td" | "th") => {
                    self.insert_element("tr", &[], true);
                }
                ("tr", "td" | "th") => {
                    self.insert_marked(name, attrs);
                    return;
                }
                _ if border => return,
                // The tag does not fit in this template mode (a `caption` in a row): it is ignored.
                _ => {
                    self.open.pop();
                    // The current node is in the way: an open cell, row, caption, or content inside one. Close it.
                }
            }
        }
    }

    /// An end tag of a table part. It closes that part, and everything inside it.
    fn table_end(&mut self, name: &str) {
        if name == "colgroup" {
            if self.is(self.current(), &["colgroup"]) {
                self.open.pop();
            }
        } else if self.in_table_scope(name) {
            self.pop_until(&[name]);
        }
        // `</td>` when no cell is open is ignored.
    }

    // ---- After the body ----

    fn after_body(&mut self, token: Token) {
        match token {
            Token::Text(text) => {
                let (spaces, rest) = split_spaces(&text);
                if !spaces.is_empty() {
                    self.in_body(Token::Text(Cow::Borrowed(spaces)));
                    // Spaces after `</body>` are still added to the body.
                }
                if !rest.is_empty() {
                    self.mode = Mode::InBody;
                    self.process(Token::Text(Cow::Borrowed(rest)));
                }
            }
            Token::Comment(text) => {
                let html = self.open[0];
                self.doc.append_comment_to(html, &text);
                // A comment after `</body>` goes in the `html` element, after the body.
            }
            Token::Doctype(_) => {}
            Token::StartTag { name, .. } if name == "html" => {}
            Token::EndTag { name } if name == "html" => self.mode = Mode::AfterAfterBody,
            token => {
                self.mode = Mode::InBody;
                self.process(token);
            }
        }
    }

    fn after_after_body(&mut self, token: Token) {
        match token {
            Token::Text(text) => {
                let (spaces, rest) = split_spaces(&text);
                if !spaces.is_empty() {
                    self.in_body(Token::Text(Cow::Borrowed(spaces)));
                }
                if !rest.is_empty() {
                    self.mode = Mode::InBody;
                    self.process(Token::Text(Cow::Borrowed(rest)));
                }
            }
            Token::Comment(text) => self.doc.append_comment(&text),
            Token::Doctype(_) => {}
            Token::StartTag { name, .. } if name == "html" => {}
            token => {
                self.mode = Mode::InBody;
                self.process(token);
            }
        }
    }
}

/// Is this a tag of a part of a table (not the table itself)?
fn is_table_structure(name: &str) -> bool {
    matches!(name, "caption" | "col" | "colgroup" | "tbody" | "td" | "tfoot" | "th" | "thead" | "tr")
}

/// `<input type=hidden>`. The type is not case sensitive.
fn is_hidden(attrs: &[Attr<'_>]) -> bool {
    attrs.iter().any(|(k, v)| k == "type" && v.eq_ignore_ascii_case("hidden"))
}

/// The five spaces of HTML: space, tab, newline, form feed and carriage return.
const SPACES: [char; 5] = [' ', '\t', '\n', '\x0C', '\r'];

fn blank(text: &str) -> bool {
    text.trim_matches(SPACES).is_empty()
}

/// Cut a text after its leading spaces: `"  ab"` gives `("  ", "ab")`.
fn split_spaces(text: &str) -> (&str, &str) {
    text.split_at(text.len() - text.trim_start_matches(SPACES).len())
    // `trim_start_matches` removes the spaces in front. The rest is shorter by the number of spaces.
}

#[cfg(test)]
// Only compiled for `cargo test`.
mod tests {
    use super::*;

    /// The tree for a page, as text lines. Each line starts with "| ", like `Document::dump`.
    fn tree(page: &str) -> String {
        parse(page).dump()
    }

    /// The text that `dump` gives for these lines. Each line holds its own indent.
    fn expect(lines: &[&str]) -> String {
        lines.iter().map(|line| format!("| {line}\n")).collect()
    }

    /// The tree of a page, with only what is inside `<body>`.
    fn body(page: &str) -> String {
        let full = tree(&format!("<body>{page}"));
        full.strip_prefix(&expect(&["<html>", "  <head>", "  <body>"])).expect("a body after an empty head").to_string()
        // Cuts off the lines for `html`, `head` and `body`. Then the tests only show what they are about.
    }

    /// Lines inside the body get 4 spaces of indent.
    fn in_body(lines: &[&str]) -> String {
        lines.iter().map(|line| format!("|     {line}\n")).collect()
    }

    #[test]
    fn adds_the_parts_that_are_missing() {
        assert_eq!(tree(""), expect(&["<html>", "  <head>", "  <body>"]));
        assert_eq!(
            tree("<title>Hi</title><p>x"),
            expect(&["<html>", "  <head>", "    <title>", "      \"Hi\"", "  <body>", "    <p>", "      \"x\""])
        );
        assert_eq!(tree("x"), expect(&["<html>", "  <head>", "  <body>", "    \"x\""]));
    }

    #[test]
    fn keeps_the_parts_that_are_there() {
        assert_eq!(
            tree("<!DOCTYPE html><html lang=en><head></head><body class=a>x</body></html>"),
            expect(&["<!DOCTYPE html>", "<html>", "  lang=\"en\"", "  <head>", "  <body>", "    class=\"a\"", "    \"x\""])
        );
    }

    #[test]
    fn spaces_between_the_parts() {
        let page = "<!DOCTYPE html>\n<html>\n<head>\n</head>\n<body>\n</body></html>";
        assert_eq!(
            tree(page),
            expect(&["<!DOCTYPE html>", "<html>", "  <head>", "    \"\n\"", "  \"\n\"", "  <body>", "    \"\n\""])
        );
        // The browser drops spaces before `<head>`, but keeps them inside the head and between the head and the body.
    }

    #[test]
    fn text_in_the_head_starts_the_body() {
        assert_eq!(tree("<head>x"), expect(&["<html>", "  <head>", "  <body>", "    \"x\""]));
    }

    #[test]
    fn head_elements_after_the_head_go_into_it() {
        assert_eq!(
            tree("<head></head><meta charset=x><body>"),
            expect(&["<html>", "  <head>", "    <meta>", "      charset=\"x\"", "  <body>"])
        );
    }

    #[test]
    fn comments_go_where_they_were_written() {
        let page = "<!--a--><html><!--b--><head><!--c--></head><!--d--><body><!--e--></body><!--f--></html><!--g-->";
        assert_eq!(
            tree(page),
            expect(&[
                "<!-- a -->",
                "<html>",
                "  <!-- b -->",
                "  <head>",
                "    <!-- c -->",
                "  <!-- d -->",
                "  <body>",
                "    <!-- e -->",
                "  <!-- f -->",
                "<!-- g -->",
            ])
        );
    }

    #[test]
    fn text_after_the_body_goes_into_the_body() {
        assert_eq!(body("a</body>b"), in_body(&["\"ab\""]));
        assert_eq!(body("a</body></html>b"), in_body(&["\"ab\""]));
    }

    #[test]
    fn a_new_paragraph_closes_the_open_one() {
        assert_eq!(body("<p>a<p>b"), in_body(&["<p>", "  \"a\"", "<p>", "  \"b\""]));
        assert_eq!(body("<p>a<div>b</div>"), in_body(&["<p>", "  \"a\"", "<div>", "  \"b\""]));
        assert_eq!(body("<p>a<table>"), in_body(&["<p>", "  \"a\"", "<table>"]));
    }

    #[test]
    fn list_items_close_each_other() {
        assert_eq!(body("<ul><li>a<li>b</ul>"), in_body(&["<ul>", "  <li>", "    \"a\"", "  <li>", "    \"b\""]));
        assert_eq!(
            body("<ul><li>a<ul><li>b</ul><li>c</ul>"),
            in_body(&["<ul>", "  <li>", "    \"a\"", "    <ul>", "      <li>", "        \"b\"", "  <li>", "    \"c\""])
        );
        // The inner `li` does not close the outer one: a `ul` is in between.
        assert_eq!(body("<dl><dt>a<dd>b<dt>c</dl>"), in_body(&["<dl>", "  <dt>", "    \"a\"", "  <dd>", "    \"b\"", "  <dt>", "    \"c\""]));
    }

    #[test]
    fn headings_do_not_nest() {
        assert_eq!(body("<h1>a<h2>b"), in_body(&["<h1>", "  \"a\"", "<h2>", "  \"b\""]));
    }

    #[test]
    fn void_elements_have_no_children() {
        assert_eq!(
            body("<p>a<br>b<img src=x>c<hr>d"),
            in_body(&["<p>", "  \"a\"", "  <br>", "  \"b\"", "  <img>", "    src=\"x\"", "  \"c\"", "<hr>", "\"d\""])
        );
        assert_eq!(body("<image src=x>"), in_body(&["<img>", "  src=\"x\""]));
    }

    #[test]
    fn stray_end_tags() {
        assert_eq!(body("</div></span>x"), in_body(&["\"x\""]));
        assert_eq!(body("a</p>b"), in_body(&["\"a\"", "<p>", "\"b\""]));
        // `</p>` with no `<p>` makes an empty paragraph.
        assert_eq!(body("a</br>b"), in_body(&["\"a\"", "<br>", "\"b\""]));
        // `</br>` is a `<br>`.
        assert_eq!(body("<div><span>a</div>b"), in_body(&["<div>", "  <span>", "    \"a\"", "\"b\""]));
        // `</div>` closes the span too.
        assert_eq!(body("<div><p>a</span>b</div>"), in_body(&["<div>", "  <p>", "    \"ab\""]));
        // `</span>` does nothing: it cannot close past the `p`.
        assert_eq!(body("<b>x</i>y</b>"), in_body(&["<b>", "  \"xy\""]));
    }

    #[test]
    fn nested_links_and_buttons_and_forms() {
        assert_eq!(body("<a>1<a>2"), in_body(&["<a>", "  \"1\"", "<a>", "  \"2\""]));
        assert_eq!(body("<button>a<button>b"), in_body(&["<button>", "  \"a\"", "<button>", "  \"b\""]));
        assert_eq!(body("<form><form>x</form>"), in_body(&["<form>", "  \"x\""]));
        // A form cannot be in a form. The second tag is ignored.
    }

    #[test]
    fn options_close_each_other() {
        assert_eq!(
            body("<select><option>a<option>b</select>x"),
            in_body(&["<select>", "  <option>", "    \"a\"", "  <option>", "    \"b\"", "\"x\""])
        );
    }

    #[test]
    fn the_first_newline_in_pre_and_textarea_is_dropped() {
        assert_eq!(body("<pre>\nx</pre>"), in_body(&["<pre>", "  \"x\""]));
        assert_eq!(body("<pre>\n\nx</pre>"), in_body(&["<pre>", "  \"\nx\""]));
        assert_eq!(body("<textarea>\nx</textarea>"), in_body(&["<textarea>", "  \"x\""]));
    }

    #[test]
    fn raw_text() {
        assert_eq!(
            tree("<script>a<b></script><p>"),
            expect(&["<html>", "  <head>", "    <script>", "      \"a<b>\"", "  <body>", "    <p>"])
        );
        assert_eq!(tree("<title>a &amp; b</title>"), expect(&["<html>", "  <head>", "    <title>", "      \"a & b\"", "  <body>"]));
        assert_eq!(body("<style>p{}</style>x"), in_body(&["<style>", "  \"p{}\"", "\"x\""]));
        assert_eq!(body("<xmp><b></xmp>"), in_body(&["<xmp>", "  \"<b>\""]));
    }

    #[test]
    fn text_is_not_lost_when_the_page_ends_inside_a_script() {
        assert_eq!(tree("<script>a"), expect(&["<html>", "  <head>", "    <script>", "      \"a\"", "  <body>"]));
    }

    #[test]
    fn second_html_and_body_tags_are_ignored() {
        assert_eq!(
            tree("<body class=a><html id=x><body id=b>y"),
            expect(&["<html>", "  <head>", "  <body>", "    class=\"a\"", "    \"y\""])
        );
    }

    #[test]
    fn adds_the_missing_table_parts() {
        assert_eq!(
            body("<table><tr><td>a<td>b<tr><td>c</table>"),
            in_body(&[
                "<table>",
                "  <tbody>",
                "    <tr>",
                "      <td>",
                "        \"a\"",
                "      <td>",
                "        \"b\"",
                "    <tr>",
                "      <td>",
                "        \"c\"",
            ])
        );
        assert_eq!(
            body("<table><thead><tr><th>h<tbody><tr><td>d</table>"),
            in_body(&["<table>", "  <thead>", "    <tr>", "      <th>", "        \"h\"", "  <tbody>", "    <tr>", "      <td>", "        \"d\""])
        );
    }

    #[test]
    fn captions_and_column_groups() {
        assert_eq!(
            body("<table><caption>c<tr><td>x</table>"),
            in_body(&["<table>", "  <caption>", "    \"c\"", "  <tbody>", "    <tr>", "      <td>", "        \"x\""])
        );
        assert_eq!(
            body("<table><col><tr><td>x</table>"),
            in_body(&["<table>", "  <colgroup>", "    <col>", "  <tbody>", "    <tr>", "      <td>", "        \"x\""])
        );
    }

    #[test]
    fn a_cell_can_hold_a_table() {
        assert_eq!(
            body("<table><tr><td><table><tr><td>x</table>y</table>z"),
            in_body(&[
                "<table>",
                "  <tbody>",
                "    <tr>",
                "      <td>",
                "        <table>",
                "          <tbody>",
                "            <tr>",
                "              <td>",
                "                \"x\"",
                "        \"y\"",
                "\"z\"",
            ])
        );
    }

    #[test]
    fn content_in_a_cell_does_not_leave_the_cell() {
        assert_eq!(
            body("<table><tr><td><div><p>a<td>b</table>"),
            in_body(&["<table>", "  <tbody>", "    <tr>", "      <td>", "        <div>", "          <p>", "            \"a\"", "      <td>", "        \"b\""])
        );
        // The new `td` closes the old cell, with the div and the p inside.
    }

    #[test]
    fn text_in_a_table_moves_before_it() {
        assert_eq!(
            body("<table>x<tr><td>y</table>"),
            in_body(&["\"x\"", "<table>", "  <tbody>", "    <tr>", "      <td>", "        \"y\""])
        );
        // This is foster parenting.
        assert_eq!(body("<table><div>a</div><tr><td>b</table>"), in_body(&["<div>", "  \"a\"", "<table>", "  <tbody>", "    <tr>", "      <td>", "        \"b\""]));
        assert_eq!(body("<table> <tr> <td>b</table>"), in_body(&["<table>", "  \" \"", "  <tbody>", "    <tr>", "      \" \"", "      <td>", "        \"b\""]));
        // Spaces are fine in a table: they stay.
        assert_eq!(body("<table><input type=HIDDEN><input></table>"), in_body(&["<input>", "<table>", "  <input>", "    type=\"HIDDEN\""]));
        // A hidden input may be in a table. Any other input moves before it.
    }

    #[test]
    fn a_table_in_a_table_part_closes_the_first() {
        assert_eq!(body("<table><tr><td>a<table>"), in_body(&["<table>", "  <tbody>", "    <tr>", "      <td>", "        \"a\"", "        <table>"]));
        // In a cell, a table is nested. In a table part, it is not:
        assert_eq!(body("<table><table>"), in_body(&["<table>", "<table>"]));
    }

    #[test]
    fn table_parts_outside_a_table_are_ignored() {
        assert_eq!(body("<tr><td>a</td></tr>"), in_body(&["\"a\""]));
    }

    #[test]
    fn end_tags_close_table_parts() {
        assert_eq!(
            body("<table><tr><td>a</td></tr></table>b"),
            in_body(&["<table>", "  <tbody>", "    <tr>", "      <td>", "        \"a\"", "\"b\""])
        );
        assert_eq!(body("<table><tr><td>a</tr>b</table>"), in_body(&["\"b\"", "<table>", "  <tbody>", "    <tr>", "      <td>", "        \"a\""]));
        // After `</tr>` we are in the table body. "b" cannot go there, so it moves before the table.
    }

    #[test]
    fn never_panics_or_loops_on_junk() {
        let pieces = [
            "<", ">", "/", "<table>", "<tr>", "<td>", "<th>", "<tbody>", "<caption>", "<colgroup>", "<col>", "</table>", "</td>", "</tr>", "</tbody>", "<p>",
            "</p>", "<div>", "</div>", "<li>", "<ul>", "</ul>", "<a>", "<b>", "</b>", "<h1>", "<form>", "</form>", "<select>", "<option>", "<script>", "</script>",
            "<title>", "</title>", "<body>", "</body>", "<html>", "</html>", "<head>", "</head>", "<br>", "</br>", "<pre>", "<textarea>", "<!--", "-->", "x", " ", "\n",
            "</a>", "<i>", "</i>", "<nobr>", "</nobr>", "<object>", "</object>", "<marquee>", "<button>", "<em>", "</em>", "<b class=x>", "<template>", "</template>",
            "<col>", "<colgroup>", "</colgroup>", "<thead>", "<tfoot>", "</caption>", "<svg>", "</svg>", "<frame>",
        ];
        let mut seed: u32 = 99;
        for _ in 0..4000 {
            let mut page = String::new();
            for _ in 0..(seed % 25) {
                seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                page.push_str(pieces[(seed >> 16) as usize % pieces.len()]);
            }
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            let doc = parse(&page);
            check_links(&doc, doc.root());
            // Every tree must hold together: each child knows its parent.
            let text = doc.dump();
            assert!(text.starts_with("| <html>") || text.starts_with("| <!") || text.starts_with("| <!--"), "bad tree for {page:?}:\n{text}");
        }
    }

    fn check_links(doc: &Document, id: NodeId) {
        for child in doc.children(id) {
            assert_eq!(doc.parent(child), Some(id));
            check_links(doc, child);
        }
    }

    #[test]
    fn open_p_count_stays_right() {
        // A `p` that is far down the stack still closes when a `div` starts, after other elements were closed with it.
        let dump = parse("<p><b><i>x<div>y</div>z").dump();
        assert_eq!(dump, parse("<p><b><i>x</i></b></p><div><b><i>y</i></b></div><b><i>z</i></b>").dump());
        // `</p>` with no open `p` makes an empty one, also after the count went up and down.
        let dump = parse("<p>a</p></p><div>b</div></p>").dump();
        assert_eq!(dump.matches("<p>").count(), 3, "{dump}");
        // The count goes down when a table closes the `p` inside it, and when `truncate` drops many elements.
        let dump = parse("<div><p><span><p>x</span></div></p>y").dump();
        assert_eq!(dump.matches("<p>").count(), 3, "{dump}");
    }

    #[test]
    fn deep_nesting_is_linear() {
        // 50,000 nested divs, then a stray `</p>` for each. With the old stack walk this took about 4 seconds.
        let html = format!("{}x{}", "<div>".repeat(50_000), "</p></div>".repeat(50_000));
        let start = std::time::Instant::now();
        let doc = parse(&html);
        assert!(start.elapsed().as_secs() < 2, "{:?}", start.elapsed());
        assert!(doc.len() > 100_000);
    }

    /// The body of a page as text, the way `innerHTML` gives it in a browser: `<b class="x">1</b>`.
    fn inner_html(page: &str) -> String {
        fn write(doc: &Document, id: NodeId, out: &mut String) {
            for child in doc.children(id) {
                match doc.data(child) {
                    NodeData::Text(span) => out.push_str(doc.text(*span)),
                    NodeData::Element { name, attrs } => {
                        let name = doc.atom_str(*name);
                        out.push_str(&format!("<{name}"));
                        for a in doc.attrs(*attrs) {
                            out.push_str(&format!(" {}=\"{}\"", doc.atom_str(a.name), doc.text(a.value)));
                        }
                        out.push('>');
                        write(doc, child, out);
                        out.push_str(&format!("</{name}>"));
                    }
                    _ => {}
                }
            }
        }
        let doc = parse(&format!("<body>{page}"));
        let body = doc.children(doc.children(doc.root()).next().unwrap()).nth(1).expect("a body");
        let mut out = String::new();
        write(&doc, body, &mut out);
        out
    }

    #[test]
    fn adoption_agency_matches_chrome() {
        // Each pair is a page and what Chrome gives for `body.innerHTML` (read with `DOMParser`).
        let cases = [
            ("<b>1<p>2</b>3</p>", "<b>1</b><p><b>2</b>3</p>"),
            ("<a><p></a></p>", "<a></a><p><a></a></p>"),
            ("<p><b>1</p>2", "<p><b>1</b></p><b>2</b>"),
            ("<i><b>1</i>2</b>", "<i><b>1</b></i><b>2</b>"),
            ("<a href=1>x<a href=2>y</a>z", "<a href=\"1\">x</a><a href=\"2\">y</a>z"),
            ("<p><b><i>x<div>y</div>z", "<p><b><i>x</i></b></p><div><b><i>y</i></b></div><b><i>z</i></b>"),
            ("<table><tr><td><b>x</td><td>y</table>", "<table><tbody><tr><td><b>x</b></td><td>y</td></tr></tbody></table>"),
            ("<b class=x><b class=x><b class=x><b class=x>1</p>2", "<b class=\"x\"><b class=\"x\"><b class=\"x\"><b class=\"x\">1<p></p>2</b></b></b></b>"),
            ("<p><b class=x><b class=x><b class=x><b class=x>1</p>2", "<p><b class=\"x\"><b class=\"x\"><b class=\"x\"><b class=\"x\">1</b></b></b></b></p><b class=\"x\"><b class=\"x\"><b class=\"x\">2</b></b></b>"),
            ("<b><p>1<b>2</b>3</b>4</p>", "<b></b><p><b>1<b>2</b>3</b>4</p>"),
            ("<a><b><div></a>x</b>", "<a><b></b></a><b></b><div><b><a></a>x</b></div>"),
            ("<b><i><s><u><p>1</b>2</i>3</s>4</u>5", "<b><i><s><u></u></s></i></b><i><s><u></u></s></i><s><u></u></s><u></u><p><u><s><i><b>1</b>2</i>3</s>4</u>5</p>"),
            ("<nobr>1<nobr>2</nobr>3", "<nobr>1</nobr><nobr>2</nobr>3"),
            ("<b>1<table><tr><td>2</b>3</table>4", "<b>1<table><tbody><tr><td>23</td></tr></tbody></table>4</b>"),
            ("<table><b><tr><td>x</b>y</table>", "<b></b><table><tbody><tr><td>xy</td></tr></tbody></table>"),
            ("<b><table><i><tr><td></b>", "<b><i></i><table><tbody><tr><td></td></tr></tbody></table></b>"),
            ("<em>1<div>2</em>3</div>4", "<em>1</em><div><em>2</em>3</div>4"),
            ("<font color=red>1<ul><li>2</font>3</ul>4", "<font color=\"red\">1</font><ul><font color=\"red\"></font><li><font color=\"red\">2</font>3</li></ul>4"),
            ("<b><div><b>1</b></div></b>2", "<b><div><b>1</b></div></b>2"),
            ("<b>1<div>2<div>3</b>4</div>5</div>6", "<b>1</b><div><b>2</b><div><b>3</b>4</div>5</div>6"),
            ("<a href=x><div>1<a href=y>2</a>3</div></a>", "<a href=\"x\"></a><div><a href=\"x\">1</a><a href=\"y\">2</a>3</div>"),
            ("<b><object>x</b>y</object>z", "<b><object>xy</object>z</b>"),
            ("<b>1<marquee>2</b>3</marquee>4", "<b>1<marquee>23</marquee>4</b>"),
            ("<p><u>1</p><p>2", "<p><u>1</u></p><p><u>2</u></p>"),
            ("<code>1<pre>2</code>3</pre>", "<code>1</code><pre><code>2</code>3</pre>"),
            ("<b><button>1</b>2</button>3", "<b></b><button><b>1</b>2</button>3"),
            ("<strong>1<h1>2</strong>3</h1>", "<strong>1</strong><h1><strong>2</strong>3</h1>"),
            ("<small><p>1</small>2<p>3", "<small></small><p><small>1</small>2</p><p>3</p>"),
        ];
        for (page, want) in cases {
            assert_eq!(inner_html(page), want, "{page}");
        }
    }

    #[test]
    fn formatting_junk_keeps_the_tree_whole() {
        // Many pages of formatting tags, blocks and tables in a bad order. The tree must always hold together, and no text may be lost.
        let pieces = [
            "<b>", "</b>", "<i>", "</i>", "<a href=1>", "<a>", "</a>", "<nobr>", "</nobr>", "<em class=x>", "</em>", "<p>", "</p>", "<div>", "</div>",
            "<table>", "</table>", "<tr>", "<td>", "</td>", "<li>", "<ul>", "</ul>", "<object>", "</object>", "<button>", "</button>", "<h1>", "</h1>", "Q",
        ];
        let mut seed: u32 = 7;
        for _ in 0..20_000 {
            let mut page = String::new();
            let mut texts = 0;
            for _ in 0..(seed % 40) {
                seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                let piece = pieces[(seed >> 16) as usize % pieces.len()];
                texts += usize::from(piece == "Q");
                page.push_str(piece);
            }
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            let doc = parse(&page);
            check_links(&doc, doc.root());
            let dump = doc.dump();
            // Texts that are next to each other join, so we count the letters. No tag has a capital letter in it.
            assert_eq!(dump.matches('Q').count(), texts, "{page:?}\n{dump}");
        }
    }

    #[test]
    fn misnested_formatting_is_fast() {
        // The adoption agency algorithm must not make a page slow. A `<b>` for each of 20,000 deep blocks, and then 20,000 `</b>`.
        let pages = [
            format!("{}x{}", "<b><div>".repeat(20_000), "</b>".repeat(20_000)),
            format!("{}x", "<b id=a><i id=b><div>".repeat(5_000)),
            format!("{}x{}", "<b>".repeat(20_000), "</p>x".repeat(20_000)),
            format!("{}", "<p><b>x</p>".repeat(20_000)),
            format!("{}x", "<table><tr><td><b>".repeat(2_000)),
        ];
        for page in pages {
            let start = std::time::Instant::now();
            let doc = parse(&page);
            assert!(start.elapsed().as_secs() < 3, "{:?}", start.elapsed());
            assert!(doc.len() > 1);
        }
    }

    /// The tree of a page that arrives in the given pieces.
    fn streamed(pieces: &[&str]) -> String {
        let mut parser = Parser::new();
        for piece in pieces {
            parser.feed(piece);
        }
        parser.finish().dump()
    }

    const PAGES: &[&str] = &[
        "<!doctype html><html><head><title>t &amp; u</title><style>p>b{}</style></head><body><p class=a id='b' c=\"d\">x<b>y<i>z</b>w</i></p></body></html>",
        "<script>if (a < b && c) { x = '</scr' + 'ipt>'; }</script><p>after</SCRIPT ><textarea>&lt;<b></textarea>",
        "&amp;&lt;&#65;&#x41;&notin;&nbsp&copy;&notit;&#0;&#x110000;&amp<p a='&amp;&lt' b=&copy=1 c=&copyx>",
        "<!-- c --><!--> <!---> <!-- a --!> <!x y><?php ?><![CDATA[x]]><!DOCTYPE html PUBLIC 'x'>t",
        "<table><tr><td>a<td>b<tr><th>c</table><b>x<table><td><i>y</table><br/><p/>a < b <3 </> </ x>",
        "<p>caf\u{e9} \u{1F600} \u{4e2d}\u{6587}<pre>\n\nx</pre><plaintext></plaintext><b>",
        "<div a=1 a=2 b",
        "text only, no tags & no end &#12",
        "<p>a</p></ti",
    ];

    #[test]
    fn a_page_in_pieces_gives_the_same_tree() {
        for page in PAGES {
            let whole = parse(page).dump();
            let cuts: Vec<usize> = (0..=page.len()).filter(|&i| page.is_char_boundary(i)).collect();
            for &i in &cuts {
                assert_eq!(streamed(&[&page[..i], &page[i..]]), whole, "cut at {i} in {page:?}");
            }
            // Cut it in three, at every pair of places. The pages are short, so this is fast.
            for (n, &i) in cuts.iter().enumerate() {
                for &j in &cuts[n..] {
                    assert_eq!(streamed(&[&page[..i], &page[i..j], &page[j..]]), whole, "cuts at {i} and {j} in {page:?}");
                }
            }
            // One letter at a time.
            let letters: Vec<String> = page.chars().map(String::from).collect();
            let letters: Vec<&str> = letters.iter().map(String::as_str).collect();
            assert_eq!(streamed(&letters), whole, "one letter at a time: {page:?}");
        }
    }

    #[test]
    fn a_long_cut_token_is_not_read_again_for_each_piece() {
        // 20,000 pieces inside one script. A parser that reads again from the start of the script
        // each time does 20,000 x 100 KB of work. Ours waits, so it is linear.
        let mut parser = Parser::new();
        parser.feed("<script>");
        let start = std::time::Instant::now();
        for _ in 0..20_000 {
            parser.feed("xxxxx");
        }
        parser.feed("</script><p>done");
        let doc = parser.finish();
        assert!(doc.dump().contains("done"));
        assert!(start.elapsed().as_secs() < 2);
    }

    #[test]
    fn noscript_has_raw_text_in_the_head_and_the_body() {
        // The scripting flag is on (headless Chrome 155 gives the same trees).
        let doc = parse("<!doctype html><html><head><noscript><link rel=stylesheet href=x><b>q</b></noscript><title>t</title></head><body><noscript><p>a</p></noscript><p>after");
        let dump = doc.dump();
        assert!(dump.contains("<noscript>\n|       \"<link rel=stylesheet href=x><b>q</b>\"\n|     <title>"), "{dump}");
        assert!(dump.contains("<noscript>\n|       \"<p>a</p>\"\n|     <p>\n|       \"after\""), "{dump}");
        // an unclosed one runs to the end of the input
        let doc = parse("<body><noscript><p>a<div>b");
        assert!(doc.dump().contains("\"<p>a<div>b\""));
    }

    #[test]
    fn a_form_in_a_template_is_not_read_by_the_form_pointer() {
        let doc = parse("<body><form id=one><template><form id=in></form></template></form><form id=two></form>");
        let html = doc.children(doc.root()).find(|&c| doc.element_name(c) == "html").unwrap();
        let body = doc.children(html).last().unwrap();
        let form = doc.children(body).next().unwrap();
        // The form in the template is kept, and it does not take the pointer. So the end tag after the template closes the first form,
        // and the second form is its sibling (Chrome 155: `form#one(1),form#two(0)`, case `fr-form-pointer` in `tplmodes-cases.txt`).
        assert_eq!(doc.children(body).count(), 2);
        let tpl = doc.children(form).next().unwrap();
        let contents = doc.template_content(tpl).expect("contents");
        assert_eq!(doc.children(contents).map(|c| doc.element_name(c)).collect::<Vec<_>>(), ["form"]);
    }

    /// The tree of a page body, with the contents of each template under a `content` line (the html5lib format).
    fn tpl(page: &str) -> String {
        let full = parse(&format!("<body>{page}")).dump_with_contents();
        full.strip_prefix(&expect(&["<html>", "  <head>", "  <body>"])).expect("a body after an empty head").to_string()
    }

    #[test]
    fn a_template_keeps_table_parts_that_have_no_table() {
        let row = in_body(&["<template>", "  content", "    <tr>", "      <td>", "        \"a\""]);
        assert_eq!(tpl("<template><tr><td>a</td></tr></template>"), row);
        assert_eq!(tpl("<template><tr><td>a"), row);
        // the first tag says what the template holds: a `td` start gets no `tr`, and the next `tr` is ignored because no row is open
        assert_eq!(tpl("<template><td>a</td><tr><td>b"), in_body(&["<template>", "  content", "    <td>", "      \"a\"", "    <td>", "      \"b\""]));
        // after `caption` the template is in table mode: a `tr` gets a `tbody`, and a `table` is ignored
        assert_eq!(
            tpl("<template><caption>c</caption><table><tr><td>x"),
            in_body(&["<template>", "  content", "    <caption>", "      \"c\"", "    <tbody>", "      <tr>", "        <td>", "          \"x\""])
        );
        // after other content a `tr` is ignored, and a table is a table
        assert_eq!(tpl("<template><div><tr><td>x"), in_body(&["<template>", "  content", "    <div>", "      \"x\""]));
        assert_eq!(tpl("<template><table><tr><td>x"), in_body(&["<template>", "  content", "    <table>", "      <tbody>", "        <tr>", "          <td>", "            \"x\""]));
    }

    #[test]
    fn a_template_in_a_table_is_not_part_of_the_table() {
        // the `tr` goes into the template, and the text in the row goes to the end of the template, not before the table
        assert_eq!(
            tpl("<table><template><tr>x<td>y</td></tr></template></table>"),
            in_body(&["<table>", "  <template>", "    content", "      <tr>", "        <td>", "          \"y\"", "      \"x\""])
        );
        // a `td` in a cell of the table reaches the table rules of the template, not the table that holds it
        assert_eq!(
            tpl("<table><tr><td><template><tr><td>q</td></tr></template>"),
            in_body(&["<table>", "  <tbody>", "    <tr>", "      <td>", "        <template>", "          content", "            <tr>", "              <td>", "                \"q\""])
        );
    }

    #[test]
    fn a_column_group_in_a_template_takes_only_columns() {
        assert_eq!(tpl("<template><col><div>x</div><col>y"), in_body(&["<template>", "  content", "    <col>", "    <col>"]));
        assert_eq!(tpl("<template><col> <!--c--></template>"), in_body(&["<template>", "  content", "    <col>", "    \" \"", "    <!-- c -->"]));
    }

    #[test]
    fn the_end_of_the_input_closes_the_templates() {
        // only a template in the head: the body is still made, and the template stays in the head
        assert_eq!(parse("<template>x").dump_with_contents(), expect(&["<html>", "  <head>", "    <template>", "      content", "        \"x\"", "  <body>"]));
        assert_eq!(parse("<template><template><td>a").dump_with_contents(), parse("<template><template><td>a</td></template></template>").dump_with_contents());
        // after the head, a template still goes into the head
        assert_eq!(
            parse("<head></head><template>x</template><p>").dump_with_contents(),
            expect(&["<html>", "  <head>", "    <template>", "      content", "        \"x\"", "  <body>", "    <p>"])
        );
    }

    #[test]
    fn a_template_has_its_own_marker_in_the_list_of_formatting_elements() {
        // the `i` of the template is not opened again after it, and the `b` outside is
        assert_eq!(
            tpl("<b>1<template><i>2</template>3"),
            in_body(&["<b>", "  \"1\"", "  <template>", "    content", "      <i>", "        \"2\"", "  \"3\""])
        );
        assert_eq!(tpl("<p><i>x<template></p>y</template>z"), in_body(&["<p>", "  <i>", "    \"x\"", "    <template>", "      content", "        \"y\"", "    \"z\""]));
    }

    #[test]
    fn the_end_tag_of_a_form_in_a_template_closes_the_form() {
        assert_eq!(tpl("<template><form>a</form>b"), in_body(&["<template>", "  content", "    <form>", "      \"a\"", "    \"b\""]));
        // in a table mode a form is empty (Chrome 155; the standard ignores it)
        assert_eq!(tpl("<template><tr></tr><form><b>x"), in_body(&["<template>", "  content", "    <tr>", "    <form>", "    <b>", "      \"x\""]));
    }

    #[test]
    fn a_plaintext_in_a_template_ends_only_with_the_input() {
        // html5lib tests18.dat, test 36
        assert_eq!(
            tree("<!doctype html><template><plaintext>a</template>b"),
            "| <!DOCTYPE html>\n| <html>\n|   <head>\n|     <template>\n|   <body>\n"
        );
        assert_eq!(
            parse("<!doctype html><template><plaintext>a</template>b").dump_with_contents(),
            "| <!DOCTYPE html>\n| <html>\n|   <head>\n|     <template>\n|       content\n|         <plaintext>\n|           \"a</template>b\"\n|   <body>\n"
        );
    }

    #[test]
    fn the_children_of_a_template_go_into_its_contents() {
        let page = "<body><template><p>a</p><template><b>x</b></template></template><template></template><svg><template></template></svg>";
        let names = |doc: &Document, n: NodeId| doc.children(n).map(|c| doc.element_name(c).to_string()).collect::<Vec<_>>();
        for doc in [parse(page), { let mut p = Parser::new(); p.feed(&page[..30]); p.feed(&page[30..]); p.finish() }] {
            let html = doc.children(doc.root()).find(|&c| doc.element_name(c) == "html").unwrap();
            let body = doc.children(html).last().unwrap();
            let kids: Vec<NodeId> = doc.children(body).collect();
            assert_eq!(kids.len(), 3);
            // the outer template keeps no child; its contents hold the paragraph and the inner template, which has its own contents
            assert!(doc.children(kids[0]).next().is_none());
            let outer = doc.template_content(kids[0]).expect("contents");
            assert!(matches!(doc.data(outer), NodeData::ShadowRoot) && doc.shadow_host(outer).is_none() && doc.parent(outer).is_none());
            assert_eq!(names(&doc, outer), ["p", "template"]);
            let inner = doc.children(outer).nth(1).unwrap();
            assert!(doc.children(inner).next().is_none());
            assert_eq!(names(&doc, doc.template_content(inner).expect("contents")), ["b"]);
            // a template with no child, and an SVG template, have no contents until a host asks for them
            assert!(doc.template_content(kids[1]).is_none() && doc.template_content(kids[2]).is_none());
        }
    }
}
