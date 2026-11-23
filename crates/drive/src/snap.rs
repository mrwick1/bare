//! What an agent reads of a live page, as text: an outline of the accessibility tree with refs (`snapshot`), the elements that match a
//! query (`find`), the rendered text (`read`) and the links (`links`). One walk of the DOM with the cascade of the last frame: no layout
//! query per element, so a snapshot of a large page costs a few milliseconds.
//!
//! A ref (`e7`) names an element of the document of now. Its number comes from one counter of the server, so no other document or session
//! gets it: a ref of an older page or of another session fails, and never names another element. It stays the same for the same element
//! across snapshots of one document, and fails as stale after a navigation. A document has an opaque id (`d3`) and a revision (the number
//! of full-page snapshots of it; a revision is a count of observations, not proof that the page was the same between two of them).
//! A diff also lists the refs that left the outline (`removed:`). `*` marks an element that the last snapshot of this document did not show, or showed with another
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
use std::sync::atomic::{AtomicU32, Ordering::Relaxed};

/// The next ref number and the next document id of this process: they never repeat, so a ref or a document id of one session is not
/// one of another, and a ref of an older document is not minted again.
static NEXT_REF: AtomicU32 = AtomicU32::new(1);
static NEXT_DOC: AtomicU32 = AtomicU32::new(1);

fn hash<T: Hash + ?Sized>(t: &T) -> u64 {
    let mut h = DefaultHasher::new();
    t.hash(&mut h);
    h.finish()
}

/// What a snapshot showed: each element with a hash of its line, and the hashes of the text lines. `read` holds a hash of each line
/// of the rendered text, so an interactive diff can name the text that changed.
#[derive(Default)]
struct Seen {
    /// The mode of that snapshot: an outline of another mode shows other rows, so it is no baseline for this one.
    mode: Mode,
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

/// The refs of one session: those of the document of now, and the range of those of older documents (so an old ref fails as stale).
#[derive(Default)]
pub struct Refs {
    /// The generation of the page that the refs belong to.
    generation: Option<usize>,
    /// The opaque id of that document (`dN`), and the number of full-page snapshots of it.
    doc: u32,
    rev: u32,
    /// The id of the document that this one replaced, until its first snapshot.
    replaced: Option<u32>,
    nodes: HashMap<u32, NodeId>,
    ids: HashMap<NodeId, u32>,
    /// The first and the last ref of this document.
    span: Option<(u32, u32)>,
    /// What the last snapshot showed, for the `*` marks. `None` before the first snapshot of this document.
    last: Option<Seen>,
    /// The first and the last ref of the older documents of the session. Other sessions may have refs inside it: they fail too.
    old: Option<(u32, u32)>,
}

impl Refs {
    fn sync(&mut self, generation: usize) {
        if self.generation != Some(generation) {
            let old = match (self.old, self.span) {
                (Some((a, b)), Some((c, d))) => Some((a.min(c), b.max(d))),
                (a, b) => a.or(b),
            };
            let replaced = self.generation.map(|_| self.doc);
            *self = Refs { generation: Some(generation), doc: NEXT_DOC.fetch_add(1, Relaxed), replaced, old, ..Refs::default() };
        }
    }

    fn mint(&mut self, p: &mut Page, n: NodeId) -> u32 {
        p.pin(n);
        *self.ids.entry(n).or_insert_with(|| {
            let r = NEXT_REF.fetch_add(1, Relaxed);
            self.nodes.insert(r, n);
            self.span = Some((self.span.map_or(r, |s| s.0), r));
            r
        })
    }

    /// The line that names the document and its revision.
    fn doc_line(&self) -> String {
        format!("doc: d{} rev {}", self.doc, self.rev)
    }

    /// The element of ref `r`, or why there is none.
    pub fn get(&self, r: u32, generation: usize) -> Result<Handle, String> {
        let here = self.generation == Some(generation);
        if here && let Some(&node) = self.nodes.get(&r) {
            return Ok(Handle { doc: generation, node });
        }
        let inside = |s: Option<(u32, u32)>| s.is_some_and(|(a, b)| (a..=b).contains(&r));
        // Before the sync of a new document, the refs of the old one are still here: they are old.
        if inside(self.old) || (!here && inside(self.span)) {
            Err(format!("e{r} is from an older page (it navigated since), or not from this session: take a new snapshot"))
        } else if here && !self.nodes.is_empty() {
            Err(format!("e{r} is not a ref of this page (it may come from another session): take a snapshot"))
        } else {
            // No ref was ever minted for this document: refs come from snapshot, find, links and read.
            Err(format!("e{r}: no snapshot or find gave refs on this page yet: take a snapshot"))
        }
    }
}

#[derive(Clone, Copy, PartialEq, Default)]
pub enum Mode {
    /// Controls, headings, landmarks, dialogs and alerts.
    #[default]
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
    /// A table row of plain cells: the parts of each cell. Empty for other lines.
    cells: Vec<Vec<Part>>,
}

/// A part of a cell: its text, or an element that has its own line (the cell shows its ref).
enum Part {
    Text(String),
    El(NodeId),
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

fn vis_hidden(cx: &Cx, n: NodeId) -> bool {
    matches!(cx.styles.style(n).map(|s| s.get(Property::Visibility)), Some(Css::Keyword(Keyword::Hidden | Keyword::Collapse)))
}

/// The most options that a select line lists: a country list has about 250, and `act select` takes any label.
const OPTIONS: usize = 25;

/// The option labels of a `<select>` (also those in an `<optgroup>`), as one flag: `options: "a" | "b"`. `act select` takes a label.
/// The options have no box, so the walk does not show them.
fn select_options(cx: &Cx, n: NodeId, f: &mut Vec<String>) {
    let mut labels = Vec::new();
    let mut count = 0;
    for c in cx.doc.children(n) {
        let group: Vec<NodeId> = if cx.tag(c) == "optgroup" { cx.doc.children(c).collect() } else { vec![c] };
        for o in group.into_iter().filter(|&o| cx.tag(o) == "option") {
            count += 1;
            if labels.len() < OPTIONS {
                let l = cx.attr(o, "label").map(norm).filter(|l| !l.is_empty()).unwrap_or_else(|| norm(&cx.text(o)));
                labels.push(format!("{:?}", short(&l, 80)));
            }
        }
    }
    if labels.is_empty() {
        return;
    }
    let more = if count > labels.len() { format!(" | … {} more", count - labels.len()) } else { String::new() };
    f.push(format!("options: {}{more}", labels.join(" | ")));
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
    if cx.tag(n) == "input" && matches!(cx.input_type(n).as_str(), "email" | "password" | "tel" | "url" | "number" | "date" | "search" | "time" | "datetime-local" | "month" | "week") {
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
                // `visibility: hidden` inherits, and a child may set `visible` again: the parent's style says if the text shows.
                if mode == Mode::All && !cx.doc.flat_parent(n).is_some_and(|p| vis_hidden(cx, p)) {
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
        let shown = el && !vis_hidden(cx, n);
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
            // An empty cell stays: a row line shows it, so the cells keep their columns.
            let skip = matches!(r, "paragraph" | "listitem" | "row" | "group" | "article") && name.is_empty();
            if !skip {
                // `links` has the href of each link: a line shows it only when the name cannot tell the link apart.
                let mut f = flags(cx, n, r, name.is_empty());
                if cx.tag(n) == "select" {
                    select_options(cx, n, &mut f);
                }
                out.push(Item::El(Line { depth, node: n, role: r, name: short(&name, 200), flags: f, cells: Vec::new() }));
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
    rows(out);
    // A custom control that holds other lines says what it holds in them: its own copy of their text only costs tokens.
    for i in 0..out.len().saturating_sub(1) {
        let d = match &out[i + 1] {
            Item::El(l) => l.depth,
            Item::Text(d, _) => *d,
        };
        let (inner, text) = match &out[i + 1] {
            Item::El(l) => (l.name.clone(), None),
            Item::Text(_, t) => (String::new(), Some(t.clone())),
        };
        // A row or a cell takes its name from its text: the lines below it say that text. A name of `aria-label` stays. A cell that
        // holds only its text keeps the name, and the text line goes (below).
        let from_text = |l: &Line| matches!(l.role, "row" | "cell" | "columnheader" | "rowheader") && cx.attr(l.node, "aria-label").is_none() && cx.attr(l.node, "aria-labelledby").is_none() && text.as_ref() != Some(&l.name);
        if let Item::El(l) = &mut out[i]
            && d > l.depth
            && (l.role == "clickable" || (l.role == "heading" && l.name == inner) || from_text(l))
        {
            l.name.clear();
        }
    }
    // A `clickable` with no name, no state and no line below it tells the agent nothing.
    let at = |i: usize| match &out[i] {
        Item::El(l) => l.depth,
        Item::Text(d, _) => *d,
    };
    let blank: Vec<bool> = (0..out.len()).map(|i| matches!(&out[i], Item::El(l) if l.role == "clickable" && l.name.is_empty() && l.flags.is_empty() && (i + 1 == out.len() || at(i + 1) <= l.depth))).collect();
    // A text run that only repeats the name of the element next to it (the text of a `<label>`) adds nothing.
    let names: Vec<Option<String>> = out.iter().map(|i| if let Item::El(l) = i { Some(l.name.clone()) } else { None }).collect();
    let mut i = 0;
    out.retain(|it| {
        let k: usize = i;
        i += 1;
        if blank[k] {
            return false;
        }
        let Item::Text(_, t) = it else { return true };
        ![k.wrapping_sub(1), k + 1].iter().any(|&j| names.get(j).and_then(|n| n.as_deref()) == Some(t.as_str()))
    });
}

/// Make each table row of plain cells one line: `row: 1 | [e3] | $3.99`. A cell shows its text and the refs of the elements in it;
/// those elements follow on their own lines, one level up. The row and column relation stays, and the text of a cell is said once. A
/// row that holds another row (a layout table) keeps its lines.
fn rows(out: &mut Vec<Item>) {
    let depth = |it: &Item| match it {
        Item::El(l) => l.depth,
        Item::Text(d, _) => *d,
    };
    let mut items = std::mem::take(out).into_iter().peekable();
    while let Some(mut it) = items.next() {
        let Item::El(row) = &mut it else {
            out.push(it);
            continue;
        };
        if row.role != "row" {
            out.push(it);
            continue;
        }
        let d = row.depth;
        let mut sub = Vec::new();
        while let Some(x) = items.next_if(|x| depth(x) > d) {
            sub.push(x);
        }
        let cell = |x: &Item| matches!(x, Item::El(l) if l.depth == d + 1 && matches!(l.role, "cell" | "columnheader" | "rowheader"));
        let plain = sub.first().is_some_and(cell) && sub.iter().all(|x| depth(x) > d + 1 || cell(x)) && !sub.iter().any(|x| matches!(x, Item::El(l) if l.role == "row"));
        if !plain {
            out.push(it);
            out.extend(sub);
            continue;
        }
        row.name.clear();
        let (mut kept, mut fresh) = (Vec::new(), false);
        for x in sub {
            match x {
                // The name of a cell stands for it until its first part: a cell with no line below it (a name of `aria-label`) shows its name.
                Item::El(c) if c.depth == d + 1 => {
                    row.cells.push(if c.name.is_empty() { Vec::new() } else { vec![Part::Text(c.name)] });
                    fresh = true;
                }
                x => {
                    let Some(parts) = row.cells.last_mut() else { continue };
                    if std::mem::take(&mut fresh) {
                        parts.clear();
                    }
                    match x {
                        Item::Text(_, t) => parts.push(Part::Text(t)),
                        Item::El(mut l) => {
                            parts.push(Part::El(l.node));
                            l.depth -= 1;
                            kept.push(Item::El(l));
                        }
                    }
                }
            }
        }
        out.push(it);
        out.extend(kept);
    }
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
        // Loader work that can run now, or a task due within 20 ms, holds the settle. A quiet loop (an animation frame loop or an
        // interval that changes nothing) does not: the engine's settle rule skips it, so a tick that ran it is no reason to go on.
        let (fetching, due) = p.browser().map_or((false, more), |b| (b.pending_fetches() > 0, b.live_work() == Some(true) || b.blocks_settle(b.now() + 20.0)));
        if (!fetching && !due) || crate::run::cancelled() || now.duration_since(start) > std::time::Duration::from_millis(if fetching { 2000 } else { 300 }) {
            break;
        }
        // No next time: the page stopped, but a `fetch` is still open (or a timer is due). Wait 5 ms, so the loop does not spin.
        if !more {
            let wait = next.map_or(std::time::Duration::from_millis(5), |t| t.saturating_duration_since(now));
            std::thread::sleep(wait.min(std::time::Duration::from_millis(20)));
        }
    }
}

fn header(p: &mut Page, refs: &Refs) -> String {
    let url = p.url().unwrap_or_default();
    let title = crate::locate::title(p).unwrap_or_default();
    format!("url: {}\ntitle: {}\n{}\n", crate::run::safe_url(&url), norm(&title), refs.doc_line())
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
/// pages forward, and the cut line gives the `offset` of the next page. It also gives, for each input line, whether the text shows it.
fn budget(lines: Vec<String>, max: usize, page: Option<usize>) -> (String, Vec<bool>) {
    let all = lines.len();
    let off = page.unwrap_or(0).min(all);
    let mut shown = vec![false; all];
    let lines = &lines[off..];
    let total: usize = lines.iter().map(|l| l.len() + 1).sum();
    if total <= max {
        shown[off..].fill(true);
        return (lines.join("\n"), shown);
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
    shown[off..off + head.len()].fill(true);
    shown[all - tail.len()..].fill(true);
    let mut out = format!("{}\n… {cut} lines cut{span}; use {} …", head.join("\n"), how(off + head.len()));
    if !tail.is_empty() {
        out.push('\n');
        out.push_str(&tail.join("\n"));
    }
    (out, shown)
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
    let controls: Vec<NodeId> = items.iter().filter_map(|i| if let Item::El(l) = i { Some(l).filter(|l| CONTROLS.contains(&l.role) && l.role != "link").map(|l| l.node) } else { None }).collect();
    // A link below the fold needs no state: `act` scrolls to it, and a page has many such links (85 on github.com, 12 % of its outline).
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
                let h = Handle { doc, node: l.node };
                let value = live(p, h, l.role, &mut l.flags);
                if off.contains(&l.node) {
                    l.flags.push("offscreen".into());
                }
                let mut s = String::with_capacity(48);
                // A plain table row has no ref: an agent acts on its cells' elements, which have their own.
                if l.cells.is_empty() {
                    let _ = write!(s, "[e{}] ", refs.mint(p, l.node));
                }
                s.push_str(l.role);
                for (k, parts) in l.cells.iter().enumerate() {
                    s.push_str(if k == 0 { ":" } else { " |" });
                    for part in parts {
                        let _ = match part {
                            Part::Text(t) => cell_text(&mut s, t),
                            Part::El(n) => write!(s, " [e{}]", refs.mint(p, *n)),
                        };
                    }
                }
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

/// Only the changed rows, each after the rows that hold it (the nearest row of each lower depth before it): the index of each row and its line.
fn diff(rows: &[Row]) -> (Vec<usize>, Vec<String>) {
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
    let idx: Vec<usize> = (0..rows.len()).filter(|&i| keep[i]).collect();
    let lines = idx.iter().map(|&i| rows[i].render(true)).collect();
    (idx, lines)
}

/// The most rows of one run of new sibling elements with the same role that a diff lists. One line counts the others.
/// A changed element that the last snapshot showed is always listed.
const RUN: usize = 10;

/// The text of a table cell, bare: `| $3.99 |`. Text that could read as a cell border, a ref or a quote, or that is empty, is quoted.
fn cell_text(s: &mut String, t: &str) -> std::fmt::Result {
    if t.is_empty() || t.contains(['|', '[', '"', '\\']) || t.trim() != t || t.chars().any(char::is_control) { write!(s, " {t:?}") } else { write!(s, " {t}") }
}

/// The role word of a row: `option` in `[e4] option "x"`, `row` in `row: 1 | 2`.
fn row_role(r: &Row) -> Option<&str> {
    r.node?;
    let t = if r.text.starts_with("[e") { r.text.split_once("] ")?.1 } else { r.text.as_str() };
    t.split([' ', ':']).next()
}

/// Cut each run of more than `RUN` new sibling rows with the same role (the options of a dropdown that opened) to its first `RUN`
/// rows and one line that counts the rest. `new` says that a row is an element that the last snapshot did not show. Gives, for each
/// line, the rows that it stands for.
fn collapse(rows: &[Row], idx: Vec<usize>, lines: Vec<String>, new: impl Fn(&Row) -> bool) -> (Vec<Vec<usize>>, Vec<String>) {
    let (mut groups, mut out) = (Vec::new(), Vec::new());
    let mut k = 0;
    while k < idx.len() {
        let first = &rows[idx[k]];
        let mut end = k + 1;
        if new(first) && row_role(first).is_some() {
            while end < idx.len() && new(&rows[idx[end]]) && rows[idx[end]].depth == first.depth && row_role(&rows[idx[end]]) == row_role(first) {
                end += 1;
            }
        }
        let keep = if end - k > RUN { k + RUN } else { end };
        for j in k..keep {
            groups.push(vec![idx[j]]);
            out.push(lines[j].clone());
        }
        if keep < end {
            let rest: Vec<usize> = idx[keep..end].to_vec();
            // A plain table row has no ref, so a run of them has no span.
            let span = |i: usize| rows[i].text.strip_prefix('[').and_then(|t| t.split_once(']')).map(|(a, _)| a.to_string());
            let range = span(rest[0]).zip(span(rest[rest.len() - 1])).map_or(String::new(), |(a, b)| format!(" ({a}–{b})"));
            let parent = out.iter().rev().find(|l| l.len() - l.trim_start().len() < first.depth).and_then(|l| l.split_once("[e")).map(|(_, t)| t.split(']').next().unwrap_or(""));
            let how = parent.map_or("use find".to_string(), |e| format!("use find, or snapshot with root=e{e}"));
            out.push(format!("{} … {} more {} rows{range}; {how}", indent(first.depth), rest.len(), row_role(first).unwrap_or("")));
            groups.push(rest);
        }
        k = end;
    }
    (groups, out)
}

/// The most refs that a diff lists as removed. The others stay in the baseline, so the next diff lists them.
const REMOVED: usize = 20;

/// The outline of the page, or of the element `root`, in at most `max` characters. With `diff`, and a snapshot of this document before,
/// only the lines that changed since then (marked `*`), the lines that hold them, and the refs that left the outline (`removed:`).
///
/// The baseline of the next diff moves only over what this reply delivered: a row that `offset` skipped or the cut hid keeps its old hash
/// (or none), so it shows as changed next time. A snapshot with `root` leaves the baseline alone. A diff with no baseline (a new document,
/// another mode) gives the full outline and says `reset:` and why.
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
    let full = root.is_none();
    if full {
        refs.rev += 1;
    }
    let mut prev = if full { refs.last.take() } else { None };
    let mut reset = None;
    if diff_only && full {
        reset = match &prev {
            None => Some(match refs.replaced.filter(|_| refs.rev == 1) {
                Some(old) => format!("new document d{} replaces d{old}", refs.doc),
                None => "no earlier snapshot of this document".to_string(),
            }),
            Some(s) if s.mode != mode => Some("mode changed since the last snapshot".to_string()),
            _ => None,
        };
    }
    if prev.as_ref().is_some_and(|s| s.mode != mode) {
        prev = None;
    }
    let refs_had_prev = prev.is_some();
    let rows = format(p, refs, items, prev.as_ref());
    // The text lines that the last snapshot did not show, in page order, once each. An interactive outline has no text lines, so
    // without these a changed message ("You clicked: Ok") gave `no change`.
    let mut new_text: Vec<&String> = Vec::new();
    if let Some(prev) = prev.as_ref().filter(|_| mode == Mode::Interactive) {
        let mut once = HashSet::new();
        new_text = text.iter().filter(|t| !prev.read.contains(&hash(t.as_str())) && once.insert(hash(t.as_str()))).collect();
    }
    // The elements that the last snapshot showed and this outline does not: gone from the page, or hidden.
    let mut removed: Vec<(u32, NodeId)> = Vec::new();
    if let Some(prev) = prev.as_ref().filter(|_| full) {
        let now: HashSet<NodeId> = rows.iter().filter_map(|r| r.node).collect();
        removed = prev.els.keys().filter(|n| !now.contains(n)).filter_map(|n| refs.ids.get(n).map(|&r| (r, *n))).collect();
        removed.sort_unstable_by_key(|&(r, _)| r);
    }
    let mut pending_text = HashSet::new();
    let mut delivered = vec![true; rows.len()];
    let mut listed = removed.len().min(REMOVED);
    let out = if diff_only && full && refs_had_prev {
        let changed = rows.iter().filter(|r| r.changed).count();
        let (idx, lines) = diff(&rows);
        let (groups, lines) = collapse(&rows, idx, lines, |r| r.changed && r.node.is_some_and(|n| prev.as_ref().is_some_and(|s| !s.els.contains_key(&n))));
        // A text that a changed line already shows as its name adds nothing.
        new_text.retain(|t| !lines.iter().any(|l| l.contains(&format!("{t:?}"))));
        // Nor does the name of a row that a summary line counts.
        let summed: HashSet<&str> = groups.iter().filter(|g| g.len() > 1).flatten().filter_map(|&i| rows[i].text.split_once(" \"").map(|(_, n)| n.split('"').next().unwrap_or(""))).collect();
        new_text.retain(|t| !summed.contains(t.as_str()));
        if changed == 0 && new_text.is_empty() && removed.is_empty() {
            format!("no change ({} lines)\n{}", rows.len(), refs.doc_line())
        } else {
            let mut out = String::new();
            if changed > 0 {
                let (body, shown) = budget(lines, max, None);
                // The rows that a summary line counts are delivered with it: the next diff does not list them again.
                for (k, g) in groups.iter().enumerate() {
                    for &i in g {
                        delivered[i] = shown[k] || !rows[i].changed;
                    }
                }
                out = format!("{body}\n({changed} of {} lines changed)", rows.len());
            }
            if !removed.is_empty() {
                let refs_list: Vec<String> = removed[..listed].iter().map(|(r, _)| format!("e{r}")).collect();
                let _ = write!(out, "\nremoved: {}", refs_list.join(", "));
                if removed.len() > listed {
                    let _ = write!(out, " … {} more removed: the next diff lists them", removed.len() - listed);
                }
            }
            for t in new_text.iter().take(TEXT_LINES) {
                let _ = write!(out, "\ntext: {:?}", short(t, TEXT_CHARS));
            }
            if new_text.len() > TEXT_LINES {
                pending_text.extend(new_text.iter().skip(TEXT_LINES).map(|t| hash(t.as_str())));
                let _ = write!(out, "\n… {} more changed text lines: use read", new_text.len() - TEXT_LINES);
            }
            format!("{}\n{}", out.trim_start_matches('\n'), refs.doc_line())
        }
    } else {
        listed = 0;
        let lines: Vec<String> = rows.iter().map(|r| r.render(refs_had_prev)).collect();
        let body = if lines.is_empty() {
            "(no elements: the page is empty, or it has not rendered yet)".into()
        } else {
            let (body, shown) = budget(lines, max, Some(offset));
            // The rows that `offset` skipped or the cut hid were not delivered. Only a complete outline accounts for removals; a partial one retains them.
            delivered = shown;
            body
        };
        if offset == 0 && delivered.iter().all(|shown| *shown) {
            listed = removed.len();
        }
        let why = reset.map_or(String::new(), |w| format!("reset: {w}; this is the full outline\n"));
        header(p, refs) + &why + &body
    };
    if full {
        let mut seen = Seen { mode, read: text.iter().map(|t| hash(t.as_str())).filter(|h| !pending_text.contains(h)).collect(), ..Seen::default() };
        for (r, ok) in rows.iter().zip(&delivered) {
            let h = hash(&r.text);
            match r.node {
                Some(n) => {
                    let old = prev.as_ref().and_then(|s| s.els.get(&n)).copied();
                    if let Some(h) = if *ok { Some(h) } else { old } {
                        seen.els.insert(n, h);
                    }
                }
                None if *ok || prev.as_ref().is_some_and(|s| s.text.contains(&h)) => {
                    seen.text.insert(h);
                }
                None => {}
            }
        }
        // A removal that this reply did not list stays in the baseline.
        if let Some(prev) = &prev {
            for &(_, n) in removed.iter().skip(listed) {
                seen.els.insert(n, prev.els[&n]);
            }
        }
        refs.last = Some(seen);
    }
    Ok(out)
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
            items.push(Item::El(Line { depth: 0, node: h.node, role, name: short(&name, 200), flags: flags(&cx, h.node, role, true), cells: Vec::new() }));
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
        out.push_str(&budget(lines, max, None).0);
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
    let head = header(p, refs);
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
        lines.push(format!("[e{}]{name} -> {shown}{lm}", refs.mint(p, n)));
    }
    if lines.is_empty() && (text.is_some() || root.is_some() || external) {
        return Ok(format!("{}no link matches the filters", header(p, refs)));
    }
    // The header and the count line count in `max` too.
    let head = format!("{}{} link(s)\n", header(p, refs), lines.len());
    let room = max.saturating_sub(head.len());
    let out = head + &budget(lines, room, None).0;
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A `Refs` that gave `n` refs to document `generation`, without a page.
    fn refs_with(generation: usize, n: u32) -> Refs {
        let mut r = Refs::default();
        r.sync(generation);
        let node = bare_html::dom::Document::new().root();
        for _ in 0..n {
            let id = NEXT_REF.fetch_add(1, Relaxed);
            r.nodes.insert(id, node);
            r.span = Some((r.span.map_or(id, |s| s.0), id));
        }
        r
    }

    #[test]
    fn two_sessions_never_share_a_ref_and_a_new_document_refuses_the_old_ones() {
        let (mut a, b) = (refs_with(1, 2), refs_with(1, 2));
        let (ra, rb) = (*a.nodes.keys().min().unwrap(), *b.nodes.keys().min().unwrap());
        assert_ne!(ra, rb);
        assert!(a.get(ra, 1).is_ok() && b.get(ra, 1).is_err() && a.get(rb, 1).is_err());
        let doc = a.doc;
        // Before the sync of the new document, and after it, the old ref is old, and the new document has other ids and refs.
        assert!(a.get(ra, 2).unwrap_err().contains("older page"));
        a.sync(2);
        assert_ne!(a.doc, doc);
        assert!(a.get(ra, 2).unwrap_err().contains("older page"));
    }
}
