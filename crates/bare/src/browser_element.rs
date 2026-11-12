//! `HTMLElement` members: `hidden`, `inert`, `tabIndex` (also on `SVGElement`), `click()`, `offsetParent`, `offsetTop`, `offsetLeft` and `innerText`.
//! Sources: HTML Standard ("The hidden attribute", "inert", "tabindex", "innerText", "rendered text collection steps", `click()`), CSSOM View
//! (`offsetParent`, `offsetTop`, `offsetLeft`). Read by hand, then compared with headless Chrome: `tools/fixtures/ui/element-cases.txt`.
//!
//! What is built
//! - `hidden`: a boolean, or the string `until-found` when the attribute is that word (ASCII case-insensitive). The setter writes `until-found`
//!   for that string, else `""` for a true value and removes the attribute for a false one. `inert`: the attribute is there or not.
//! - `tabIndex`: the attribute as a "valid integer" (leading white space, a sign, digits; the rest is ignored; out of `i32` is the default).
//!   The default is 0 for `a area button input select textarea iframe object video audio`, the first `summary` of a `details`, and an SVG `a`;
//!   else -1 (like Chrome: `contenteditable` is -1, not 0). The setter writes `ToInt32` in decimal.
//! - `click()`: `HTMLElement` only. A disabled control does nothing. A click that is running on the same element is ignored. Else one `click`
//!   event (`isTrusted` false, bubbles, cancelable, composed) goes to the element, and a submit button submits its form (`browser_form.rs`).
//! - `offsetParent`, `offsetTop`, `offsetLeft`: from native layout (the same path as `offsetWidth`, see `browser_geometry.rs`). No box (detached,
//!   `display:none` or below it) is `null` and 0. `offsetParent` is `null` for `html`, `body` and `position:fixed`, else the nearest ancestor that is
//!   positioned, or `td th table` when the element is static, or `body`. The offsets are of the border box: against the page origin when the parent is
//!   `null` or `body` (scroll is ignored, as layout is), else against the padding edge of the parent. Whole pixels, halves up, like `offsetWidth`.
//! - `innerText` getter: the "rendered text collection steps" over the native cascade: `display:none` is skipped; `visibility` hides the text of the
//!   element, not of the children; `white-space` (`normal nowrap pre pre-line pre-wrap break-spaces`) and `text-transform` apply; `br` is a line feed;
//!   a block-level box (also a flex item, a float, an absolute box) gives one required line break at each side, `p` gives two; a table cell that is
//!   not last in its row gives a tab, a row that is not last in its table a line feed; runs of breaks are one run of the largest; breaks at the
//!   start and the end are dropped. An element with no box, or a detached one, gives `textContent`. Setter: the text, split at `\r\n`, `\r` and `\n`,
//!   into text nodes with a `<br>` between; nothing is parsed.
//!
//! Gates (`Error::Unsupported`, none is faked)
//! - `click()` when the activation target is a link (`a`/`area` with `href`), a `label`, a `summary` or a checkbox or radio: their default action
//!   is built only for the window (`browser_ui.rs`). The gate comes before any event.
//! - `innerText` over a `select` or `textarea` that is shown. `offsetTop` and `offsetLeft` when the element or its offset parent has no fragment
//!   (a culled inline box) or more than one (an inline box that wraps). A shadow tree that is connected, as in `browser_geometry.rs`.
//!
//! Deviations (on purpose)
//! - `click()` sends a plain `Event`, as the driver does (there is no `MouseEvent`, see `browser_form.rs`).
//! - `innerText` does not know soft line wraps, so the space at the end of a wrapped line stays; `text-transform: capitalize` looks at one text node
//!   at a time; `display:contents` is not modelled. An atomic inline (`img`, `inline-block`) counts as text on the line, so the white space around it stays.

use crate::browser::*;
use crate::browser_dom as nd;
use bare_css::values::{Keyword, Property, Value as Cv};
use bare_html::dom::{Document, NodeData, NodeId};
use bare_js::eval::Interp;
use bare_js::value::{Value, to_int32};

type V = R<Value>;

fn arg(a: &[Value], i: usize) -> Value {
    a.get(i).copied().unwrap_or(Value::Undefined)
}

/// The local name of an element of any namespace, `""` for other nodes.
fn name(doc: &Document, n: NodeId) -> &str {
    match *doc.data(n) {
        NodeData::Element { name, .. } => doc.atom_str(name),
        _ => "",
    }
}

/// The name of an HTML element, `""` for other nodes and for SVG and MathML.
fn html_name(doc: &Document, n: NodeId) -> &str {
    if doc.is_foreign(n) { "" } else { name(doc, n) }
}

fn attr<'a>(doc: &'a Document, n: NodeId, a: &str) -> Option<&'a str> {
    doc.find_atom(a).and_then(|a| doc.attr(n, a))
}

fn kw(v: Cv, k: Keyword) -> bool {
    v == Cv::Keyword(k)
}

// ---------------------------------------------------------------------------------------------- hidden, inert, tabIndex

fn hidden(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let n = d.style_html_el(it, this)?;
    let found = attr(&d.doc.borrow(), n, "hidden").map(|s| s.eq_ignore_ascii_case("until-found"));
    match found {
        Some(true) => d.js_str(it, "until-found"),
        found => Ok(Value::Bool(found.is_some())),
    }
}

fn set_hidden(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let n = d.style_html_el(it, this)?;
    let v = arg(a, 0);
    if matches!(v, Value::String(_)) && d.string_arg(it, v)?.eq_ignore_ascii_case("until-found") {
        nd::attr_set(d, it, n, "hidden", "until-found")?;
    } else if it.to_js_bool(v)? {
        nd::attr_set(d, it, n, "hidden", "")?;
    } else {
        d.doc.borrow_mut().remove_attr(n, "hidden");
    }
    Ok(Value::Undefined)
}

fn inert(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let n = d.style_html_el(it, this)?;
    Ok(Value::Bool(attr(&d.doc.borrow(), n, "inert").is_some()))
}

fn set_inert(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let n = d.style_html_el(it, this)?;
    if it.to_js_bool(arg(a, 0))? {
        nd::attr_set(d, it, n, "inert", "")?;
    } else {
        d.doc.borrow_mut().remove_attr(n, "inert");
    }
    Ok(Value::Undefined)
}

/// The default `tabIndex` of an element.
fn default_tab(doc: &Document, n: NodeId) -> i32 {
    let focusable = match name(doc, n) {
        "a" => true,
        _ if doc.is_foreign(n) => false,
        "area" | "button" | "input" | "select" | "textarea" | "iframe" | "object" | "video" | "audio" => true,
        "summary" => doc.parent(n).is_some_and(|p| html_name(doc, p) == "details" && doc.children(p).find(|&c| html_name(doc, c) == "summary") == Some(n)),
        _ => false,
    };
    if focusable { 0 } else { -1 }
}

/// "Rules for parsing integers" (HTML Standard): `None` when there is no number or it does not fit `i32`.
fn parse_int(s: &str) -> Option<i32> {
    let t = s.trim_start_matches([' ', '\t', '\n', '\x0c', '\r']);
    let (neg, t) = match t.as_bytes().first()? {
        b'-' => (true, &t[1..]),
        b'+' => (false, &t[1..]),
        _ => (false, t),
    };
    let digits = t.bytes().take_while(u8::is_ascii_digit).count();
    if digits == 0 {
        return None;
    }
    let v = t[..digits].trim_start_matches('0').parse::<i64>().unwrap_or(if digits > 18 { i64::MAX } else { 0 });
    i32::try_from(if neg { -v } else { v }).ok()
}

fn tab_index(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let n = d.el(it, this)?;
    let doc = d.doc.borrow();
    Ok(Value::Number(attr(&doc, n, "tabindex").and_then(parse_int).unwrap_or_else(|| default_tab(&doc, n)) as f64))
}

fn set_tab_index(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let n = d.el(it, this)?;
    let v = to_int32(it.to_js_number(arg(a, 0))?);
    nd::attr_set(d, it, n, "tabindex", &v.to_string())?;
    Ok(Value::Undefined)
}

// ---------------------------------------------------------------------------------------------- click

fn click(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let n = d.style_html_el(it, this)?;
    if d.is_disabled_control(it, n)? || d.clicking.borrow().contains(&n) {
        return Ok(Value::Undefined);
    }
    // A link with `download`: the click saves the target (the session starts it).
    let mut save = None;
    if let Some(t) = d.activation_target(it, n)? {
        let doc = d.doc.borrow();
        let kind = match html_name(&doc, t) {
            "a" => match (attr(&doc, t, "href"), attr(&doc, t, "download")) {
                (Some(h), Some(_)) if d.nav.s.borrow().downloads_on => {
                    save = Some((t, h.to_string()));
                    None
                }
                (h, _) => h.map(|_| "a link"),
            },
            "area" => attr(&doc, t, "href").map(|_| "a link"),
            "label" => Some("a label"),
            "summary" => Some("a summary"),
            "input" if matches!(nd::input_kind(&doc, t), "checkbox" | "radio") => Some("a checkbox or a radio"),
            _ => None,
        };
        if kind.is_some() {
            return Err(Dom::gate("HTMLElement.click() on a link, a label, a summary, a checkbox or a radio (the default action is built for the window only)"));
        }
    }
    d.clicking.borrow_mut().push(n);
    let r = d.user_click(it, n, None);
    d.clicking.borrow_mut().retain(|&c| c != n);
    if let (Some((t, href)), Ok(true)) = (save, &r) {
        d.queue_download(it, t, &href)?;
    }
    r.map(|_| Value::Undefined)
}

// ---------------------------------------------------------------------------------------------- offsetParent, offsetTop, offsetLeft

struct Offset {
    parent: Option<NodeId>,
    top: i32,
    left: i32,
}

/// The first box fragment of `n`. `None`: it has none. An inline box over lines has one fragment for each line. Layout writes the
/// lines in order, so the first fragment is the first line, as for `getClientRects`.
fn first_fragment(layout: &bare_layout::fragment::Layout, n: NodeId) -> Option<bare_layout::fragment::Rect> {
    layout.fragments.iter().find(|f| f.node == n && f.kind == bare_layout::fragment::Kind::Box).map(|f| f.rect)
}

/// What `offsetTop` and `offsetLeft` measure from: the first box of an element.
enum First {
    Box(bare_layout::fragment::Rect),
    /// A `br`: not a box.
    Break,
    None,
}

/// The first box of `n`, in layout coordinates.
fn first_box(d: &Dom, it: &mut Interp<'_>, styles: &bare_css::cascade::Styles, layout: &bare_layout::fragment::Layout, n: NodeId) -> R<First> {
    if let Some(r) = first_fragment(layout, n) {
        return Ok(First::Box(r));
    }
    // Layout makes no fragment for an inline box with no background and no border ("culled"): that is not "no box". Its first rectangle
    // of `getClientRects` (`browser_rects.rs`) is its first line, or its place on the line when it is empty. That rectangle is in window
    // coordinates, so take the scroll offsets and the sticky offset back out of it.
    let kind = bare_layout::boxes::Tree::new(&d.doc.borrow(), styles).kind(n);
    match kind {
        bare_layout::boxes::Kind::Inline => {
            let Some(&w) = crate::browser_rects::element_rects(d, it, n)?.first() else { return Ok(First::None) };
            let (dx, dy) = d.with_geo_it(it, |g, s, _| g.shift(s, n))?;
            Ok(First::Box(bare_layout::fragment::Rect { x: w.x - dx, y: w.y - dy, ..w }))
        }
        bare_layout::boxes::Kind::Break => Ok(First::Break),
        _ => Ok(First::None),
    }
}

/// `None`: the element has no box. Else its offset parent and offsets.
fn offset(d: &Dom, it: &mut Interp<'_>, el: NodeId) -> R<Option<Offset>> {
    if !d.in_document(it, el)? {
        return Ok(None);
    }
    let styles = d.cascade_now(it)?;
    let shown = |n: NodeId| styles.style(n).is_some_and(|s| !kw(s.get(Property::Display), Keyword::None));
    let doc = d.doc.borrow();
    let (mut cur, mut parent) = (Some(el), None);
    // The first positioned ancestor, and whether the element or an ancestor is not shown.
    let own = styles.style(el);
    let fixed = own.is_some_and(|s| kw(s.get(Property::Position), Keyword::Fixed));
    let is_static = own.is_none_or(|s| kw(s.get(Property::Position), Keyword::Static));
    let barred = fixed || matches!(html_name(&doc, el), "html" | "body");
    while let Some(c) = cur {
        it.charge(1)?;
        if !shown(c) && matches!(doc.data(c), NodeData::Element { .. }) {
            return Ok(None);
        }
        if c != el && parent.is_none() && !barred && matches!(doc.data(c), NodeData::Element { .. }) {
            let by_position = styles.style(c).is_some_and(|s| !kw(s.get(Property::Position), Keyword::Static));
            if by_position || html_name(&doc, c) == "body" || (is_static && matches!(html_name(&doc, c), "td" | "th" | "table")) {
                parent = Some(c);
            }
        }
        cur = doc.parent(c);
    }
    if html_name(&doc, el) == "body" {
        return Ok(Some(Offset { parent: None, top: 0, left: 0 }));
    }
    drop(doc);
    let lay = d.layout_shared(it, &styles)?;
    let (layout, areas) = (&lay.layout, &lay.areas);
    let r = match first_box(d, it, &styles, layout, el)? {
        First::Box(r) => r,
        // A `br` is not a box (Chrome: `LayoutBR` is a text), so its offsets are 0. It still has an offset parent.
        First::Break => return Ok(Some(Offset { parent, top: 0, left: 0 })),
        First::None => return Ok(None),
    };
    // A sticky box is where it sticks now.
    let (sx, sy) = d.stuck_now(&styles, areas, el);
    let (mut x, mut y) = (r.x + sx, r.y + sy);
    if let Some(p) = parent.filter(|&p| html_name(&d.doc.borrow(), p) != "body") {
        let First::Box(pr) = first_box(d, it, &styles, layout, p)? else {
            return Err(Dom::gate("offsetTop and offsetLeft in an offset parent that has no box of its own"));
        };
        let ps = styles.style(p).expect("a positioned parent has a style");
        let (px, py) = d.stuck_now(&styles, areas, p);
        x -= pr.x + px + ps.px(Property::BorderLeftWidth);
        y -= pr.y + py + ps.px(Property::BorderTopWidth);
    }
    let long = |v: f32| {
        let r = (v as f64 + 0.5).floor();
        (v.is_finite() && (i32::MIN as f64..=i32::MAX as f64).contains(&r)).then_some(r as i32)
    };
    match (long(y), long(x)) {
        (Some(top), Some(left)) => Ok(Some(Offset { parent, top, left })),
        _ => Err(Dom::gate("an offset that does not fit a long")),
    }
}

fn offset_parent(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let el = d.style_html_el(it, this)?;
    match offset(d, it, el)?.and_then(|o| o.parent) {
        Some(p) => d.wrap(it, p),
        None => Ok(Value::Null),
    }
}

fn offset_top(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let el = d.style_html_el(it, this)?;
    Ok(Value::Number(offset(d, it, el)?.map_or(0, |o| o.top) as f64))
}

fn offset_left(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let el = d.style_html_el(it, this)?;
    Ok(Value::Number(offset(d, it, el)?.map_or(0, |o| o.left) as f64))
}

// ---------------------------------------------------------------------------------------------- innerText

/// The result of the collection: text, plus the required line breaks that wait for the next text.
struct Out {
    s: String,
    /// The largest required line break count since the last text.
    brk: usize,
    /// A collapsible space that waits: it is written only if more text follows on the line.
    space: bool,
    line_start: bool,
}

impl Out {
    /// A required line break count item. A line break ends the line, so a waiting space is lost.
    fn count(&mut self, n: usize) {
        if n == 0 {
            return;
        }
        self.brk = self.brk.max(n);
        self.space = false;
        self.line_start = true;
    }

    /// The breaks that wait are written between two texts, never at the start.
    fn flush(&mut self) {
        if !self.s.is_empty() {
            self.s.extend(std::iter::repeat_n('\n', self.brk));
        }
        self.brk = 0;
    }

    /// A string item that is not text of the page: `br`, a tab, a line feed.
    fn lit(&mut self, t: &str) {
        self.flush();
        self.s.push_str(t);
        self.space = false;
        self.line_start = true;
    }

    fn ch(&mut self, c: char) {
        self.flush();
        if self.space {
            self.s.push(' ');
            self.space = false;
        }
        self.s.push(c);
        self.line_start = false;
    }

    /// The text of one text node under the `white-space` keyword `ws`.
    fn text(&mut self, text: &str, ws: Keyword) {
        let keep = matches!(ws, Keyword::Pre | Keyword::PreWrap | Keyword::BreakSpaces);
        for c in text.chars() {
            match c {
                _ if keep => {
                    self.flush();
                    self.s.push(c);
                    self.space = false;
                    self.line_start = c == '\n';
                }
                '\n' if ws == Keyword::PreLine => self.lit("\n"),
                ' ' | '\t' | '\n' | '\r' | '\x0c' => self.space = !self.line_start,
                _ => self.ch(c),
            }
        }
    }
}

/// `text-transform` on one text node.
fn transform(text: &str, t: Cv) -> String {
    if kw(t, Keyword::Uppercase) {
        text.to_uppercase()
    } else if kw(t, Keyword::Lowercase) {
        text.to_lowercase()
    } else if kw(t, Keyword::Capitalize) {
        let (mut s, mut prev) = (String::with_capacity(text.len()), ' ');
        for c in text.chars() {
            if prev.is_alphanumeric() || matches!(prev, '\'' | '\u{2019}') { s.push(c) } else { s.extend(c.to_uppercase()) }
            prev = c;
        }
        s
    } else {
        text.to_string()
    }
}

fn inner_text(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let n = d.style_html_el(it, this)?;
    let styles = if d.in_document(it, n)? { Some(d.cascade_now(it)?) } else { None };
    let shown = |s: &bare_css::cascade::Styles, n| s.style(n).is_some_and(|c| !kw(c.get(Property::Display), Keyword::None));
    let Some(styles) = styles.filter(|s| shown(s, n)) else {
        return nd::text_content(d, it, this, &[]);
    };
    let doc = d.doc.borrow();
    let tree = bare_layout::boxes::Tree::new(&doc, &styles);
    let table_part = |k: Cv| [Keyword::TableRow, Keyword::TableCell, Keyword::TableRowGroup, Keyword::TableHeaderGroup, Keyword::TableFooterGroup].iter().any(|&p| kw(k, p));
    let group = |k: Cv| [Keyword::TableRowGroup, Keyword::TableHeaderGroup, Keyword::TableFooterGroup].iter().any(|&g| kw(k, g));
    // A later sibling (an element) with this display.
    let later = |mut c: NodeId, ks: &[Keyword]| {
        while let Some(s) = doc.next_sibling(c) {
            if styles.style(s).is_some_and(|st| ks.iter().any(|&k| kw(st.get(Property::Display), k))) {
                return true;
            }
            c = s;
        }
        false
    };
    let mut out = Out { s: String::new(), brk: 0, space: false, line_start: true };
    // The nodes to leave: the exit steps of an element run after its children.
    let mut stack: Vec<(NodeId, bool)> = doc.children(n).collect::<Vec<_>>().into_iter().rev().map(|c| (c, false)).collect();
    while let Some((c, exit)) = stack.pop() {
        it.charge(1)?;
        if out.s.len() > d.lim.max_string_bytes {
            return Err(d.lim("dom-string"));
        }
        match *doc.data(c) {
            NodeData::Text(sp) => {
                let Some(p) = doc.parent(c).and_then(|p| styles.style(p)) else { continue };
                if kw(p.get(Property::Visibility), Keyword::Visible) {
                    let ws = if let Cv::Keyword(k) = p.get(Property::WhiteSpace) { k } else { Keyword::Normal };
                    out.text(&transform(doc.text(sp), p.get(Property::TextTransform)), ws);
                }
            }
            NodeData::Element { .. } => {
                let Some(st) = styles.style(c).filter(|st| !kw(st.get(Property::Display), Keyword::None)) else { continue };
                let visible = kw(st.get(Property::Visibility), Keyword::Visible);
                let (kind, display) = (tree.kind(c), st.get(Property::Display));
                let atomic = kind == bare_layout::boxes::Kind::Atomic;
                let in_flex = doc.flat_parent(c).and_then(|p| styles.style(p)).is_some_and(|p| [Keyword::Flex, Keyword::InlineFlex, Keyword::Grid, Keyword::InlineGrid].iter().any(|&k| kw(p.get(Property::Display), k)));
                let block = match kind {
                    bare_layout::boxes::Kind::Block | bare_layout::boxes::Kind::ListItem => !table_part(display) || in_flex,
                    bare_layout::boxes::Kind::Float | bare_layout::boxes::Kind::OutOfFlow => true,
                    _ => false,
                };
                let p = html_name(&doc, c) == "p";
                if !exit {
                    if visible {
                        if matches!(html_name(&doc, c), "select" | "textarea") {
                            return Err(Dom::gate("innerText over a select or a textarea"));
                        }
                        out.count(if p { 2 } else { block as usize });
                        if atomic {
                            if out.space {
                                out.flush();
                                out.s.push(' ');
                            }
                            out.space = false;
                            out.line_start = true;
                        }
                    }
                    stack.push((c, true));
                    stack.extend(doc.children(c).collect::<Vec<_>>().into_iter().rev().map(|k| (k, false)));
                } else if visible {
                    if html_name(&doc, c) == "br" {
                        out.lit("\n");
                    }
                    if kw(display, Keyword::TableCell) && later(c, &[Keyword::TableCell]) {
                        out.lit("\t");
                    }
                    let in_group = doc.parent(c).is_some_and(|g| styles.style(g).is_some_and(|s| group(s.get(Property::Display))));
                    if kw(display, Keyword::TableRow) && (later(c, &[Keyword::TableRow]) || in_group && doc.parent(c).is_some_and(|g| later(g, &[Keyword::TableRowGroup, Keyword::TableHeaderGroup, Keyword::TableFooterGroup]))) {
                        out.lit("\n");
                    }
                    out.count(if p { 2 } else { block as usize });
                    if atomic {
                        out.space = false;
                        out.line_start = false;
                    }
                }
            }
            _ => {}
        }
    }
    let s = out.s;
    drop(doc);
    d.js_str(it, &s)
}

fn set_inner_text(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let n = d.style_html_el(it, this)?;
    let v = arg(a, 0);
    let s = if matches!(v, Value::Null) { String::new() } else { d.string_arg(it, v)? };
    // `\r\n`, `\r` and `\n` are one break each.
    let s = s.replace("\r\n", "\n").replace('\r', "\n");
    let lines: Vec<&str> = if s.is_empty() { Vec::new() } else { s.split('\n').collect() };
    d.mo_group(Some(n), || {
        let mut doc = d.doc.borrow_mut();
        d.room(&doc, lines.iter().filter(|l| !l.is_empty()).count() + lines.len().saturating_sub(1), s.len())?;
        while let Some(c) = doc.children(n).next() {
            it.charge(1)?;
            doc.detach(c);
        }
        for (i, line) in lines.iter().enumerate() {
            if i > 0 {
                let br = doc.create_element("br");
                doc.move_to(br, n, None);
            }
            if !line.is_empty() {
                let t = doc.create_text(line);
                doc.move_to(t, n, None);
            }
        }
        Ok(())
    })?;
    Ok(Value::Undefined)
}

// ---------------------------------------------------------------------------------------------- draggable, checkVisibility

/// The `draggable` IDL attribute: `true` and `false` (ASCII case-insensitive) decide; any other value or none is "auto", which is true for `img`, and for
/// `a` and `area` with `href` (Chrome; the standard names only `a`).
fn draggable(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let n = d.style_html_el(it, this)?;
    let doc = d.doc.borrow();
    Ok(Value::Bool(match attr(&doc, n, "draggable") {
        Some(v) if v.eq_ignore_ascii_case("true") => true,
        Some(v) if v.eq_ignore_ascii_case("false") => false,
        _ => match name(&doc, n) {
            "img" => true,
            "a" | "area" => attr(&doc, n, "href").is_some(),
            _ => false,
        },
    }))
}

fn set_draggable(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let n = d.style_html_el(it, this)?;
    let on = it.to_js_bool(arg(a, 0))?;
    nd::attr_set(d, it, n, "draggable", if on { "true" } else { "false" })?;
    Ok(Value::Undefined)
}

/// Does a style text use what the engine does not parse: `content-visibility` or `display: contents`?
fn unmodelled(t: &str, auto: bool) -> bool {
    use bare_css::tokenizer::{Token, tokenize};
    let mut tokens = tokenize(t).filter(|t| !matches!(t, Token::Whitespace));
    while let Some(t) = tokens.next() {
        let Token::Ident(name) = t else { continue };
        if !name.eq_ignore_ascii_case("content-visibility") && !name.eq_ignore_ascii_case("display") { continue; }
        if !matches!(tokens.next(), Some(Token::Colon)) { continue; }
        if let Some(Token::Ident(value)) = tokens.next() {
            if name.eq_ignore_ascii_case("display") && value.eq_ignore_ascii_case("contents")
                || name.eq_ignore_ascii_case("content-visibility") && (value.eq_ignore_ascii_case("hidden") || auto && value.eq_ignore_ascii_case("auto")) { return true; }
        }
    }
    false
}

/// `Element.checkVisibility(options)` (CSSOM View). Options are read in the order of the dictionary (sorted by name). `opacityProperty` and
/// `visibilityProperty` are the old names of `checkOpacity` and `checkVisibilityCSS`: either one turns the check on.
fn check_visibility(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let n = d.el(it, this)?;
    let mut opt = [false; 5];
    match arg(a, 0) {
        Value::Undefined | Value::Null => {}
        o @ Value::Object(_) => {
            for (i, k) in ["checkOpacity", "checkVisibilityCSS", "contentVisibilityAuto", "opacityProperty", "visibilityProperty"].into_iter().enumerate() {
                let k = d.key(it, k)?;
                let v = it.get_value(o, k)?;
                opt[i] = it.to_js_bool(v)?;
            }
        }
        _ => return Err(it.type_error("Failed to execute 'checkVisibility' on 'Element': The provided value is not of type 'CheckVisibilityOptions'.")),
    }
    let (opacity, visibility) = (opt[0] | opt[3], opt[1] | opt[4]);
    {
        let doc = d.doc.borrow();
        if doc.is_foreign(n) {
            return Err(Dom::gate("checkVisibility of an SVG or MathML element (Chrome's answer is not modelled)"));
        }
        if matches!(name(&doc, n), "option" | "optgroup") {
            return Err(Dom::gate("checkVisibility of an option (it depends on how the select is drawn)"));
        }
    }
    if !d.in_document(it, n)? {
        return Ok(Value::Bool(false));
    }
    let Some(snap) = d.snapshot(it, n)? else { return Ok(Value::Bool(false)) };
    if snap.hidden {
        return Ok(Value::Bool(false));
    }
    let odd = {
        let c = d.computed.cache.borrow();
        let b = c.as_ref().expect("built by the snapshot");
        if opt[2] { b.key.iter().any(|(_, _, t)| unmodelled(t, true)) } else { *b.odd.get_or_init(|| b.key.iter().any(|(_, _, t)| unmodelled(t, false))) }
    };
    if visibility && matches!(snap.style.get(Property::Visibility), Cv::Keyword(Keyword::Hidden | Keyword::Collapse)) {
        return Ok(Value::Bool(false));
    }
    let doc = d.doc.borrow();
    let (mut cur, mut child, mut steps) = (Some(n), n, 0u64);
    while let Some(e) = cur {
        steps += 1;
        if let NodeData::Element { .. } = doc.data(e) {
            if attr(&doc, e, "style").is_some_and(|t| unmodelled(t, opt[2])) {
                return Err(Dom::gate("checkVisibility with content-visibility or display: contents in a style attribute (not modelled)"));
            }
            if opacity && snap.styles.style(e).is_some_and(|s| s.get(Property::Opacity) == Cv::Number(0.0)) {
                return Ok(Value::Bool(false));
            }
            // The content of a closed `details` (all but the first `summary`) is `content-visibility: hidden`.
            if e != child && name(&doc, e) == "details" && !doc.is_foreign(e) && attr(&doc, e, "open").is_none() {
                let summary = doc.children(e).find(|&c| name(&doc, c) == "summary");
                if summary != Some(child) {
                    return Ok(Value::Bool(false));
                }
            }
        }
        child = e;
        cur = doc.flat_parent(e);
    }
    drop(doc);
    it.charge(steps / 8 + 1)?;
    if odd {
        return Err(Dom::gate("checkVisibility on a page with content-visibility or display: contents in a style sheet (not modelled)"));
    }
    Ok(Value::Bool(true))
}

pub(crate) fn install(d: &Dom, it: &mut Interp<'_>) -> R<()> {
    let (html, svg) = (d.p().html, d.p().svg);
    d.def_get(it, html, "hidden", hidden, Some(set_hidden))?;
    d.def_get(it, html, "inert", inert, Some(set_inert))?;
    d.def_get(it, html, "draggable", draggable, Some(set_draggable))?;
    d.def_method(it, d.p().element, "checkVisibility", 0, check_visibility)?;
    d.def_get(it, html, "innerText", inner_text, Some(set_inner_text))?;
    d.def_get(it, html, "offsetParent", offset_parent, None)?;
    d.def_get(it, html, "offsetTop", offset_top, None)?;
    d.def_get(it, html, "offsetLeft", offset_left, None)?;
    d.def_method(it, html, "click", 0, click)?;
    for proto in [html, svg] {
        d.def_get(it, proto, "tabIndex", tab_index, Some(set_tab_index))?;
    }
    Ok(())
}
