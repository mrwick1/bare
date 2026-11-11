//! Locators: find an element as a user names it. `css="..."` is a CSS selector. `role=button name="Sign in"`, `label="Email"`,
//! `text="Overview"` and `placeholder~"search"` use what a user sees, as Playwright's `getByRole`, `getByLabel`, `getByText` and
//! `getByPlaceholder` do. `=` matches the whole text after whitespace is collapsed; `~` matches a part of it, in any case.
//!
//! - Role: the `role` attribute (its first word), else the implicit role of the element (HTML-AAM, the subset in `implicit_role`).
//! - Accessible name (accname 1.2, the steps that the controls of a form, links, buttons, menus and headings use): `aria-labelledby`,
//!   `aria-label`, the native label (`<label>`, the value of an `<input type=submit>`, `alt`, `<legend>`, `<caption>`), the content
//!   for the roles that take their name from it, `title`, `placeholder`.
//! - Label: the text of `aria-labelledby`, `aria-label`, or the `<label>` elements of a control.
//! - Text: the smallest elements whose rendered text matches (an element that matches and has a child element that matches is skipped).
//!
//! Role, label, text and placeholder locators skip elements that are not rendered (`display: none`, `visibility: hidden`, under
//! `aria-hidden="true"`); `Found::hidden` counts them, so a failure can say "it is there, but hidden". A CSS locator keeps them: the
//! action then refuses them as hidden.
//!
//! Gaps: locators do not look into shadow roots (the text of a shadow tree counts for its host); no `aria-owns`, no CSS `content`.

use bare::control::Handle;
use bare::page::Page;
use bare_css::cascade::Styles;
use bare_css::values::{Keyword, Property, Value as Css};
use bare_html::dom::{Document, NodeData, NodeId};
use std::collections::HashSet;

/// How a text must match.
#[derive(Clone, Debug, PartialEq)]
pub enum Text {
    /// The whole text, after whitespace is collapsed (`=`).
    Is(String),
    /// A part of the text, in any case (`~`).
    Has(String),
}

impl Text {
    pub fn matches(&self, s: &str) -> bool {
        match self {
            Text::Is(t) => norm(s) == norm(t),
            Text::Has(t) => norm(s).to_lowercase().contains(&norm(t).to_lowercase()),
        }
    }
}

impl std::fmt::Display for Text {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Text::Is(t) => write!(f, "={t:?}"),
            Text::Has(t) => write!(f, "~{t:?}"),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Locator {
    Css(String),
    Role(String, Option<Text>),
    Label(Text),
    Text(Text),
    Placeholder(Text),
    /// A ref from a snapshot (`ref=e7`): the run resolves it, not `find`.
    Ref(u32),
}

impl std::fmt::Display for Locator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Locator::Css(s) => write!(f, "css={s:?}"),
            Locator::Role(r, None) => write!(f, "role={r}"),
            Locator::Role(r, Some(n)) => write!(f, "role={r} name{n}"),
            Locator::Label(t) => write!(f, "label{t}"),
            Locator::Text(t) => write!(f, "text{t}"),
            Locator::Placeholder(t) => write!(f, "placeholder{t}"),
            Locator::Ref(r) => write!(f, "ref=e{r}"),
        }
    }
}

/// The matches of a locator: the rendered ones in tree order, and how many more matched but are not rendered.
pub struct Found {
    pub shown: Vec<Handle>,
    pub hidden: usize,
}

/// Collapse whitespace and trim.
pub fn norm(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The elements that `loc` names in the document of now.
pub fn find(page: &mut Page, loc: &Locator) -> Result<Found, String> {
    if let Locator::Css(sel) = loc {
        return Ok(Found { shown: page.query_all(sel)?, hidden: 0 });
    }
    if let Locator::Ref(r) = loc {
        return Err(format!("ref=e{r} needs a live session with a snapshot"));
    }
    let all = page.query_all("*")?;
    let list = page
        .inspect(|doc, frame| {
            let cx = Cx { doc, styles: &frame.styles };
            let hits: Vec<(Handle, bool)> = all.iter().filter(|h| cx.wanted(h.node, loc)).map(|&h| (h, cx.rendered(h.node))).collect();
            if !matches!(loc, Locator::Text(_)) {
                return hits;
            }
            // The smallest elements only: drop each match that has a match inside it.
            let mut outer = HashSet::new();
            for (h, _) in &hits {
                let mut up = doc.parent(h.node);
                while let Some(p) = up.filter(|p| outer.insert(*p)) {
                    up = doc.parent(p);
                }
            }
            hits.into_iter().filter(|(h, _)| !outer.contains(&h.node)).collect()
        })
        .ok_or_else(|| page.errors.last().cloned().unwrap_or_else(|| "the page has no document or no layout".into()))?;
    let hidden = list.iter().filter(|(_, shown)| !shown).count();
    Ok(Found { shown: list.into_iter().filter(|(_, shown)| *shown).map(|(h, _)| h).collect(), hidden })
}

/// The role and the accessible name of an element (for a failure message and for tests).
pub fn role_and_name(page: &mut Page, h: Handle) -> Option<(String, String)> {
    page.inspect(|doc, frame| {
        let cx = Cx { doc, styles: &frame.styles };
        (cx.role(h.node).unwrap_or("").to_string(), cx.name(h.node))
    })
}

/// The rendered text of an element (`text=` and `expect LOC text=` read it).
pub fn text(page: &mut Page, h: Handle) -> Option<String> {
    page.inspect(|doc, frame| norm(&Cx { doc, styles: &frame.styles }.text(h.node)))
}

/// `document.title`: the text of the first `<title>` element, or what a script set. Empty when the page has none.
pub fn title(page: &mut Page) -> Option<String> {
    let b = page.browser()?;
    let r = b.run_script("document.title");
    let title = match r {
        Ok(v @ bare_js::value::Value::String(_)) => Some(b.it.display(v)),
        _ => None,
    };
    page.touched();
    title
}

pub(crate) struct Cx<'a> {
    pub(crate) doc: &'a Document,
    pub(crate) styles: &'a Styles,
}

/// The roles that take their name from the content (accname 1.2, "name from: contents").
const FROM_CONTENT: &[&str] = &[
    "button", "cell", "checkbox", "columnheader", "gridcell", "heading", "link", "menuitem", "menuitemcheckbox", "menuitemradio", "option", "radio",
    "row", "rowheader", "switch", "tab", "tooltip", "treeitem",
];

impl<'a> Cx<'a> {
    pub(crate) fn attr(&self, n: NodeId, name: &str) -> Option<&'a str> {
        self.doc.find_atom(name).and_then(|a| self.doc.attr(n, a))
    }

    pub(crate) fn tag(&self, n: NodeId) -> &'a str {
        if self.doc.is_foreign(n) { "" } else { self.doc.element_name(n) }
    }

    /// The `type` of an `<input>`, in lowercase (`text` when absent).
    pub(crate) fn input_type(&self, n: NodeId) -> String {
        self.attr(n, "type").map_or_else(|| "text".into(), |t| t.trim().to_ascii_lowercase())
    }

    /// Does the element have a box, not `visibility: hidden`, and no `aria-hidden="true"` on it or above it?
    pub(crate) fn rendered(&self, n: NodeId) -> bool {
        let Some(st) = self.styles.style(n) else { return false };
        if matches!(st.get(Property::Visibility), Css::Keyword(Keyword::Hidden | Keyword::Collapse)) {
            return false;
        }
        let mut cur = Some(n);
        while let Some(c) = cur {
            if !self.boxed(c) || self.attr(c, "aria-hidden").is_some_and(|v| v.trim().eq_ignore_ascii_case("true")) {
                return false;
            }
            cur = self.doc.parent_or_host(c);
        }
        true
    }

    /// Can this node have a box: not an element, or an element with a style that is not `display: none`? (The cascade can keep a
    /// style for an element under `display: none`, for `getComputedStyle`.)
    pub(crate) fn boxed(&self, n: NodeId) -> bool {
        !matches!(self.doc.data(n), NodeData::Element { .. }) || self.styles.style(n).is_some_and(|s| !matches!(s.get(Property::Display), Css::Keyword(Keyword::None)))
    }

    fn wanted(&self, n: NodeId, loc: &Locator) -> bool {
        match loc {
            Locator::Css(_) => true,
            Locator::Role(r, name) => self.role(n).is_some_and(|x| x.eq_ignore_ascii_case(r)) && name.as_ref().is_none_or(|t| t.matches(&self.name(n))),
            Locator::Label(t) => self.label(n).is_some_and(|l| t.matches(&l)),
            Locator::Placeholder(t) => matches!(self.tag(n), "input" | "textarea") && self.attr(n, "placeholder").is_some_and(|p| t.matches(p)),
            Locator::Text(t) => !self.in_head(n) && t.matches(&self.text(n)),
            Locator::Ref(_) => false,
        }
    }

    pub(crate) fn in_head(&self, n: NodeId) -> bool {
        let mut cur = Some(n);
        while let Some(c) = cur {
            if matches!(self.tag(c), "head" | "script" | "style" | "template" | "noscript") {
                return true;
            }
            cur = self.doc.parent(c);
        }
        false
    }

    /// The role of the element: its `role` attribute (the first word), else its implicit role.
    pub(crate) fn role(&self, n: NodeId) -> Option<&'a str> {
        if let Some(r) = self.attr(n, "role").and_then(|r| r.split_whitespace().next()) {
            return Some(r);
        }
        let has = |a| self.attr(n, a).is_some();
        Some(match self.tag(n) {
            "a" | "area" if has("href") => "link",
            "button" => "button",
            "input" => match self.input_type(n).as_str() {
                "button" | "submit" | "reset" | "image" => "button",
                "checkbox" => "checkbox",
                "radio" => "radio",
                "range" => "slider",
                "number" => "spinbutton",
                "search" => "searchbox",
                "hidden" | "file" | "color" | "date" | "datetime-local" | "month" | "time" | "week" => return None,
                _ => "textbox",
            },
            "textarea" => "textbox",
            "select" if has("multiple") || self.attr(n, "size").and_then(|s| s.trim().parse::<u32>().ok()).is_some_and(|s| s > 1) => "listbox",
            "select" => "combobox",
            "option" => "option",
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => "heading",
            "img" if self.attr(n, "alt") == Some("") => "presentation",
            "img" => "img",
            "ul" | "ol" | "menu" => "list",
            "li" => "listitem",
            "nav" => "navigation",
            "main" => "main",
            "aside" => "complementary",
            "header" => "banner",
            "footer" => "contentinfo",
            "form" => "form",
            "dialog" => "dialog",
            "table" => "table",
            "tr" => "row",
            "td" => "cell",
            "th" => "columnheader",
            "hr" => "separator",
            "p" => "paragraph",
            "fieldset" | "details" => "group",
            "article" => "article",
            "progress" => "progressbar",
            _ => return None,
        })
    }

    /// The accessible name of the element.
    pub(crate) fn name(&self, n: NodeId) -> String {
        norm(&self.alt(n, false, 0))
    }

    /// The text alternative of a node (accname 1.2, step 2), without the parts that the controls here do not use.
    fn alt(&self, n: NodeId, in_ref: bool, depth: u8) -> String {
        if depth > 0 && !self.rendered(n) || depth > 32 {
            return String::new();
        }
        if !in_ref && let Some(ids) = self.attr(n, "aria-labelledby") {
            let text: Vec<String> = ids.split_whitespace().filter_map(|id| self.by_id(id)).map(|r| self.alt(r, true, 0)).collect();
            if text.iter().any(|t| !t.trim().is_empty()) {
                return text.join(" ");
            }
        }
        if let Some(l) = self.attr(n, "aria-label").filter(|l| !l.trim().is_empty()) {
            return l.to_string();
        }
        let tag = self.tag(n);
        let ty = if tag == "input" { self.input_type(n) } else { String::new() };
        let native = match (tag, ty.as_str()) {
            ("input", "button" | "submit" | "reset") => {
                Some(self.attr(n, "value").map(str::to_string).unwrap_or_else(|| match ty.as_str() { "submit" => "Submit".into(), "reset" => "Reset".into(), _ => String::new() }))
            }
            ("input", "image") | ("img" | "area", _) => self.attr(n, "alt").map(str::to_string),
            ("input" | "textarea" | "select", _) => Some(self.labels(n).iter().map(|&l| self.content(l, depth + 1, Some(n))).collect::<Vec<_>>().join(" ")),
            ("fieldset", _) => self.child(n, "legend").map(|c| self.content(c, depth + 1, None)),
            ("table", _) => self.child(n, "caption").map(|c| self.content(c, depth + 1, None)),
            _ => None,
        };
        if let Some(t) = native.filter(|t| !t.trim().is_empty()) {
            return t;
        }
        let from_content = in_ref || depth > 0 || self.role(n).is_some_and(|r| FROM_CONTENT.contains(&r));
        if from_content && !matches!(tag, "input" | "textarea" | "select") {
            let t = self.content(n, depth + 1, None);
            if !t.trim().is_empty() {
                return t;
            }
        }
        self.attr(n, "title").or_else(|| matches!(tag, "input" | "textarea").then(|| self.attr(n, "placeholder")).flatten()).unwrap_or("").to_string()
    }

    /// The text of the children of `n`, each element through `alt` (a block box is set off by spaces). `skip`: a control inside its label.
    fn content(&self, n: NodeId, depth: u8, skip: Option<NodeId>) -> String {
        let mut out = String::new();
        for c in self.doc.flat_children(n) {
            match self.doc.data(c) {
                NodeData::Text(span) => out.push_str(self.doc.text(*span)),
                NodeData::Element { .. } if Some(c) != skip => {
                    let block = self.styles.style(c).is_some_and(|s| !matches!(s.get(Property::Display), Css::Keyword(Keyword::Inline)));
                    let t = self.alt(c, false, depth);
                    if block {
                        out.push(' ');
                    }
                    out.push_str(&t);
                    if block {
                        out.push(' ');
                    }
                }
                _ => {}
            }
        }
        out
    }

    /// The rendered text of an element (`innerText` without its line rules), or the value of a button `<input>`.
    pub(crate) fn text(&self, n: NodeId) -> String {
        if self.tag(n) == "input" {
            return match self.input_type(n).as_str() {
                "button" | "submit" | "reset" => self.attr(n, "value").unwrap_or("").to_string(),
                _ => String::new(),
            };
        }
        let mut out = String::new();
        self.text_into(n, &mut out);
        out
    }

    fn text_into(&self, n: NodeId, out: &mut String) {
        for c in self.doc.flat_children(n) {
            match self.doc.data(c) {
                NodeData::Text(span) => out.push_str(self.doc.text(*span)),
                NodeData::Element { .. } if self.boxed(c) && !self.in_head(c) => {
                    out.push(' ');
                    self.text_into(c, out);
                    out.push(' ');
                }
                _ => {}
            }
        }
    }

    /// The label text of a control: `aria-labelledby`, `aria-label`, or its `<label>` elements.
    pub(crate) fn label(&self, n: NodeId) -> Option<String> {
        if let Some(ids) = self.attr(n, "aria-labelledby") {
            return Some(ids.split_whitespace().filter_map(|id| self.by_id(id)).map(|r| self.alt(r, true, 0)).collect::<Vec<_>>().join(" "));
        }
        if let Some(l) = self.attr(n, "aria-label") {
            return Some(l.to_string());
        }
        if !matches!(self.tag(n), "input" | "textarea" | "select") || self.input_type(n) == "hidden" {
            return None;
        }
        let labels = self.labels(n);
        (!labels.is_empty()).then(|| labels.iter().map(|&l| self.content(l, 1, Some(n))).collect::<Vec<_>>().join(" "))
    }

    /// The `<label>` elements of a control: the ones with `for` = its id, and the one around it.
    fn labels(&self, n: NodeId) -> Vec<NodeId> {
        let mut out = Vec::new();
        if let Some(id) = self.attr(n, "id").filter(|id| !id.is_empty()) {
            self.walk(self.doc.root(), &mut |c| {
                if self.tag(c) == "label" && self.attr(c, "for") == Some(id) {
                    out.push(c);
                }
                false
            });
        }
        let mut up = self.doc.parent(n);
        while let Some(p) = up {
            if self.tag(p) == "label" {
                if !out.contains(&p) && self.attr(p, "for").is_none_or(|f| Some(f) == self.attr(n, "id")) {
                    out.push(p);
                }
                break;
            }
            up = self.doc.parent(p);
        }
        out
    }

    fn child(&self, n: NodeId, tag: &str) -> Option<NodeId> {
        self.doc.children(n).find(|&c| self.tag(c) == tag)
    }

    fn by_id(&self, id: &str) -> Option<NodeId> {
        let mut found = None;
        self.walk(self.doc.root(), &mut |c| {
            if self.attr(c, "id") == Some(id) {
                found = Some(c);
            }
            found.is_some()
        });
        found
    }

    /// Visit the elements under `n` in tree order until `f` gives true.
    fn walk(&self, n: NodeId, f: &mut impl FnMut(NodeId) -> bool) -> bool {
        for c in self.doc.children(n) {
            if matches!(self.doc.data(c), NodeData::Element { .. }) && (f(c) || self.walk(c, f)) {
                return true;
            }
        }
        false
    }
}
