//! What an agent reads of a live page, as text: an outline of the accessibility tree with refs (`snapshot`), the elements that match a
//! query (`find`), the rendered text (`read`) and the links (`links`). One walk of the DOM with the cascade of the last frame: no layout
//! query per element, so a snapshot of a large page costs a few milliseconds.
//!
//! A ref (`e7`) names an element of the document of now. It stays the same for the same element across snapshots of one document, and
//! fails as stale after a navigation. `*` marks an element that the last snapshot of this document did not show.

use crate::locate::{Cx, norm};
use bare::control::Handle;
use bare::page::Page;
use bare_css::values::{Keyword, Property, Value as Css};
use bare_html::dom::{NodeData, NodeId};
use std::collections::{HashMap, HashSet};
use std::fmt::Write;

/// The refs of one document.
#[derive(Default)]
pub struct Refs {
    doc: usize,
    nodes: Vec<NodeId>,
    ids: HashMap<NodeId, u32>,
    /// What the last snapshot showed, for the `*` marks. `None` before the first snapshot of this document.
    last: Option<HashSet<NodeId>>,
    /// The most refs of an older document, so a ref from it fails as stale, not as unknown.
    old: u32,
}

impl Refs {
    fn sync(&mut self, doc: usize) {
        if self.doc != doc {
            let old = self.old.max(self.nodes.len() as u32);
            *self = Refs { doc, old, ..Refs::default() };
        }
    }

    fn mint(&mut self, n: NodeId) -> u32 {
        *self.ids.entry(n).or_insert_with(|| {
            self.nodes.push(n);
            self.nodes.len() as u32
        })
    }

    /// The element of ref `r` (1-based), or why there is none.
    pub fn get(&self, r: u32, doc: usize) -> Result<Handle, String> {
        let Some(&node) = self.nodes.get((r as usize).wrapping_sub(1)) else {
            return Err(if r <= self.old || self.doc != doc { format!("e{r} is from an older page (it navigated since): take a new snapshot") } else { format!("e{r} is not a ref of this page: take a snapshot") });
        };
        if self.doc != doc {
            return Err(format!("e{r} is from an older page (it navigated since): take a new snapshot"));
        }
        Ok(Handle { doc, node })
    }
}

#[derive(Clone, Copy, PartialEq)]
pub enum Mode {
    /// Controls, headings, landmarks, dialogs and alerts.
    Interactive,
    /// Those and the text between them.
    All,
}

/// Roles that an agent acts on.
const CONTROLS: &[&str] = &[
    "button", "link", "textbox", "searchbox", "checkbox", "radio", "combobox", "listbox", "option", "slider", "spinbutton", "switch", "tab",
    "menuitem", "menuitemcheckbox", "menuitemradio", "treeitem", "gridcell", "clickable",
];
/// Roles that give a control its context.
const MARKS: &[&str] = &[
    "heading", "dialog", "alertdialog", "alert", "status", "navigation", "main", "form", "search", "banner", "contentinfo", "complementary",
    "region", "tablist", "menu", "menubar", "tree", "grid", "table", "list", "img", "progressbar", "tabpanel", "log",
];
/// Roles whose subtree adds nothing once the line shows their name.
const LEAVES: &[&str] = &[
    "button", "link", "textbox", "searchbox", "checkbox", "radio", "option", "slider", "spinbutton", "switch", "tab", "menuitem",
    "menuitemcheckbox", "menuitemradio", "heading", "img", "progressbar",
];

struct Line {
    depth: usize,
    node: NodeId,
    role: &'static str,
    name: String,
    /// Attribute states, filled in the walk.
    flags: Vec<String>,
}

enum Item {
    El(Line),
    Text(usize, String),
}

fn intern(r: &str) -> &'static str {
    CONTROLS.iter().chain(MARKS).chain(["paragraph", "listitem", "cell", "row", "columnheader", "rowheader", "group", "article"].iter()).find(|x| x.eq_ignore_ascii_case(r)).copied().unwrap_or("generic")
}

/// The DOM events that make an element clickable for a user.
const CLICKS: &[&str] = &["click", "mousedown", "mouseup", "pointerdown", "pointerup", "touchstart"];

/// The role that a snapshot shows for `n`, with the roles that `locate` does not give. `clicks`: the nodes with a click listener.
fn role_of(cx: &Cx, n: NodeId, clicks: &HashSet<NodeId>) -> Option<&'static str> {
    if let Some(r) = cx.role(n) {
        return Some(intern(r));
    }
    match cx.tag(n) {
        "summary" => Some("button"),
        "section" if cx.attr(n, "aria-label").is_some() || cx.attr(n, "aria-labelledby").is_some() => Some("region"),
        "search" => Some("search"),
        _ if cx.attr(n, "contenteditable").is_some_and(|v| v != "false") => Some("textbox"),
        // A focusable element with no role: a custom control (a `div` with a click handler and a tabindex).
        _ if cx.attr(n, "tabindex").and_then(|t| t.trim().parse::<i32>().ok()).is_some_and(|t| t >= 0) => Some("clickable"),
        // A script listens for clicks on it: what a user clicks, though the page gave it no role.
        _ if clicks.contains(&n) || cx.attr(n, "onclick").is_some() => Some("clickable"),
        _ => None,
    }
}

fn inline(cx: &Cx, n: NodeId) -> bool {
    cx.styles.style(n).is_some_and(|s| matches!(s.get(Property::Display), Css::Keyword(Keyword::Inline | Keyword::InlineBlock | Keyword::InlineFlex)))
}

/// The attribute states of a line: what `aria-*` and HTML attributes say. The live states (value, checked, focus) come later.
fn flags(cx: &Cx, n: NodeId, role: &str) -> Vec<String> {
    let mut f = Vec::new();
    let a = |k| cx.attr(n, k);
    if role == "heading" {
        let lvl = a("aria-level").map(str::to_string).or_else(|| cx.tag(n).strip_prefix('h').filter(|d| d.len() == 1).map(str::to_string));
        if let Some(l) = lvl {
            f.push(format!("h{l}"));
        }
    }
    for (k, on) in [("aria-expanded", "expanded"), ("aria-selected", "selected"), ("aria-pressed", "pressed"), ("aria-current", "current")] {
        match a(k) {
            Some("true") => f.push(on.into()),
            Some("false") if k == "aria-expanded" => f.push("collapsed".into()),
            Some(v) if k == "aria-current" && v != "false" => f.push(on.into()),
            _ => {}
        }
    }
    if a("required").is_some() || a("aria-required") == Some("true") {
        f.push("required".into());
    }
    if a("readonly").is_some() || a("aria-readonly") == Some("true") {
        f.push("readonly".into());
    }
    if a("aria-invalid").is_some_and(|v| v != "false") {
        f.push("invalid".into());
    }
    if role == "link"
        && let Some(h) = a("href")
    {
        f.push(format!("href={}", short(h, 80)));
    }
    if cx.tag(n) == "input" && matches!(cx.input_type(n).as_str(), "email" | "password" | "tel" | "url" | "number" | "date" | "search") {
        f.push(cx.input_type(n));
    }
    f
}

fn short(s: &str, max: usize) -> String {
    if s.chars().count() > max { s.chars().take(max).collect::<String>() + "…" } else { s.to_string() }
}

/// The walk: elements with a shown role, and in `All` mode the text runs between them.
fn walk(cx: &Cx, root: NodeId, mode: Mode, clicks: &HashSet<NodeId>, out: &mut Vec<Item>) {
    let mut text = String::new();
    let flush = |text: &mut String, depth: usize, out: &mut Vec<Item>| {
        let t = norm(text);
        text.clear();
        if !t.is_empty() {
            out.push(Item::Text(depth, short(&t, 400)));
        }
    };
    // (node, depth, leaving): a node is visited when entered and, for a block, again when left to end its text run.
    let mut stack = vec![(root, 0usize, false)];
    while let Some((n, depth, leaving)) = stack.pop() {
        if leaving {
            flush(&mut text, depth, out);
            continue;
        }
        match cx.doc.data(n) {
            NodeData::Text(span) => {
                if mode == Mode::All {
                    text.push_str(cx.doc.text(*span));
                    text.push(' ');
                }
                continue;
            }
            NodeData::Element { .. } | NodeData::Document | NodeData::ShadowRoot => {}
            _ => continue,
        }
        let el = matches!(cx.doc.data(n), NodeData::Element { .. });
        if el {
            if !cx.boxed(n) || cx.attr(n, "aria-hidden").is_some_and(|v| v.trim().eq_ignore_ascii_case("true")) {
                continue;
            }
            if matches!(cx.tag(n), "head" | "script" | "style" | "template" | "noscript") {
                continue;
            }
        }
        let shown = el && !matches!(cx.styles.style(n).map(|s| s.get(Property::Visibility)), Some(Css::Keyword(Keyword::Hidden | Keyword::Collapse)));
        let role = if shown { role_of(cx, n, clicks) } else { None };
        let keep = role.is_some_and(|r| CONTROLS.contains(&r) || MARKS.contains(&r) || (mode == Mode::All && !matches!(r, "list" | "generic")));
        let block = el && !inline(cx, n);
        if keep || block {
            flush(&mut text, depth, out);
        }
        let mut inner = depth;
        if keep {
            let r = role.expect("kept");
            let mut name = cx.name(n);
            if name.is_empty() && (r == "clickable" || cx.tag(n) == "summary") {
                name = short(&norm(&cx.text(n)), 80);
            }
            // A live region says its message: the agent must see an error or a toast without a second read.
            if name.is_empty() && matches!(r, "alert" | "status" | "log") {
                name = short(&norm(&cx.text(n)), 200);
            }
            // A plain block with no name is noise: its text follows as text lines.
            let skip = matches!(r, "paragraph" | "listitem" | "cell" | "row" | "group" | "article") && name.is_empty();
            if !skip {
                out.push(Item::El(Line { depth, node: n, role: r, name: short(&name, 200), flags: flags(cx, n, r) }));
                inner = depth + 1;
            }
            if LEAVES.contains(&r) {
                continue;
            }
        }
        if block {
            stack.push((n, inner, true));
        }
        let mut kids: Vec<NodeId> = cx.doc.shadow_root(n).into_iter().chain(cx.doc.children(n)).collect();
        // A closed `<details>` shows its summary only.
        if el && cx.tag(n) == "details" && cx.attr(n, "open").is_none() {
            kids.retain(|&c| cx.tag(c) == "summary");
        }
        for &c in kids.iter().rev() {
            stack.push((c, inner, false));
        }
    }
    flush(&mut text, 0, out);
    // A text run that only repeats the name of the element next to it (the text of a `<label>`) adds nothing.
    let names: Vec<Option<String>> = out.iter().map(|i| if let Item::El(l) = i { Some(l.name.clone()) } else { None }).collect();
    let mut i = 0;
    out.retain(|it| {
        let k: usize = i;
        i += 1;
        let Item::Text(_, t) = it else { return true };
        ![k.wrapping_sub(1), k + 1].iter().any(|&j| names.get(j).and_then(|n| n.as_deref()) == Some(t.as_str()))
    });
}

/// Settle the page before a read: the document loaded, and the tasks that are due now run (bounded, so a busy page still answers).
/// Wait for the load, then run the tasks that are due now (at most 300 ms).
pub fn settle(p: &mut Page) {
    let _ = p.wait_load(std::time::Duration::from_secs(10));
    let end = std::time::Instant::now() + std::time::Duration::from_millis(300);
    while std::time::Instant::now() < end {
        let (more, next) = p.tick();
        let soon = next.is_some_and(|t| t <= std::time::Instant::now() + std::time::Duration::from_millis(20));
        if !more && !soon {
            break;
        }
    }
}

fn header(p: &mut Page) -> String {
    let url = p.url().unwrap_or_default();
    let title = crate::locate::title(p).unwrap_or_default();
    format!("url: {}\ntitle: {}\n", crate::run::safe_url(&url), norm(&title))
}

/// Keep `max` characters: 80 % from the top and 20 % from the end, cut at line ends.
fn budget(lines: Vec<String>, max: usize) -> String {
    let total: usize = lines.iter().map(|l| l.len() + 1).sum();
    if total <= max {
        return lines.join("\n");
    }
    let (mut head, mut used) = (Vec::new(), 0);
    for l in &lines {
        if used + l.len() + 1 > max * 4 / 5 {
            break;
        }
        used += l.len() + 1;
        head.push(l.as_str());
    }
    let (mut tail, mut used_t) = (Vec::new(), 0);
    for l in lines[head.len()..].iter().rev() {
        if used_t + l.len() + 1 > max / 5 {
            break;
        }
        used_t += l.len() + 1;
        tail.push(l.as_str());
    }
    tail.reverse();
    let cut = lines.len() - head.len() - tail.len();
    format!("{}\n… {cut} lines cut (pass a larger max_chars, or root=REF for one part) …\n{}", head.join("\n"), tail.join("\n"))
}

/// The value, checked and focus states, which live in the page, not in attributes.
fn live(p: &mut Page, h: Handle, role: &str, flags: &mut Vec<String>) -> Option<String> {
    if matches!(role, "checkbox" | "radio" | "switch" | "menuitemcheckbox" | "menuitemradio") && p.checked(h) {
        flags.push("checked".into());
    }
    if CONTROLS.contains(&role) && p.disabled(h) {
        flags.push("disabled".into());
    }
    if p.focused(h) {
        flags.push("focused".into());
    }
    if !matches!(role, "textbox" | "searchbox" | "combobox" | "spinbutton" | "slider" | "listbox") {
        return None;
    }
    let v = p.value(h)?;
    if p.is_password(h) {
        return Some(if v.is_empty() { String::new() } else { "••••".into() });
    }
    Some(short(&v, 200))
}

fn format(p: &mut Page, refs: &mut Refs, items: Vec<Item>, prev: Option<&HashSet<NodeId>>) -> Vec<String> {
    let doc = p.generation();
    let mut lines = Vec::with_capacity(items.len());
    for it in items {
        match it {
            Item::Text(d, t) => lines.push(format!("{}{t:?}", "  ".repeat(d))),
            Item::El(mut l) => {
                let r = refs.mint(l.node);
                let h = Handle { doc, node: l.node };
                let value = live(p, h, l.role, &mut l.flags);
                let mut s = String::with_capacity(48);
                let _ = write!(s, "{}{}[e{r}] {}", "  ".repeat(l.depth), if prev.is_some_and(|p| !p.contains(&l.node)) { "*" } else { "" }, l.role);
                if !l.name.is_empty() {
                    let _ = write!(s, " {:?}", l.name);
                }
                if let Some(v) = value.filter(|v| !v.is_empty()) {
                    let _ = write!(s, " = {v:?}");
                }
                if !l.flags.is_empty() {
                    let _ = write!(s, " ({})", l.flags.join(", "));
                }
                lines.push(s);
            }
        }
    }
    lines
}

/// The outline of the page, or of the element `root`, in at most `max` characters.
pub fn snapshot(p: &mut Page, refs: &mut Refs, mode: Mode, root: Option<u32>, max: usize) -> Result<String, String> {
    settle(p);
    let doc = p.generation();
    refs.sync(doc);
    let start = root.map(|r| refs.get(r, doc)).transpose()?;
    let clicks = p.browser().map(|b| b.listened(CLICKS)).unwrap_or_default();
    let mut items = Vec::new();
    p.inspect(|d, frame| {
        let cx = Cx { doc: d, styles: &frame.styles };
        walk(&cx, start.map_or(d.root(), |h| h.node), mode, &clicks, &mut items);
    })
    .ok_or_else(|| p.errors.last().cloned().unwrap_or_else(|| "the page has no document or no layout yet".into()))?;
    let shown: HashSet<NodeId> = items.iter().filter_map(|i| if let Item::El(l) = i { Some(l.node) } else { None }).collect();
    let prev = if root.is_none() { refs.last.take() } else { None };
    let lines = format(p, refs, items, prev.as_ref());
    if root.is_none() {
        refs.last = Some(shown);
    } else {
        refs.last = prev.or(refs.last.take());
    }
    let body = if lines.is_empty() { "(no elements: the page is empty, or it has not rendered yet)".into() } else { budget(lines, max) };
    Ok(header(p) + &body)
}

/// The elements that `loc` matches, as snapshot lines with refs.
pub fn find(p: &mut Page, refs: &mut Refs, loc: &crate::locate::Locator, max: usize) -> Result<String, String> {
    settle(p);
    let doc = p.generation();
    refs.sync(doc);
    let found = crate::locate::find(p, loc)?;
    let clicks = p.browser().map(|b| b.listened(CLICKS)).unwrap_or_default();
    let mut items = Vec::new();
    p.inspect(|d, frame| {
        let cx = Cx { doc: d, styles: &frame.styles };
        for h in &found.shown {
            let role = role_of(&cx, h.node, &clicks).unwrap_or("generic");
            let name = match cx.name(h.node) {
                n if n.is_empty() => norm(&cx.text(h.node)),
                n => n,
            };
            items.push(Item::El(Line { depth: 0, node: h.node, role, name: short(&name, 200), flags: flags(&cx, h.node, role) }));
        }
    });
    let n = items.len();
    let mut out = format!("{n} match(es) for {loc}");
    if found.hidden > 0 {
        let _ = write!(out, ", {} more not rendered", found.hidden);
    }
    let lines = format(p, refs, items, None);
    if !lines.is_empty() {
        out.push('\n');
        out.push_str(&budget(lines, max));
    }
    Ok(out)
}

/// The rendered text of the page or of one element, with a line break at each block. `offset` and `max` page through it.
pub fn read(p: &mut Page, refs: &mut Refs, root: Option<u32>, offset: usize, max: usize) -> Result<String, String> {
    settle(p);
    let doc = p.generation();
    refs.sync(doc);
    let start = root.map(|r| refs.get(r, doc)).transpose()?;
    let mut items = Vec::new();
    p.inspect(|d, frame| {
        let cx = Cx { doc: d, styles: &frame.styles };
        walk_text(&cx, start.map_or(d.root(), |h| h.node), &mut items);
    })
    .ok_or_else(|| p.errors.last().cloned().unwrap_or_else(|| "the page has no document or no layout yet".into()))?;
    let text = items.join("\n");
    let total = text.chars().count();
    let page: String = text.chars().skip(offset).take(max).collect();
    let end = offset + page.chars().count();
    let more = if end < total { format!("\n… {} more characters: read again with offset={end}", total - end) } else { String::new() };
    Ok(format!("{}chars {offset}-{end} of {total}\n{page}{more}", header(p)))
}

/// Text only: a line for each run of text between block boundaries.
fn walk_text(cx: &Cx, root: NodeId, out: &mut Vec<String>) {
    let mut text = String::new();
    let flush = |text: &mut String, out: &mut Vec<String>| {
        let t = norm(text);
        text.clear();
        if !t.is_empty() {
            out.push(t);
        }
    };
    let mut stack = vec![(root, false)];
    while let Some((n, leaving)) = stack.pop() {
        if leaving {
            flush(&mut text, out);
            continue;
        }
        match cx.doc.data(n) {
            NodeData::Text(span) => {
                text.push_str(cx.doc.text(*span));
                text.push(' ');
                continue;
            }
            NodeData::Element { .. } => {
                if !cx.boxed(n) || matches!(cx.tag(n), "head" | "script" | "style" | "template" | "noscript") {
                    continue;
                }
                if matches!(cx.styles.style(n).map(|s| s.get(Property::Visibility)), Some(Css::Keyword(Keyword::Hidden | Keyword::Collapse))) {
                    continue;
                }
                if cx.tag(n) == "img"
                    && let Some(a) = cx.attr(n, "alt").filter(|a| !a.trim().is_empty())
                {
                    text.push_str(&format!("[image: {a}] "));
                }
                if cx.tag(n) == "br" {
                    flush(&mut text, out);
                }
                if !inline(cx, n) {
                    flush(&mut text, out);
                    stack.push((n, true));
                }
                if cx.tag(n) == "details" && cx.attr(n, "open").is_none() {
                    for c in cx.doc.children(n).filter(|&c| cx.tag(c) == "summary").collect::<Vec<_>>().into_iter().rev() {
                        stack.push((c, false));
                    }
                    continue;
                }
            }
            NodeData::Document | NodeData::ShadowRoot => {}
            _ => continue,
        }
        let kids: Vec<NodeId> = cx.doc.shadow_root(n).into_iter().chain(cx.doc.children(n)).collect();
        for &c in kids.iter().rev() {
            stack.push((c, false));
        }
    }
    flush(&mut text, out);
}

/// The rendered links: ref, text and absolute URL.
pub fn links(p: &mut Page, refs: &mut Refs, max: usize) -> Result<String, String> {
    settle(p);
    let doc = p.generation();
    refs.sync(doc);
    let base = p.url().and_then(|u| bare_url::Url::parse(&u).ok());
    let mut found = Vec::new();
    p.inspect(|d, frame| {
        let cx = Cx { doc: d, styles: &frame.styles };
        let mut items = Vec::new();
        walk(&cx, d.root(), Mode::Interactive, &HashSet::new(), &mut items);
        for it in items {
            if let Item::El(l) = it
                && l.role == "link"
                && let Some(h) = cx.attr(l.node, "href")
            {
                found.push((l.node, l.name, h.to_string()));
            }
        }
    })
    .ok_or_else(|| p.errors.last().cloned().unwrap_or_else(|| "the page has no document or no layout yet".into()))?;
    let mut lines = Vec::with_capacity(found.len());
    for (n, name, href) in found {
        let abs = base.as_ref().and_then(|b| b.join(&href).ok()).map_or(href, |u| u.to_string());
        lines.push(format!("[e{}] {name:?} -> {abs}", refs.mint(n)));
    }
    Ok(format!("{}{} link(s)\n{}", header(p), lines.len(), budget(lines, max)))
}
