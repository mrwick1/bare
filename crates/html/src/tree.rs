//! The tree builder: turn tokens into a DOM tree.
//!
//! The tokenizer gives tokens. The tree builder decides where each one goes. Its job is to make a
//! good tree out of bad HTML, in the one way that the HTML standard says. For example:
//! - `<title>Hi</title><p>x` has no `<html>`, `<head>` or `<body>`. The builder adds them.
//! - `<p>a<p>b` has no `</p>`. A new `<p>` closes the open one.
//! - `<table>x<tr><td>y` has text in a table, where it is not allowed. The builder moves the
//!   text in front of the table. This is called "foster parenting".
//!
//! The builder keeps two things:
//! - The stack of open elements. When we read `<div>`, the new div is the child of the element at the
//!   top of the stack, and then it goes on the stack. When we read `</div>`, we pop the stack.
//!   The element at the top is the "current node".
//! - The insertion mode. The standard has about 20 modes, like "before head", "in body" and "in table".
//!   Each mode says what to do with each kind of token. We use a few modes. For tables we look at the
//!   stack instead of keeping modes: if the current node is a `tr`, we are "in a row".
//!
//! The standard says that every page, however broken, gives a tree. No error stops the builder.
//!
//! Left out (known gaps):
//! - The adoption agency algorithm and the list of active formatting elements. They fix
//!   misnested tags like `<b><p>x</b>y</p>`. Here `<b>` is an ordinary element.
//! - Foreign content (SVG and MathML), `<template>`, `<select>` modes, framesets and `<noscript>`.
//! - Quirks mode, and merging the attributes of a second `<html>` or `<body>` tag.
//!
//! Comments compare Rust with JavaScript, because you know JS.

use crate::dom::{Document, NodeData, NodeId};
use std::borrow::Cow;

use crate::tokenizer::{Attr, Token, tokenize};

/// Parse a page (already decoded to text) into a DOM tree.
pub fn parse(input: &str) -> Document {
    let mut builder = Builder {
        doc: Document::new(),
        open: Vec::new(),
        mode: Mode::Initial,
        text_return: Mode::InBody,
        head: None,
        form: None,
        skip_newline: false,
        foster: false,
    };
    for token in tokenize(input) {
        builder.process(token);
    }
    builder.finish();
    builder.doc.shrink_to_fit();
    // A `Vec` that grows keeps spare room (up to twice as much). The page is read, so give the room back.
    builder.doc
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

struct Builder {
    doc: Document,
    open: Vec<NodeId>,
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
}

impl Builder {
    // ---- Small helpers ----

    /// The current node: the top of the stack. Before the first element it is the document.
    fn current(&self) -> NodeId {
        self.open.last().copied().unwrap_or(self.doc.root())
    }

    /// Is this node an element with one of these names?
    fn is(&self, id: NodeId, names: &[&str]) -> bool {
        matches!(self.doc.data(id), NodeData::Element { name, .. } if names.contains(&self.doc.atom_str(*name)))
        // `matches!` with an `if` checks the pattern and then the condition. It gives true or false.
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
        if self.foster
            && self.is(current, TABLE_PARTS)
            && let Some(i) = self.open.iter().rposition(|&n| self.is(n, &["table"]))
        {
            // `rposition` finds the last table on the stack: the one we are in.
            let table = self.open[i];
            return match self.doc.parent(table) {
                Some(parent) => (parent, Some(table)),
                None => (self.open[i - 1], None),
            };
        }
        (current, None)
    }

    /// Add an element. `push` says if it also goes on the stack. Void elements (`br`, `img`) do not.
    fn insert_element(&mut self, name: &str, attrs: &[Attr<'_>], push: bool) -> NodeId {
        let (parent, before) = self.place();
        let id = self.doc.insert_element(parent, before, name, attrs);
        if push {
            self.open.push(id);
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
            if self.is(node, SCOPE) || self.is(node, extra) {
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
        if self.in_scope(&["p"], &["button"]) {
            self.close_implied("p");
            self.pop_until(&["p"]);
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
        }
    }

    /// The input is over. A page that stops early still gets its `html`, `head` and `body`.
    fn finish(&mut self) {
        if self.mode == Mode::Text {
            self.open.pop();
            self.mode = self.text_return;
        }
        if !matches!(self.mode, Mode::InBody | Mode::AfterBody | Mode::AfterAfterBody) {
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
            _ => return false,
        }
        true
    }

    /// Add an element whose content is raw text. Go to `Text` mode until its end tag.
    fn raw_text_element(&mut self, name: &str, attrs: &[Attr<'_>]) {
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
                if matches!(&*name, "base" | "basefont" | "bgsound" | "link" | "meta" | "title" | "style" | "script" | "noframes") {
                    // A head element after the head has ended. It still goes into the head.
                    let head = self.head.expect("the head exists in this mode");
                    self.open.push(head);
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
                if self.open.iter().any(|&n| self.is(n, &["table"])) {
                    self.table_start(&name, &attrs);
                }
                // Without a table, these tags are ignored.
            }
            Token::EndTag { name } if is_table_structure(&name) || name == "table" => self.table_end(&name),
            Token::StartTag { name, attrs, self_closing } if name == "table" && in_table_part => {
                self.pop_until(&["table"]);
                // A table in a table part ends the table that we are in. Then the new one starts.
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
            Token::Text(text) => self.insert_text(&text),
            Token::Comment(text) => self.insert_comment(&text),
            Token::Doctype(_) => {}
            Token::StartTag { name, attrs, .. } => self.body_start(&name, attrs),
            Token::EndTag { name } => self.body_end(&name),
        }
    }

    fn body_start(&mut self, name: &str, attrs: Vec<Attr<'_>>) {
        match name {
            "html" | "body" | "head" => {}
            // A second `<html>` or `<body>` is ignored. The standard merges their attributes. We do not yet.
            "base" | "basefont" | "bgsound" | "link" | "meta" | "title" | "style" | "script" | "noframes" => {
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
                if self.form.is_none() {
                    self.close_p();
                    self.form = Some(self.insert_element(name, &attrs, true));
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
                self.insert_element(name, &attrs, true);
            }
            "a" => {
                // An `<a>` inside an `<a>` closes the first. The standard does this with the adoption agency
                // algorithm. We only close the open `a`.
                for &node in self.open.iter().rev() {
                    if self.is(node, &["a"]) {
                        self.pop_until(&["a"]);
                        break;
                    }
                    if self.is(node, SCOPE) {
                        break;
                    }
                }
                self.insert_element(name, &attrs, true);
            }
            "table" => {
                self.close_p();
                self.insert_element(name, &attrs, true);
            }
            "area" | "br" | "embed" | "img" | "keygen" | "wbr" | "input" | "param" | "source" | "track" => {
                self.insert_element(name, &attrs, false);
                // Void elements have no end tag and no children.
            }
            "image" => {
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
                self.raw_text_element(name, &attrs);
            }
            "iframe" | "noembed" => self.raw_text_element(name, &attrs),
            "option" => {
                if self.is(self.current(), &["option"]) {
                    self.open.pop();
                }
                self.insert_element(name, &attrs, true);
            }
            "optgroup" => {
                if self.is(self.current(), &["option"]) {
                    self.open.pop();
                }
                if self.is(self.current(), &["optgroup"]) {
                    self.open.pop();
                }
                self.insert_element(name, &attrs, true);
            }
            _ => {
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
            if self.is(node, SPECIAL) && !self.is(node, &["address", "div", "p"]) {
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
                let form = self.form.take();
                if form.is_some() && self.in_scope(&["form"], &[]) {
                    self.close_implied("");
                    self.pop_until(&["form"]);
                }
            }
            "p" => {
                if !self.in_scope(&["p"], &["button"]) {
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
            "br" => self.body_start("br", vec![]),
            // `</br>` is read as `<br>`.
            _ if CLOSES_P.contains(&name) || matches!(name, "button" | "applet" | "marquee" | "object" | "pre" | "listing") => {
                if self.in_scope(&[name], &[]) {
                    self.close_implied("");
                    self.pop_until(&[name]);
                }
            }
            _ => {
                // Any other end tag: close the nearest open element with this name.
                // Stop at a special element: `</span>` cannot close the `div` that holds the span.
                for i in (0..self.open.len()).rev() {
                    let node = self.open[i];
                    if self.is(node, &[name]) {
                        self.close_implied(name);
                        self.open.truncate(i);
                        return;
                    }
                    if self.is(node, SPECIAL) {
                        return;
                    }
                }
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
            match (current.as_str(), name) {
                ("table", "caption" | "colgroup" | "tbody" | "thead" | "tfoot") => {
                    self.insert_element(name, attrs, true);
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
                    self.insert_element(name, attrs, true);
                    return;
                }
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
}
