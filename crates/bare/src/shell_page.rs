//! What the shell does to a page for its keys: scroll it, list its clickable elements (hints), click one, find text and read the focus.
//! Every call works on the `Page` through its public and crate calls, so the same code runs on a page of a window and on a test page.
//!
//! Gaps: a hint needs a box with a size inside the window and a hit test that gives the element (or a node inside it) at the middle of
//! the visible part, so an element under another one gets no label. A scroll container with the focus keys is not looked at: the key
//! scrolls the box under the middle of the window. Find searches the text of one line piece at a time (a match that crosses a line
//! break or the edge of an element is not found), ignores `text-transform`, and draws its box at the position of the layout (a
//! `letter-spacing` shifts it).

use crate::browser_dom::input_kind;
use crate::browser_scroll::{Align, ScrollKey};
use crate::page::Page;
use bare_html::dom::{Document, NodeData, NodeId};
use bare_layout::font::{self, Metrics};
use bare_layout::fragment::Kind;
use bare_window::{Event, Mods};
use std::collections::HashMap;

/// The most labels that one window gets.
const MAX_HINTS: usize = 400;

/// A clickable element that shows in the window. The box is in window pixels, cut to the window.
#[derive(Debug, Clone, PartialEq)]
pub struct Cand {
    pub node: NodeId,
    pub rect: [f32; 4],
    /// A text field or an editing host: a click gives it the focus.
    pub field: bool,
    /// The `href` of the link that holds the element, as written.
    pub href: Option<String>,
}

fn attr<'a>(doc: &'a Document, n: NodeId, name: &str) -> Option<&'a str> {
    doc.find_atom(name).and_then(|a| doc.attr(n, a))
}

/// What the page takes a click on: the idea of `Page::cursor_over`, with the roles.
fn clickable(doc: &Document, n: NodeId) -> bool {
    if !matches!(doc.data(n), NodeData::Element { .. }) || attr(doc, n, "disabled").is_some() {
        return false;
    }
    let tag = |t| doc.is_html(n, t);
    let role = attr(doc, n, "role").is_some_and(|r| matches!(r.trim().to_ascii_lowercase().as_str(), "button" | "link" | "checkbox" | "tab" | "menuitem"));
    (tag("a") && attr(doc, n, "href").is_some())
        || tag("button")
        || (tag("input") && input_kind(doc, n) != "hidden")
        || tag("select")
        || tag("textarea")
        || tag("summary")
        || role
        || attr(doc, n, "onclick").is_some()
        || attr(doc, n, "tabindex").and_then(|t| t.trim().parse::<i32>().ok()).is_some_and(|t| t >= 0)
        || attr(doc, n, "contenteditable").is_some_and(|v| ["", "true", "plaintext-only"].iter().any(|k| v.eq_ignore_ascii_case(k)))
}

/// The nearest `a` with an `href` at or above `n`.
fn link_of(doc: &Document, n: NodeId) -> Option<String> {
    let mut cur = Some(n);
    while let Some(c) = cur {
        if doc.is_html(c, "a")
            && let Some(h) = attr(doc, c, "href")
        {
            return Some(h.to_string());
        }
        cur = doc.parent(c);
    }
    None
}

/// Is the focus in an editable element (a text field, a textarea, an editing host)?
pub fn editable_focus(p: &mut Page) -> bool {
    let Some(b) = p.browser() else { return false };
    b.active_element().ok().flatten().is_some_and(|n| b.is_text_input(n) || b.is_editing_host(n))
}

/// The clickable elements that show in the window, in tree order (`fields`: only the text fields).
pub fn candidates(p: &mut Page, fields: bool) -> Vec<Cand> {
    let (w, h) = p.viewport();
    let (w, h) = (w as f32, h as f32);
    let mut found: Vec<(NodeId, [f32; 4], Option<String>)> = Vec::new();
    p.scroll_with(|g, s| {
        let mut seen = std::collections::HashSet::new();
        for f in &g.layout.fragments {
            if f.kind != Kind::Box || seen.contains(&f.node) || !clickable(g.doc, f.node) {
                continue;
            }
            let (dx, dy) = g.shift(s, f.node);
            let (x0, y0) = ((f.rect.x + dx).max(0.0), (f.rect.y + dy).max(0.0));
            let (x1, y1) = ((f.rect.x + dx + f.rect.w).min(w), (f.rect.y + dy + f.rect.h).min(h));
            if x1 - x0 < 2.0 || y1 - y0 < 2.0 {
                continue;
            }
            seen.insert(f.node);
            found.push((f.node, [x0, y0, x1 - x0, y1 - y0], link_of(g.doc, f.node)));
        }
        false
    });
    found.truncate(MAX_HINTS * 2);
    let mut out = Vec::new();
    for (node, rect, href) in found {
        let Some(top) = p.hit((rect[0] + rect[2] / 2.0) as f64, (rect[1] + rect[3] / 2.0) as f64) else { continue };
        let Some(b) = p.browser() else { break };
        let doc = b.document();
        let doc = doc.borrow();
        // The element must be the one on top at its middle, or hold it.
        let mut on_top = false;
        let mut cur = Some(top);
        while let Some(c) = cur.filter(|_| !on_top) {
            on_top = c == node;
            cur = doc.parent(c);
        }
        drop(doc);
        let field = b.is_text_input(node) || b.is_editing_host(node);
        if on_top && (field || !fields) {
            out.push(Cand { node, rect, field, href });
        }
        if out.len() == MAX_HINTS {
            break;
        }
    }
    out
}

/// A press and a release of the main button at the middle of `rect`, through the path of a real pointer. Gives whether to draw.
pub fn click(p: &mut Page, rect: [f32; 4]) -> bool {
    let (x, y) = ((rect[0] + rect[2] / 2.0) as f64, (rect[1] + rect[3] / 2.0) as f64);
    let mods = Mods::default();
    let mut redraw = p.handle(Event::PointerMove { x, y, buttons: 0, mods }).redraw;
    for pressed in [true, false] {
        redraw |= p.handle(Event::PointerButton { x, y, button: 0, buttons: pressed as u16, pressed, mods }).redraw;
    }
    redraw
}

/// The absolute http(s) URL of a link, as the page resolves it.
pub fn resolve(p: &mut Page, href: &str) -> Option<String> {
    p.browser()?.resolve_url(href).ok().filter(|u| u.starts_with("http://") || u.starts_with("https://"))
}

/// How the page scrolls.
#[derive(Debug, Clone, Copy)]
pub enum Move {
    By(f32),
    /// Half a window: `1` down, `-1` up.
    Half(i32),
    Top,
    Bottom,
}

/// Scroll the box under the middle of the window (else the page). Gives whether something moved.
pub fn scroll(p: &mut Page, mv: Move) -> bool {
    let (w, h) = p.viewport();
    let target = p.hit(w as f64 / 2.0, h as f64 / 2.0);
    let moved = p.scroll_with(|g, s| match mv {
        Move::By(dy) => g.wheel(s, target, 0.0, dy),
        Move::Half(sign) => g.wheel(s, target, 0.0, sign as f32 * g.view().1 / 2.0),
        Move::Top if target.is_none() => g.key(s, ScrollKey::Home),
        Move::Bottom if target.is_none() => g.key(s, ScrollKey::End),
        Move::Top => g.wheel(s, target, 0.0, -1e9),
        Move::Bottom => g.wheel(s, target, 0.0, 1e9),
    });
    if moved {
        p.send_scroll_events();
    }
    moved
}

/// One match of the text that was searched.
#[derive(Debug, Clone, PartialEq)]
pub struct Found {
    /// The text node, and its place among the text nodes of the document (the order of a `TreeWalker`).
    pub node: NodeId,
    pub index: usize,
    /// UTF-16 offsets in the node text.
    pub from: u32,
    pub to: u32,
    /// The box of the match in layout pixels: x, y, width, height.
    pub rect: [f32; 4],
}

/// The shown text of one line piece: each char with its UTF-16 offsets in the node (`start..end`), a run of white space as one space.
fn shown(text: &str, base: u32) -> Vec<(char, u32, u32)> {
    let mut out: Vec<(char, u32, u32)> = Vec::new();
    let mut at = base;
    for c in text.chars() {
        let end = at + c.len_utf16() as u32;
        if c.is_whitespace() {
            match out.last_mut() {
                Some(l) if l.0 == ' ' => l.2 = end,
                _ => out.push((' ', at, end)),
            }
        } else {
            out.push((c, at, end));
        }
        at = end;
    }
    out
}

fn lower(c: char) -> char {
    c.to_lowercase().next().unwrap_or(c)
}

/// All the matches of `needle` (case-insensitive) in the text that the layout shows, in document order.
pub fn find_all(p: &mut Page, needle: &str) -> Vec<Found> {
    let want: Vec<char> = needle.chars().map(lower).collect();
    let mut out = Vec::new();
    if want.is_empty() {
        return out;
    }
    p.scroll_with(|g, _| {
        // The text nodes in document order.
        let mut order: HashMap<NodeId, usize> = HashMap::new();
        let mut stack = vec![g.doc.root()];
        while let Some(n) = stack.pop() {
            if matches!(g.doc.data(n), NodeData::Text(_)) {
                order.insert(n, order.len());
            }
            stack.extend(g.doc.children(n).collect::<Vec<_>>().into_iter().rev());
        }
        for f in &g.layout.fragments {
            let Kind::Text { start, end } = f.kind else { continue };
            let (Some(&index), NodeData::Text(span)) = (order.get(&f.node), g.doc.data(f.node)) else { continue };
            let full = g.doc.text(*span);
            let Some(piece) = full.get(start as usize..end as usize) else { continue };
            let base = full[..start as usize].encode_utf16().count() as u32;
            let hay = shown(piece, base);
            let metrics = g.doc.parent(f.node).and_then(|e| g.styles.style(e)).map(Metrics::of);
            let mut i = 0;
            while i + want.len() <= hay.len() {
                if hay[i..i + want.len()].iter().zip(&want).all(|(h, w)| lower(h.0) == *w) {
                    let last = i + want.len() - 1;
                    let rect = match &metrics {
                        Some(m) => {
                            let width = |a: usize, b: usize| m.px(font::width(m.face, &hay[a..b].iter().map(|h| h.0).collect::<String>()));
                            [f.rect.x + width(0, i), f.rect.y, width(i, last + 1), f.rect.h]
                        }
                        None => [f.rect.x, f.rect.y, f.rect.w, f.rect.h],
                    };
                    out.push(Found { node: f.node, index, from: hay[i].1, to: hay[last].2, rect });
                    i += want.len();
                } else {
                    i += 1;
                }
            }
        }
        false
    });
    out
}

/// The box of a match in window pixels now (the scroll moved it).
pub fn found_rect(p: &mut Page, m: &Found) -> Option<[f32; 4]> {
    let mut out = None;
    p.scroll_with(|g, s| {
        let (dx, dy) = g.shift(s, m.node);
        out = Some([m.rect[0] + dx, m.rect[1] + dy, m.rect[2], m.rect[3]]);
        false
    });
    out
}

/// Select the match as the document selection, and scroll it into view.
pub fn select(p: &mut Page, m: &Found) {
    let src = format!("(() => {{ const w = document.createTreeWalker(document, 4); let n = null; for (let i = 0; i <= {}; i++) n = w.nextNode(); if (n) getSelection().setBaseAndExtent(n, {}, n, {}); }})()", m.index, m.from, m.to);
    if let Some(b) = p.browser() {
        let _ = b.run_script(&src);
    }
    p.touched();
    // A text node may have no box of its own: the element around it has.
    let moved = p.scroll_with(|g, s| g.reveal(s, m.node, Align::IfNeeded, Align::IfNeeded) || g.doc.parent(m.node).is_some_and(|e| g.reveal(s, e, Align::IfNeeded, Align::IfNeeded)));
    if moved {
        p.send_scroll_events();
    }
}

/// The first match at or below the top of the window; the first of all when none is.
pub fn first_after_top(p: &mut Page, found: &[Found]) -> usize {
    for (i, m) in found.iter().enumerate() {
        if found_rect(p, m).is_some_and(|r| r[1] >= 0.0) {
            return i;
        }
    }
    0
}
