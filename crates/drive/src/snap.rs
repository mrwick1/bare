//! What an agent reads of a live page, as text: an outline of the accessibility tree with refs (`snapshot`), the elements that match a
//! query (`find`), the rendered text (`read`) and the links (`links`). One walk of the DOM with the cascade of the last frame: no layout
//! query per element, so a snapshot of a large page costs a few milliseconds.
//!
//! A ref (`e7`) names an element of the document of now. It stays the same for the same element across snapshots of one document, and
//! fails as stale after a navigation. `*` marks an element that the last snapshot of this document did not show, or showed with another
//! name, value or state. Replies are small on purpose (an agent pays for each character): a link line shows its `href` only when it has
//! no name (`links` has them all), a `diff` snapshot shows only the marked lines and their parents, and tracking parameters are cut from URLs.

use crate::locate::{Cx, norm};
use bare::control::Handle;
use bare::page::Page;
use bare_css::values::{Keyword, Property, Value as Css};
use bare_html::dom::{NodeData, NodeId};
use std::collections::{HashMap, HashSet};
use std::fmt::Write;
use std::hash::{DefaultHasher, Hash, Hasher};

fn hash<T: Hash + ?Sized>(t: &T) -> u64 {
    let mut h = DefaultHasher::new();
    t.hash(&mut h);
    h.finish()
}

/// What a snapshot showed: each element with a hash of its line, and the hashes of the text lines. `read` holds a hash of each line
/// of the rendered text, so an interactive diff can name the text that changed.
#[derive(Default)]
struct Seen {
    els: HashMap<NodeId, u64>,
    text: HashSet<u64>,
    read: HashSet<u64>,
}

/// The most text lines, and the most characters of each, that a diff adds.
const TEXT_LINES: usize = 5;
const TEXT_CHARS: usize = 200;

/// A parameter that only tracks the visitor: it never changes the page that the URL names.
fn tracking(k: &str) -> bool {
    k.starts_with("utm_") || k.starts_with("wmf_") || matches!(k, "fbclid" | "gclid" | "dclid" | "msclkid" | "mc_cid" | "mc_eid" | "_ga" | "_gl" | "igshid" | "yclid" | "ref_src" | "ref_url")
}

/// `url` without tracking parameters.
fn clean_url(url: &str) -> String {
    let (rest, frag) = url.split_once('#').map_or((url, ""), |(a, b)| (a, b));
    let Some((path, query)) = rest.split_once('?') else { return url.to_string() };
    let kept: Vec<&str> = query.split('&').filter(|kv| !tracking(kv.split('=').next().unwrap_or(""))).collect();
    let mut out = path.to_string();
    if !kept.is_empty() {
        out.push('?');
        out.push_str(&kept.join("&"));
    }
    if url.contains('#') {
        out.push('#');
        out.push_str(frag);
    }
    out
}

/// The refs of one document.
#[derive(Default)]
pub struct Refs {
    doc: usize,
    nodes: Vec<NodeId>,
    ids: HashMap<NodeId, u32>,
    /// What the last snapshot showed, for the `*` marks. `None` before the first snapshot of this document.
    last: Option<Seen>,
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
        let here = self.doc == doc;
        match self.nodes.get((r as usize).wrapping_sub(1)) {
            Some(&node) if here => Ok(Handle { doc, node }),
            _ if r <= self.old || (!here && r as usize <= self.nodes.len()) => Err(format!("e{r} is from an older page (it navigated since): take a new snapshot")),
            _ if here && !self.nodes.is_empty() => Err(format!("e{r} is not a ref of this page: take a snapshot")),
            // No ref was ever minted for this document: refs come from snapshot, find, links and read.
            _ => Err(format!("e{r}: no snapshot or find gave refs on this page yet: take a snapshot")),
        }
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
    "region", "tablist", "menu", "menubar", "tree", "grid", "table", "list", "img", "progressbar", "tabpanel", "log", "iframe",
];
/// Roles whose subtree adds nothing once the line shows their name.
const LEAVES: &[&str] = &[
    "button", "link", "textbox", "searchbox", "checkbox", "radio", "option", "slider", "spinbutton", "switch", "tab", "menuitem",
    "menuitemcheckbox", "menuitemradio", "heading", "img", "progressbar", "iframe",
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
        "iframe" => Some("iframe"),
        "summary" => Some("button"),
        "section" if cx.attr(n, "aria-label").is_some() || cx.attr(n, "aria-labelledby").is_some() => Some("region"),
        "search" => Some("search"),
        _ if cx.attr(n, "contenteditable").is_some_and(|v| v != "false") => Some("textbox"),
        // A focusable element with no role: a custom control (a `div` with a click handler and a tabindex).
        _ if cx.attr(n, "tabindex").and_then(|t| t.trim().parse::<i32>().ok()).is_some_and(|t| t >= 0) => Some("clickable"),
        // A script listens for clicks on it: what a user clicks, though the page gave it no role.
        _ if clicks.contains(&n) || cx.attr(n, "onclick").is_some() => Some("clickable"),
        // The top of a subtree with a pointer cursor: React and others listen on the document root, so the element has no listener of its own.
        _ if pointer(cx, n) && !cx.doc.parent_or_host(n).is_some_and(|p| pointer(cx, p)) => Some("clickable"),
        _ => None,
    }
}

/// Does an element below `n` (in the flat tree) have the role of a control and a name?
/// A control with no name (the `#` anchor icon of a docs heading) does not count: its line would cost tokens and tell nothing.
fn holds_control(cx: &Cx, n: NodeId) -> bool {
    let mut stack: Vec<NodeId> = cx.doc.flat_children(n).collect();
    while let Some(c) = stack.pop() {
        if !matches!(cx.doc.data(c), NodeData::Element { .. }) || !cx.boxed(c) {
            continue;
        }
        if cx.role(c).is_some_and(|r| CONTROLS.contains(&intern(r))) && !cx.name(c).is_empty() {
            return true;
        }
        stack.extend(cx.doc.flat_children(c));
    }
    false
}

fn pointer(cx: &Cx, n: NodeId) -> bool {
    cx.styles.style(n).is_some_and(|s| matches!(s.get(Property::Cursor), Css::Keyword(Keyword::Pointer)))
}

fn inline(cx: &Cx, n: NodeId) -> bool {
    cx.styles.style(n).is_some_and(|s| matches!(s.get(Property::Display), Css::Keyword(Keyword::Inline | Keyword::InlineBlock | Keyword::InlineFlex)))
}

/// The attribute states of a line: what `aria-*` and HTML attributes say. The live states (value, checked, focus) come later.
fn flags(cx: &Cx, n: NodeId, role: &str, href: bool) -> Vec<String> {
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
        && href
        && let Some(h) = a("href")
    {
        f.push(format!("href={}", short(&clean_url(h), 80)));
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
            if name.is_empty() && r == "iframe" {
                name = cx.attr(n, "title").or_else(|| cx.attr(n, "src")).map(norm).unwrap_or_default();
            }
            if name.is_empty() && matches!(r, "alert" | "status" | "log") {
                name = short(&norm(&cx.text(n)), 200);
            }
            // A plain block with no name is noise: its text follows as text lines.
            let skip = matches!(r, "paragraph" | "listitem" | "cell" | "row" | "group" | "article") && name.is_empty();
            if !skip {
                // `links` has the href of each link: a line shows it only when the name cannot tell the link apart.
                out.push(Item::El(Line { depth, node: n, role: r, name: short(&name, 200), flags: flags(cx, n, r, name.is_empty()) }));
                inner = depth + 1;
            }
            // A heading that holds a link (an issue title on GitHub) shows the link: the agent clicks it or takes its URL.
            if LEAVES.contains(&r) && !(r == "heading" && holds_control(cx, n)) {
                continue;
            }
        }
        if block {
            stack.push((n, inner, true));
        }
        // The flat tree: a shadow host shows its shadow tree, and a `<slot>` shows the light children that it takes.
        let mut kids: Vec<NodeId> = cx.doc.flat_children(n).collect();
        // A closed `<details>` shows its summary only.
        if el && cx.tag(n) == "details" && cx.attr(n, "open").is_none() {
            kids.retain(|&c| cx.tag(c) == "summary");
        }
        for &c in kids.iter().rev() {
            stack.push((c, inner, false));
        }
    }
    flush(&mut text, 0, out);
    // A custom control that holds other lines says what it holds in them: its own copy of their text only costs tokens.
    for i in 0..out.len().saturating_sub(1) {
        let d = match &out[i + 1] {
            Item::El(l) => l.depth,
            Item::Text(d, _) => *d,
        };
        let inner = match &out[i + 1] {
            Item::El(l) => l.name.clone(),
            Item::Text(..) => String::new(),
        };
        if let Item::El(l) = &mut out[i]
            && d > l.depth
            && (l.role == "clickable" || (l.role == "heading" && l.name == inner))
        {
            l.name.clear();
        }
    }
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

/// Settle the page before a read: the document loaded, and the tasks that are due now run. It returns as soon as the page is quiet: no
/// task ran, no timer is due within 20 ms and no `fetch` is open. A picture on its way is not work for a read (the 4 ms poll that it
/// asks for is not a reason to wait). A page that waits for a reply (a script that sets a role or fills a list after a `fetch`) gets
/// up to 2 s; otherwise the bound is 300 ms, so a busy page still answers.
pub fn settle(p: &mut Page) {
    let _ = p.wait_load(std::time::Duration::from_secs(10));
    let start = std::time::Instant::now();
    loop {
        let (more, next) = p.tick();
        let now = std::time::Instant::now();
        let (fetching, due) = p.browser().map_or((false, false), |b| (b.pending_fetches() > 0, b.next_scheduled_due().is_some_and(|d| d - b.now() <= 20.0)));
        if (!more && !fetching && !due) || now.duration_since(start) > std::time::Duration::from_millis(if fetching { 2000 } else { 300 }) {
            break;
        }
        // No next time: the page stopped, but a `fetch` is still open (or a timer is due). Wait 5 ms, so the loop does not spin.
        if !more {
            let wait = next.map_or(std::time::Duration::from_millis(5), |t| t.saturating_duration_since(now));
            std::thread::sleep(wait.min(std::time::Duration::from_millis(20)));
        }
    }
}

fn header(p: &mut Page) -> String {
    let url = p.url().unwrap_or_default();
    let title = crate::locate::title(p).unwrap_or_default();
    format!("url: {}\ntitle: {}\n", crate::run::safe_url(&url), norm(&title))
}

/// The first and the last ref of `lines`, as `eA–eB`, or `""` when none has a ref.
fn ref_span(lines: &[String]) -> String {
    let num = |l: &String| l.find("[e").and_then(|i| l[i + 2..].split(']').next()).map(str::to_string);
    match (lines.iter().find_map(num), lines.iter().rev().find_map(num)) {
        (Some(a), Some(b)) => format!(" (e{a}–e{b})"),
        _ => String::new(),
    }
}

/// Keep `max` characters: 80 % from the top and 20 % from the end, cut at line ends. A line says what was cut, with the refs that the
/// cut lines hold (they stay valid). With `page` (snapshot), the lines before `offset` are skipped and the end is not kept: the call
/// pages forward, and the cut line gives the `offset` of the next page.
fn budget(lines: Vec<String>, max: usize, page: Option<usize>) -> String {
    let off = page.unwrap_or(0).min(lines.len());
    let lines = &lines[off..];
    let total: usize = lines.iter().map(|l| l.len() + 1).sum();
    if total <= max {
        return lines.join("\n");
    }
    // The cut line counts too: room for its longest form (all lines cut, the widest refs) comes off `max`.
    let how = |next: usize| match page {
        Some(_) => format!("find or read an element, snapshot with root=REF, or snapshot with offset={next}"),
        None => "pass a larger max_chars".into(),
    };
    let room = max.saturating_sub(format!("\n… {} lines cut{}; use {} …", lines.len(), ref_span(lines), how(off + lines.len())).len());
    let keep = if off > 0 { room } else { room * 4 / 5 };
    let (mut head, mut used) = (Vec::new(), 0);
    for l in lines {
        if used + l.len() + 1 > keep {
            break;
        }
        used += l.len() + 1;
        head.push(l.as_str());
    }
    let (mut tail, mut used_t) = (Vec::new(), 0);
    if off == 0 {
        for l in lines[head.len()..].iter().rev() {
            if used_t + l.len() + 1 > room / 5 {
                break;
            }
            used_t += l.len() + 1;
            tail.push(l.as_str());
        }
        tail.reverse();
    }
    let cut = lines.len() - head.len() - tail.len();
    let span = ref_span(&lines[head.len()..lines.len() - tail.len()]);
    let mut out = format!("{}\n… {cut} lines cut{span}; use {} …", head.join("\n"), how(off + head.len()));
    if !tail.is_empty() {
        out.push('\n');
        out.push_str(&tail.join("\n"));
    }
    out
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
    if let Some(labels) = p.selected_labels(h) {
        return Some(short(&labels.join(", "), 200));
    }
    let v = p.value(h)?;
    if p.is_password(h) {
        return Some(if v.is_empty() { String::new() } else { "••••".into() });
    }
    Some(short(&v, 200))
}

/// One output line, before the marks: its depth, its text (with no indent) and whether the last snapshot did not show it as it is.
struct Row {
    depth: usize,
    text: String,
    changed: bool,
    /// The element of the row; `None` for a text run.
    node: Option<NodeId>,
}

fn indent(d: usize) -> &'static str {
    &"                                                                "[..d.min(64)]
}

impl Row {
    fn render(&self, mark: bool) -> String {
        let m = if mark && self.changed && self.node.is_some() { "*" } else { "" };
        format!("{}{m}{}", indent(self.depth), self.text)
    }
}

/// `seen`: what the last snapshot showed. Text runs are never marked; `diff` keeps them if they are new.
fn format(p: &mut Page, refs: &mut Refs, items: Vec<Item>, seen: Option<&Seen>) -> Vec<Row> {
    let doc = p.generation();
    let controls: Vec<NodeId> = items.iter().filter_map(|i| if let Item::El(l) = i { Some(l).filter(|l| CONTROLS.contains(&l.role)).map(|l| l.node) } else { None }).collect();
    let off = p.offscreen(&controls);
    let mut rows = Vec::with_capacity(items.len());
    for it in items {
        match it {
            Item::Text(depth, t) => {
                let text = format!("{t:?}");
                let changed = seen.is_some_and(|s| !s.text.contains(&hash(&text)));
                rows.push(Row { depth, text, changed, node: None });
            }
            Item::El(mut l) => {
                let r = refs.mint(l.node);
                let h = Handle { doc, node: l.node };
                let value = live(p, h, l.role, &mut l.flags);
                if off.contains(&l.node) {
                    l.flags.push("offscreen".into());
                }
                let mut s = String::with_capacity(48);
                let _ = write!(s, "[e{r}] {}", l.role);
                if !l.name.is_empty() {
                    let _ = write!(s, " {:?}", l.name);
                }
                if let Some(v) = value.filter(|v| !v.is_empty()) {
                    let _ = write!(s, " = {v:?}");
                }
                if !l.flags.is_empty() {
                    let _ = write!(s, " ({})", l.flags.join(", "));
                }
                let changed = seen.is_some_and(|p| p.els.get(&l.node) != Some(&hash(&s)));
                rows.push(Row { depth: l.depth, text: s, changed, node: Some(l.node) });
            }
        }
    }
    rows
}

/// Only the changed rows, each after the rows that hold it (the nearest row of each lower depth before it).
fn diff(rows: &[Row]) -> Vec<String> {
    let mut keep = vec![false; rows.len()];
    for i in (0..rows.len()).filter(|&i| rows[i].changed) {
        keep[i] = true;
        let mut d = rows[i].depth;
        for j in (0..i).rev() {
            if d == 0 {
                break;
            }
            if rows[j].depth < d {
                keep[j] = true;
                d = rows[j].depth;
            }
        }
    }
    rows.iter().zip(keep).filter(|(_, k)| *k).map(|(r, _)| r.render(true)).collect()
}

/// The outline of the page, or of the element `root`, in at most `max` characters. With `diff`, and a snapshot of this document before,
/// only the lines that changed since then (marked `*`) and the lines that hold them.
pub fn snapshot(p: &mut Page, refs: &mut Refs, mode: Mode, root: Option<u32>, offset: usize, max: usize, diff_only: bool) -> Result<String, String> {
    settle(p);
    let doc = p.generation();
    refs.sync(doc);
    let start = root.map(|r| refs.get(r, doc)).transpose()?;
    let clicks = p.browser().map(|b| b.listened(CLICKS)).unwrap_or_default();
    let mut items = Vec::new();
    let mut text = Vec::new();
    p.inspect(|d, frame| {
        let cx = Cx { doc: d, styles: &frame.styles };
        walk(&cx, start.map_or(d.root(), |h| h.node), mode, &clicks, &mut items);
        // The rendered text of the whole page, for the text lines of the next diff. One more walk, without names or roles.
        if root.is_none() {
            walk_text(&cx, d.root(), &mut text);
        }
    })
    .ok_or_else(|| p.errors.last().cloned().unwrap_or_else(|| "the page has no document or no layout yet".into()))?;
    let prev = if root.is_none() { refs.last.take() } else { None };
    let refs_had_prev = prev.is_some();
    let rows = format(p, refs, items, prev.as_ref());
    // The text lines that the last snapshot did not show, in page order, once each. An interactive outline has no text lines, so
    // without these a changed message ("You clicked: Ok") gave `no change`.
    let mut new_text: Vec<&String> = Vec::new();
    if let Some(prev) = prev.as_ref().filter(|_| mode == Mode::Interactive) {
        let mut once = HashSet::new();
        new_text = text.iter().filter(|t| !prev.read.contains(&hash(t.as_str())) && once.insert(hash(t.as_str()))).collect();
    }
    if root.is_none() {
        let mut seen = Seen { read: text.iter().map(|t| hash(t.as_str())).collect(), ..Seen::default() };
        for r in &rows {
            match r.node {
                Some(n) => {
                    seen.els.insert(n, hash(&r.text));
                }
                None => {
                    seen.text.insert(hash(&r.text));
                }
            }
        }
        refs.last = Some(seen);
    }
    if diff_only && root.is_none() && refs_had_prev {
        let changed = rows.iter().filter(|r| r.changed).count();
        let lines = diff(&rows);
        // A text that a changed line already shows as its name adds nothing.
        new_text.retain(|t| !lines.iter().any(|l| l.contains(&format!("{t:?}"))));
        if changed == 0 && new_text.is_empty() {
            return Ok(format!("no change ({} lines)", rows.len()));
        }
        let mut out = if changed == 0 { String::new() } else { format!("{}\n({changed} of {} lines changed)", budget(lines, max, None), rows.len()) };
        for t in new_text.iter().take(TEXT_LINES) {
            let _ = write!(out, "\ntext: {:?}", short(t, TEXT_CHARS));
        }
        if new_text.len() > TEXT_LINES {
            let _ = write!(out, "\n… {} more changed text lines: use read", new_text.len() - TEXT_LINES);
        }
        return Ok(out.trim_start_matches('\n').to_string());
    }
    let lines: Vec<String> = rows.iter().map(|r| r.render(refs_had_prev)).collect();
    let body = if lines.is_empty() { "(no elements: the page is empty, or it has not rendered yet)".into() } else { budget(lines, max, Some(offset)) };
    Ok(header(p) + &body)
}

/// The elements that `loc` matches, as snapshot lines with refs.
/// With `scope`, only the matches inside an element of that CSS selector.
pub fn find(p: &mut Page, refs: &mut Refs, loc: &crate::locate::Locator, scope: Option<&str>, max: usize) -> Result<String, String> {
    settle(p);
    let doc = p.generation();
    refs.sync(doc);
    let mut found = crate::locate::find_in(p, loc, scope)?;
    // The matches with a box of a size first: a field that is in the tree but has no size is not what an agent looks for.
    let (seen, flat): (Vec<Handle>, Vec<Handle>) = found.shown.iter().partition(|&&h| p.visible_box(h).is_ok());
    found.shown = seen.into_iter().chain(flat).collect();
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
            items.push(Item::El(Line { depth: 0, node: h.node, role, name: short(&name, 200), flags: flags(&cx, h.node, role, true) }));
        }
    });
    let n = items.len();
    let mut out = format!("{n} match(es) for {loc}");
    if let Some(sel) = scope {
        let _ = write!(out, " in css={sel:?}");
    }
    if found.hidden > 0 {
        let _ = write!(out, ", {} more not rendered", found.hidden);
    }
    let lines: Vec<String> = format(p, refs, items, None).iter().map(|r| r.render(false)).collect();
    if !lines.is_empty() {
        out.push('\n');
        out.push_str(&budget(lines, max, None));
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
    // The whole reply fits in `max`: the header, the count line and the more line (each with the widest numbers) come off it.
    let head = header(p);
    let room = max.saturating_sub(head.chars().count() + format!("chars {offset}-{total} of {total}\n").chars().count());
    let room = if total.saturating_sub(offset) <= room { room } else { room.saturating_sub(format!("\n… {total} more characters: read again with offset={total}").chars().count()) };
    let page: String = text.chars().skip(offset).take(room).collect();
    // A limit that the header fills gives no text, and the next offset would be the same: say what limit works.
    if page.is_empty() && offset < total {
        return Err(format!("max_chars {max} leaves no room for text after the header: pass at least {}", max - room + 100));
    }
    let end = offset + page.chars().count();
    let more = if end < total { format!("\n… {} more characters: read again with offset={end}", total - end) } else { String::new() };
    Ok(format!("{head}chars {offset}-{end} of {total}\n{page}{more}"))
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
        let kids: Vec<NodeId> = cx.doc.flat_children(n).collect();
        for &c in kids.iter().rev() {
            stack.push((c, false));
        }
    }
    flush(&mut text, out);
}

/// The landmark roles that `links` names.
const LANDMARKS: &[&str] = &["navigation", "banner", "contentinfo", "main", "complementary", "search", "region", "form"];

/// Does `n` sit in an article, aside, main, nav or section? A `header` or `footer` there is no banner or contentinfo.
fn sectioned(cx: &Cx, n: NodeId) -> bool {
    let mut up = cx.doc.parent_or_host(n);
    while let Some(a) = up {
        if matches!(cx.tag(a), "article" | "aside" | "main" | "nav" | "section") {
            return true;
        }
        up = cx.doc.parent_or_host(a);
    }
    false
}

/// The nearest landmark above `n`: its role and its name (empty when it has none). A `form` needs a name to be a landmark.
fn landmark(cx: &Cx, n: NodeId) -> Option<(&'static str, String)> {
    let mut up = cx.doc.parent_or_host(n);
    while let Some(a) = up {
        up = cx.doc.parent_or_host(a);
        let Some(r) = role_of(cx, a, &HashSet::new()).filter(|r| LANDMARKS.contains(r)) else { continue };
        let name = cx.name(a);
        let scoped = matches!(cx.tag(a), "header" | "footer") && cx.attr(a, "role").is_none() && sectioned(cx, a);
        if scoped || (r == "form" && name.is_empty()) {
            continue;
        }
        return Some((r, name));
    }
    None
}

/// The rendered links: ref, text and absolute URL.
/// Filters: `text` (in the text or the URL, any case), `root` (a ref: the links inside it, or the link itself) and `external` (only links to another origin).
pub fn links(p: &mut Page, refs: &mut Refs, max: usize, text: Option<&str>, root: Option<u32>, external: bool) -> Result<String, String> {
    settle(p);
    let doc = p.generation();
    refs.sync(doc);
    let start = root.map(|r| refs.get(r, doc)).transpose()?;
    let needle = text.map(str::to_lowercase);
    let base = p.url().and_then(|u| bare_url::Url::parse(&u).ok());
    let origin = base.as_ref().map(|b| b.to_string()).and_then(|u| u.find("://").map(|i| u[..u[i + 3..].find('/').map_or(u.len(), |j| i + 3 + j)].to_string())).unwrap_or_default();
    let mut found = Vec::new();
    p.inspect(|d, frame| {
        let cx = Cx { doc: d, styles: &frame.styles };
        let mut items = Vec::new();
        walk(&cx, start.map_or(d.root(), |h| h.node), Mode::Interactive, &HashSet::new(), &mut items);
        for it in items {
            if let Item::El(l) = it
                && l.role == "link"
                && let Some(h) = cx.attr(l.node, "href")
            {
                found.push((l.node, l.name, h.to_string(), landmark(&cx, l.node)));
            }
        }
    })
    .ok_or_else(|| p.errors.last().cloned().unwrap_or_else(|| "the page has no document or no layout yet".into()))?;
    let mut lines = Vec::with_capacity(found.len());
    for (n, name, href, lm) in found {
        let abs = base.as_ref().and_then(|b| b.join(&href).ok()).map_or(href, |u| u.to_string());
        // A URL of this origin is the origin, then nothing or a path, a query or a fragment.
        let own = abs.strip_prefix(&origin).is_some_and(|r| r.is_empty() || r.starts_with(['/', '?', '#']));
        if external && (own || !abs.contains("://")) {
            continue;
        }
        if let Some(t) = &needle
            && !name.to_lowercase().contains(t.as_str())
            && !abs.to_lowercase().contains(t.as_str())
        {
            continue;
        }
        // A link of this origin gives its path: the origin is in the header, and it costs tokens in each line. A `text` or `root`
        // filter keeps few lines, so they give the full URL: a model asked for a URL copies it as it is.
        let filtered = needle.is_some() || start.is_some();
        let shown = clean_url(if filtered { &abs } else { abs.strip_prefix(&origin).filter(|r| r.starts_with('/')).unwrap_or(&abs) });
        let name = if name.is_empty() { String::new() } else { format!(" {name:?}") };
        // The nearest landmark, with its name when it has one.
        let lm = lm.map_or(String::new(), |(r, ln)| if ln.is_empty() { format!(" ({r})") } else { format!(" ({r} {:?})", short(&ln, 80)) });
        lines.push(format!("[e{}]{name} -> {shown}{lm}", refs.mint(n)));
    }
    if lines.is_empty() && (text.is_some() || root.is_some() || external) {
        return Ok(format!("{}no link matches the filters", header(p)));
    }
    // The header and the count line count in `max` too.
    let head = format!("{}{} link(s)\n", header(p), lines.len());
    let room = max.saturating_sub(head.len());
    let out = head + &budget(lines, room, None);
    if out.chars().count() > max {
        return Err(format!("max_chars {max} is too small for the header and the cut line: pass at least {}", out.chars().count() + 100));
    }
    Ok(out)
}

/// How big the page is: its DOM nodes, and the snapshot lines of a walk that shows text (none: the page shows no text and no control).
pub fn size(p: &mut Page) -> Option<(usize, usize)> {
    p.inspect(|d, frame| {
        let cx = Cx { doc: d, styles: &frame.styles };
        let mut items = Vec::new();
        walk(&cx, d.root(), Mode::All, &HashSet::new(), &mut items);
        (d.len(), items.len())
    })
}
