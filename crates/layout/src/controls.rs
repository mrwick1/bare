//! Form controls: the size and the text of `<input>`, `<select>` and `<textarea>`, with the numbers of Chrome on Linux.
//!
//! The controls are boxes of the UA style sheet (`ua.css`: font 13.3333 px, padding, border, `box-sizing`). What the style
//! sheet cannot say is the size of the content when `width` or `height` is `auto`:
//!
//! - A text field is `size` letters wide (20 if the attribute is missing) and one line high. Chrome counts the letters
//!   with the average width of the font (`OS/2.xAvgCharWidth`, rounded to a whole pixel) and adds `maxCharWidth - avg`,
//!   where the max is the width of the font box (`head.xMax - head.xMin`, rounded). For Noto Sans Mono that is 0.644 em
//!   and 2.416 em: 203 px at 13.3333 px with `size` 20. The numbers match Chrome for 9, 12, 13.3333, 15, 18 and 40 px
//!   and are 1 to 3 px off for some other sizes (to verify: why; the hinted advance of Skia is the likely cause).
//! - A button made from an `<input>` is as wide as its label (`value`, or `Submit` and `Reset`) and one line high.
//! - A `<select>` is as wide as its widest option, plus what its padding and arrow take (the UA padding), and one line high.
//! - A `<textarea>` is `cols` letters (20) wide, with the width of a scrollbar when it scrolls (Chrome always reserves it),
//!   and `rows` lines high (2). Its text is a child of the element and goes through the normal layout.
//! - A `<button>` has children: its content is laid out as in any inline-block. Chrome centers that content in the height.
//!
//! The text of a field (the `value`, the placeholder) has no box: paint draws it in the content box.

use crate::block::SCROLLBAR;
use crate::boxes::Tree;
use crate::font::{self, Metrics};
use bare_css::values::Keyword;
use bare_html::dom::{Atom, Document, NodeData, NodeId};

/// The atoms of the names that controls use. A page with no `<input>` has no atom for it, so the test is one compare.
#[derive(Clone, Copy)]
pub struct Atoms {
    pub input: Option<Atom>,
    pub select: Option<Atom>,
    pub textarea: Option<Atom>,
    pub button: Option<Atom>,
    pub kind: Option<Atom>,
    pub size: Option<Atom>,
    pub cols: Option<Atom>,
    pub rows: Option<Atom>,
    pub value: Option<Atom>,
    pub placeholder: Option<Atom>,
    pub checked: Option<Atom>,
    pub selected: Option<Atom>,
    pub label: Option<Atom>,
}

impl Atoms {
    pub fn new(doc: &Document) -> Atoms {
        let f = |n| doc.find_atom(n);
        Atoms {
            input: f("input"),
            select: f("select"),
            textarea: f("textarea"),
            button: f("button"),
            kind: f("type"),
            size: f("size"),
            cols: f("cols"),
            rows: f("rows"),
            value: f("value"),
            placeholder: f("placeholder"),
            checked: f("checked"),
            selected: f("selected"),
            label: f("label"),
        }
    }
}

/// What kind of control an element is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Control {
    /// A text line in a box: `text`, `email`, `password`, `search`, `url`, `tel`, `number` and a type that we do not know.
    Field,
    /// A button whose label is its `value`: `submit`, `reset`, `button`.
    Label,
    /// `checkbox` and `radio`. The UA style sheet gives the size. `radio` tells which one.
    Check { radio: bool },
    Select,
    TextArea,
    /// `<button>`: content like an inline-block, centered in the height.
    Button,
}

/// The control that `node` is, if it is one. Types that draw something we do not have (`file`, `color`, `range`, `date`,
/// `image`) are no control: they keep the box of the style sheet.
pub fn of(tree: &Tree, node: NodeId) -> Option<Control> {
    let NodeData::Element { name, .. } = *tree.doc.data(node) else { return None };
    let a = &tree.controls;
    let name = Some(name);
    if name == a.input {
        let kind = a.kind.and_then(|k| tree.doc.attr(node, k)).unwrap_or("");
        let is = |s: &str| kind.eq_ignore_ascii_case(s);
        return match () {
            _ if is("checkbox") => Some(Control::Check { radio: false }),
            _ if is("radio") => Some(Control::Check { radio: true }),
            _ if is("submit") || is("reset") || is("button") => Some(Control::Label),
            _ if ["file", "color", "range", "date", "datetime-local", "time", "month", "week", "image", "hidden"].iter().any(|t| is(t)) => None,
            _ => Some(Control::Field),
        };
    }
    if name == a.select {
        return Some(Control::Select);
    }
    if name == a.textarea {
        return Some(Control::TextArea);
    }
    if name == a.button {
        return Some(Control::Button);
    }
    None
}

/// The average letter width that Chrome uses for a font of `size` pixels, and the width of the font box.
fn avg_width(size: f32) -> f32 {
    (size * 0.644).round()
}

fn max_width(size: f32) -> f32 {
    (size * 2.416).round()
}

/// A positive whole number in an attribute, or `default`.
fn count(tree: &Tree, node: NodeId, name: Option<Atom>, default: u32) -> u32 {
    name.and_then(|a| tree.doc.attr(node, a)).and_then(|v| v.trim().parse::<u32>().ok()).filter(|&n| n > 0).unwrap_or(default)
}

/// The label of a button made from an `<input>`: its `value`, or the word of its type when the attribute is missing.
pub fn label<'a>(tree: &Tree<'a>, node: NodeId) -> &'a str {
    let a = &tree.controls;
    if let Some(v) = a.value.and_then(|v| tree.doc.attr(node, v)) {
        return v;
    }
    let kind = a.kind.and_then(|k| tree.doc.attr(node, k)).unwrap_or("");
    if kind.eq_ignore_ascii_case("submit") {
        "Submit"
    } else if kind.eq_ignore_ascii_case("reset") {
        "Reset"
    } else {
        ""
    }
}

/// The width of a text, in pixels, in a font of `m`.
pub fn text_width(m: &Metrics, s: &str) -> f32 {
    m.px(s.chars().map(font::width_of).sum())
}

/// The text of an `<option>`: its `label`, or its text with the spaces collapsed.
pub fn option_text(tree: &Tree, option: NodeId) -> String {
    if let Some(l) = tree.controls.label.and_then(|a| tree.doc.attr(option, a)).filter(|l| !l.is_empty()) {
        return l.to_string();
    }
    let mut out = String::new();
    for c in tree.doc.children(option) {
        if let NodeData::Text(span) = *tree.doc.data(c) {
            for word in tree.doc.text(span).split_ascii_whitespace() {
                if !out.is_empty() {
                    out.push(' ');
                }
                out.push_str(word);
            }
        }
    }
    out
}

/// The options of a `<select>`, in order: its children, and the children of its `<optgroup>`s.
pub fn options(tree: &Tree, select: NodeId) -> Vec<NodeId> {
    let mut out = Vec::new();
    let mut stack: Vec<NodeId> = tree.doc.children(select).collect();
    stack.reverse();
    while let Some(n) = stack.pop() {
        let NodeData::Element { name, .. } = *tree.doc.data(n) else { continue };
        match tree.doc.atom_str(name) {
            "option" => out.push(n),
            "optgroup" => stack.extend(tree.doc.children(n).collect::<Vec<_>>().into_iter().rev()),
            _ => {}
        }
    }
    out
}

/// The text that a closed `<select>` shows: the option with `selected` (the last one), or the first option.
pub fn selected_text(tree: &Tree, select: NodeId) -> String {
    let all = options(tree, select);
    let chosen = all.iter().rev().find(|&&o| tree.controls.selected.is_some_and(|a| tree.doc.attr(o, a).is_some())).or(all.first());
    chosen.map(|&o| option_text(tree, o)).unwrap_or_default()
}

/// The size of the content box when `width` and `height` are `auto`: (width, height). `None` for a control that has no size
/// of its own (a checkbox, a `<button>`).
pub fn size(tree: &Tree, node: NodeId, control: Control) -> Option<(f32, f32)> {
    let style = tree.style(node);
    let m = Metrics::of(style);
    let line = m.line_height(style);
    match control {
        Control::Field => {
            let (avg, chars) = (avg_width(m.size), count(tree, node, tree.controls.size, 20) as f32);
            Some(((chars * avg).ceil() + max_width(m.size) - avg, line))
        }
        Control::Label => Some((text_width(&m, label(tree, node)), line)),
        Control::Select => {
            let widest = options(tree, node).iter().map(|&o| text_width(&m, &option_text(tree, o))).fold(0.0f32, f32::max);
            Some((widest, line))
        }
        Control::TextArea => {
            let (cols, rows) = (count(tree, node, tree.controls.cols, 20) as f32, count(tree, node, tree.controls.rows, 2) as f32);
            let bar = if matches!(tree.overflow(node).1, Keyword::Auto | Keyword::Scroll) { SCROLLBAR } else { 0.0 };
            Some((cols * avg_width(m.size) + bar, rows * line))
        }
        Control::Check { .. } | Control::Button => None,
    }
}

/// The distance from the top of the content box to the baseline of the text line in it (a field, a button, a select):
/// the line is centered in the content box.
pub fn baseline(tree: &Tree, node: NodeId, content_height: f32) -> f32 {
    let style = tree.style(node);
    let m = Metrics::of(style);
    let line = m.line_height(style);
    // The line box has the ascent and the descent in its middle: half of the free room is above the letters.
    (content_height - line).max(0.0) / 2.0 + (line - m.normal_line_height()) / 2.0 + m.ascent
}

#[cfg(test)]
mod tests {
    use crate::block::tests::{rect, run};

    fn at(html: &str, css: &str, id: &str) -> (f32, f32) {
        let r = rect(html, css, id);
        // The font size 13.3333 px makes a letter a little under 8 px. Chrome rounds to 1/64 px: compare to 0.1 px.
        let round = |v: f32| (v * 10.0).round() / 10.0;
        (round(r.w), round(r.h))
    }

    #[test]
    fn a_text_field_has_one_line_and_twenty_letters() {
        // Chrome, Noto Sans Mono 13.3333 px: 203 by 18 content, plus padding 1 and 2, border 2.
        assert_eq!(at("<input id=a>", "", "a"), (211.0, 24.0));
        assert_eq!(at("<input id=a size=40>", "", "a"), (391.0, 24.0));
        assert_eq!(at("<input id=a size=1>", "", "a"), (40.0, 24.0));
        assert_eq!(at("<input id=a type=password>", "", "a"), (211.0, 24.0));
        // A block field is not stretched: it has its own width.
        assert_eq!(at("<input id=a style='display:block'>", "", "a"), (211.0, 24.0));
        // `width` and `height` still win.
        assert_eq!(at("<input id=a style='width:100px;height:30px'>", "", "a"), (108.0, 36.0));
    }

    #[test]
    fn buttons_are_as_wide_as_their_label() {
        // "Submit" is 6 letters of 8 px: 48, plus padding 12 and border 4 (border-box in Chrome too).
        assert_eq!(at("<input id=a type=submit>", "", "a"), (64.0, 24.0));
        assert_eq!(at("<input id=a type=button value=Go>", "", "a"), (32.0, 24.0));
        assert_eq!(at("<input id=a type=reset>", "", "a"), (56.0, 24.0));
        assert_eq!(at("<button id=a>Log in</button>", "", "a"), (64.0, 24.0));
        assert_eq!(at("<button id=a style='height:60px'>x</button>", "", "a").1, 60.0);
    }

    #[test]
    fn checkboxes_and_radios_are_13_px() {
        assert_eq!(at("<input id=a type=checkbox>", "", "a"), (13.0, 13.0));
        assert_eq!(at("<input id=a type=radio>", "", "a"), (13.0, 13.0));
    }

    #[test]
    fn a_textarea_has_cols_and_rows() {
        // 20 letters of 9 px, the scrollbar, 2 lines of 18, padding 2 and border 1.
        assert_eq!(at("<textarea id=a></textarea>", "", "a"), (201.0, 42.0));
        assert_eq!(at("<textarea id=a cols=10 rows=3></textarea>", "", "a"), (111.0, 60.0));
        assert_eq!(at("<textarea id=a style='overflow:hidden'></textarea>", "", "a"), (186.0, 42.0));
    }

    #[test]
    fn a_select_is_as_wide_as_its_widest_option() {
        // Text, then 22 px: border 2, padding 20.
        assert_eq!(at("<select id=a><option>a</option></select>", "", "a"), (30.0, 22.0));
        assert_eq!(at("<select id=a><option>abcdefghij</option><option>x</option></select>", "", "a"), (102.0, 22.0));
        assert_eq!(at("<select id=a></select>", "", "a"), (22.0, 22.0));
        assert_eq!(at("<select id=a><optgroup><option> a   b </option></optgroup></select>", "", "a"), (22.0 + 24.0, 22.0));
    }

    #[test]
    fn a_control_sits_on_the_text_baseline() {
        // The text next to the field and the text in it share a baseline: the box is not pushed down by its own height.
        let (doc, l) = run("<p>x <input id=a> y</p>", "p { margin: 0; font-size: 13.3333px }");
        let text = l.fragments.iter().find(|f| matches!(f.kind, crate::fragment::Kind::Text { .. })).unwrap();
        let field = l.fragments.iter().find(|f| f.kind == crate::fragment::Kind::Box && doc.element_name(f.node) == "input").unwrap();
        // Field: border 2 + padding 1 + ascent 14 = 17 from its top. Text: ascent 14 from its top.
        assert_eq!(field.rect.y + 17.0, text.rect.y + 14.0);
    }
}
