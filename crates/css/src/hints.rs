//! Presentational hints: the HTML attributes that act as style (HTML standard, "Rendering", tables). `<table width=85%
//! cellpadding=0 border=1 align=center>` and `<td valign=top width=50>` are the common ones in old pages.
//!
//! A hint is a declaration of the author origin with the lowest priority: any rule of the page beats it, and it beats
//! the browser's sheet. `of` gives the declarations as text, like a `style=""` attribute, and the cascade reads them
//! the same way. A cell reads `cellpadding` and `border` of the table around it (the table, and up to three levels
//! above the cell: `td`, `tr`, `tbody`). `<img width height>` gives its size. Only elements of a page that has a `<table>` are looked at.
//! An `<svg>` gives its size (`width`, `height`, `viewBox`), and the shapes of an SVG give `fill` and `stroke`: only elements of a page that has an `<svg>` are looked at.

use bare_html::dom::{Atom, Document, NodeData, NodeId};

/// The atoms of the names that a page can have. A name that the page never uses has no atom, so nothing matches it.
pub struct Hints {
    table: Option<Atom>,
    svg: Option<Atom>,
    shapes: [Option<Atom>; 8],
    view_box: Option<Atom>,
    fill: Option<Atom>,
    stroke: Option<Atom>,
    td: Option<Atom>,
    th: Option<Atom>,
    tr: Option<Atom>,
    group: [Option<Atom>; 3],
    col: [Option<Atom>; 2],
    img: Option<Atom>,
    width: Option<Atom>,
    height: Option<Atom>,
    border: Option<Atom>,
    spacing: Option<Atom>,
    padding: Option<Atom>,
    align: Option<Atom>,
    valign: Option<Atom>,
    nowrap: Option<Atom>,
}

impl Hints {
    /// `None` for a page with no `<table>` and no `<svg>`: no element has a hint.
    pub fn new(doc: &Document) -> Option<Hints> {
        let a = |n| doc.find_atom(n);
        // The sheet of the browser has a selector `svg`, so its atom is there on every page: look for an element that is not in HTML.
        let (table, svg) = (a("table"), if doc.has_foreign() { a("svg") } else { None });
        if table.is_none() && svg.is_none() {
            return None;
        }
        Some(Hints {
            table,
            svg,
            shapes: ["g", "path", "line", "polyline", "polygon", "rect", "circle", "ellipse"].map(|n| svg.and_then(|_| a(n))),
            view_box: svg.and_then(|_| a("viewBox")),
            fill: svg.and_then(|_| a("fill")),
            stroke: svg.and_then(|_| a("stroke")),
            td: a("td"),
            th: a("th"),
            tr: a("tr"),
            group: [a("thead"), a("tbody"), a("tfoot")],
            col: [a("col"), a("colgroup")],
            img: a("img"),
            width: a("width"),
            height: a("height"),
            border: a("border"),
            spacing: a("cellspacing"),
            padding: a("cellpadding"),
            align: a("align"),
            valign: a("valign"),
            nowrap: a("nowrap"),
        })
    }

    /// The hint declarations of an element, as the text of a `style=""` attribute. `None` for most elements.
    pub fn of(&self, doc: &Document, node: NodeId) -> Option<String> {
        let NodeData::Element { name, .. } = *doc.data(node) else { return None };
        let attr = |n: Option<Atom>, el: NodeId| n.and_then(|n| doc.attr(el, n));
        let mut out = String::new();
        let size = |out: &mut String, property: &str, v: Option<&str>| {
            if let Some(d) = v.and_then(dimension) {
                out.push_str(&format!("{property}:{d};"));
            }
        };
        let align = |out: &mut String, v: Option<&str>| {
            if let Some(v) = v {
                match v.to_ascii_lowercase().as_str() {
                    "left" | "right" | "center" | "justify" => out.push_str(&format!("text-align:{};", v.to_ascii_lowercase())),
                    "middle" => out.push_str("text-align:center;"),
                    _ => {}
                }
            }
        };
        let valign = |out: &mut String, v: Option<&str>| {
            if let Some(v) = v.map(str::to_ascii_lowercase)
                && matches!(v.as_str(), "top" | "middle" | "bottom" | "baseline")
            {
                out.push_str(&format!("vertical-align:{v};"));
            }
        };
        // The shapes of an SVG have no box: paint draws them from their attributes. A `g` keeps a style, for the shapes in it, and has nothing to show.
        // Only the children of an `svg` or a `g` need the hint: a box with `display: none` gets no style for its children.
        let hidden = self.svg.is_some() && Some(name) != self.shapes[0] && doc.parent(node).is_some_and(|p| matches!(*doc.data(p), NodeData::Element { name: n, .. } if Some(n) == self.svg || Some(n) == self.shapes[0]));
        if Some(name) == self.svg {
            self.svg_size(doc, node, &mut out);
            self.paint(doc, node, &mut out);
        } else if self.svg.is_some() && self.shapes.contains(&Some(name)) {
            self.paint(doc, node, &mut out);
        } else if Some(name) == self.table {
            size(&mut out, "width", attr(self.width, node));
            size(&mut out, "height", attr(self.height, node));
            if let Some(v) = attr(self.spacing, node).and_then(number) {
                out.push_str(&format!("border-spacing:{v}px;"));
            }
            if let Some(v) = attr(self.border, node) {
                // An empty value is 1 (the standard). Not a number: no border.
                let n = if v.trim().is_empty() { Some(1) } else { number(v) };
                if let Some(n) = n.filter(|&n| n > 0) {
                    out.push_str(&format!("border-width:{n}px;border-style:outset;"));
                }
            }
            match attr(self.align, node).map(str::to_ascii_lowercase).as_deref() {
                Some("center" | "middle") => out.push_str("margin-left:auto;margin-right:auto;"),
                Some("left") => out.push_str("float:left;"),
                Some("right") => out.push_str("float:right;"),
                _ => {}
            }
        } else if Some(name) == self.td || Some(name) == self.th {
            size(&mut out, "width", attr(self.width, node));
            size(&mut out, "height", attr(self.height, node));
            align(&mut out, attr(self.align, node));
            valign(&mut out, attr(self.valign, node));
            if self.nowrap.is_some_and(|n| doc.attr(node, n).is_some()) {
                out.push_str("white-space:nowrap;");
            }
            // The table is the grandparent or the great-grandparent: `td` in `tr` in `table`, or in `tbody` in `table`.
            let table = std::iter::successors(doc.parent(node), |&n| doc.parent(n)).take(3).find(|&n| matches!(*doc.data(n), NodeData::Element { name, .. } if Some(name) == self.table));
            if let Some(t) = table {
                if let Some(d) = attr(self.padding, t).and_then(dimension) {
                    out.push_str(&format!("padding:{d};"));
                }
                if attr(self.border, t).is_some_and(|v| v.trim().is_empty() || number(v).is_some_and(|n| n > 0)) {
                    out.push_str("border-width:1px;border-style:inset;");
                }
            }
        } else if Some(name) == self.tr || self.group.contains(&Some(name)) {
            align(&mut out, attr(self.align, node));
            valign(&mut out, attr(self.valign, node));
        } else if self.col.contains(&Some(name)) {
            size(&mut out, "width", attr(self.width, node));
        } else if Some(name) == self.img {
            // Spacer images size the cells of old pages.
            size(&mut out, "width", attr(self.width, node));
            size(&mut out, "height", attr(self.height, node));
        }
        if hidden {
            out.push_str("display:none;");
        }
        (!out.is_empty()).then_some(out)
    }
}

impl Hints {
    /// The size of an `<svg>` from `width`, `height` and `viewBox` (SVG 2, "sizing"): 300 by 150 with no attribute, the width of the container with a `viewBox` alone. A missing side comes
    /// from the other side and the ratio of the `viewBox`, so the box has that ratio. A `%` side is of the containing block.
    fn svg_size(&self, doc: &Document, node: NodeId, out: &mut String) {
        let attr = |n: Option<Atom>| n.and_then(|n| doc.attr(node, n));
        let (w, h) = (attr(self.width).and_then(dimension), attr(self.height).and_then(dimension));
        let ratio = attr(self.view_box).and_then(|v| {
            let mut n = v.split(|c: char| c == ',' || c.is_whitespace()).filter(|t| !t.is_empty()).map(|t| t.parse::<f32>().ok());
            let (_, _, w, h) = (n.next()??, n.next()??, n.next()??, n.next()??);
            (n.next().is_none() && w > 0.0 && h > 0.0).then_some((w, h))
        });
        match (w, h, ratio) {
            (Some(w), Some(h), _) => out.push_str(&format!("width:{w};height:{h};")),
            (Some(w), None, Some(_)) => out.push_str(&format!("width:{w};")),
            (None, Some(h), Some((vw, vh))) if h.ends_with("px") => {
                let px: f32 = h.trim_end_matches("px").parse().unwrap_or(0.0);
                out.push_str(&format!("width:{}px;height:{h};", px * vw / vh));
            }
            (None, None, Some(_)) => out.push_str("width:100%;"),
            (w, h, _) => out.push_str(&format!("width:{};height:{};", w.as_deref().unwrap_or("300px"), h.as_deref().unwrap_or("150px"))),
        }
        if let Some((vw, vh)) = ratio {
            out.push_str(&format!("aspect-ratio:{vw}/{vh};"));
        }
    }

    /// `fill` and `stroke` as attributes. A value that is not a color or `none` is dropped by the parser of the declaration.
    fn paint(&self, doc: &Document, node: NodeId, out: &mut String) {
        for (name, property) in [(self.fill, "fill"), (self.stroke, "stroke")] {
            if let Some(v) = name.and_then(|n| doc.attr(node, n)).filter(|v| !v.contains([';', '{', '}', '!'])) {
                out.push_str(&format!("{property}:{v};"));
            }
        }
    }
}

/// A whole number at the start of an attribute value (`"3"`, `" 12px"`): `None` if there is none.
fn number(v: &str) -> Option<u32> {
    let v = v.trim_start();
    let end = v.find(|c: char| !c.is_ascii_digit()).unwrap_or(v.len());
    v[..end].parse().ok()
}

/// A length from an attribute (HTML "rules for parsing dimension values"): `85%` and `100` (pixels). A number with
/// a fraction is kept. Other text after the number is ignored, as `100px` is 100.
fn dimension(v: &str) -> Option<String> {
    let v = v.trim_start();
    let digits = v.find(|c: char| !(c.is_ascii_digit() || c == '.')).unwrap_or(v.len());
    let n: f32 = v[..digits].parse().ok().filter(|n: &f32| n.is_finite())?;
    Some(if v[digits..].starts_with('%') { format!("{n}%") } else { format!("{n}px") })
}

#[cfg(test)]
mod tests {
    use super::*;
    use bare_html::tree::parse;

    /// The hint of the element with this `id`.
    fn hint(html: &str, id: &str) -> Option<String> {
        let doc = parse(html);
        let hints = Hints::new(&doc)?;
        let mut stack = vec![doc.root()];
        while let Some(n) = stack.pop() {
            if doc.attr(n, Atom::ID) == Some(id) {
                return hints.of(&doc, n);
            }
            stack.extend(doc.children(n));
        }
        panic!("no element {id}")
    }

    #[test]
    fn table_attributes() {
        let t = hint("<table id=t width=85% cellspacing=3 border=2 align=center>", "t").unwrap();
        assert_eq!(t, "width:85%;border-spacing:3px;border-width:2px;border-style:outset;margin-left:auto;margin-right:auto;");
        assert_eq!(hint("<table id=t width=300 height=50 align=right border>", "t").unwrap(), "width:300px;height:50px;border-width:1px;border-style:outset;float:right;");
        // `border=0` and a word are no border. A page with no hint attribute has none.
        assert_eq!(hint("<table id=t border=0 cellspacing=x>", "t"), None);
        assert_eq!(hint("<table id=t>", "t"), None);
    }

    #[test]
    fn cell_attributes_and_the_table_around() {
        let html = "<table cellpadding=4 border=1><tbody><tr><td id=c width=50 valign=top align=right nowrap>x</td><th id=h>y</table>";
        assert_eq!(hint(html, "c").unwrap(), "width:50px;text-align:right;vertical-align:top;white-space:nowrap;padding:4px;border-width:1px;border-style:inset;");
        assert_eq!(hint(html, "h").unwrap(), "padding:4px;border-width:1px;border-style:inset;");
        // A table with no attribute gives no hint to its cells.
        assert_eq!(hint("<table><tr><td id=c>x</table>", "c"), None);
        // A table inside a cell: the cells inside read their own table.
        let html = "<table cellpadding=9><tr><td><table><tr><td id=c>x</table></table>";
        assert_eq!(hint(html, "c"), None);
    }

    #[test]
    fn an_svg_gets_its_size_and_paint_from_attributes() {
        let h = |html: &str| hint(html, "s").unwrap_or_default();
        assert_eq!(h("<svg id=s width=24 height=24 viewBox='0 0 24 24' fill=none stroke=currentColor>"), "width:24px;height:24px;aspect-ratio:24/24;fill:none;stroke:currentColor;");
        // No attribute: 300 by 150. A viewBox alone: the width of the box. One side and a viewBox: the other side follows the ratio.
        assert_eq!(h("<svg id=s>"), "width:300px;height:150px;");
        assert_eq!(h("<svg id=s viewBox='0 0 20 10'>"), "width:100%;aspect-ratio:20/10;");
        assert_eq!(h("<svg id=s viewBox='0,0,20,10' width=40>"), "width:40px;aspect-ratio:20/10;");
        assert_eq!(h("<svg id=s viewBox='0 0 20 10' height=40>"), "width:80px;height:40px;aspect-ratio:20/10;");
        // A view box with a zero side, or with text in it, is no ratio.
        assert_eq!(h("<svg id=s viewBox='0 0 0 10' width=60>"), "width:60px;height:150px;");
        assert_eq!(h("<svg id=s viewBox='0 0 x 10' width=60>"), "width:60px;height:150px;");
        // The shapes give `fill` and `stroke`. A value that could end the declaration is dropped.
        assert_eq!(h("<svg><path id=s fill=red stroke=#00f d='M0 0'>"), "fill:red;stroke:#00f;display:none;");
        assert_eq!(h("<svg><path id=s fill='red;width:5px'>"), "display:none;");
        // Children of an `svg` or a `g` have no box, except a `g`.
        assert_eq!(h("<svg><g id=s fill=red>"), "fill:red;");
        assert_eq!(h("<svg><g><text id=s>"), "display:none;");
        // A page with no `<table>` and no `<svg>` has no hints.
        assert!(Hints::new(&parse("<p id=s>")).is_none());
    }

    #[test]
    fn rows_and_columns() {
        assert_eq!(hint("<table><tr id=r valign=bottom align=middle><td>", "r").unwrap(), "text-align:center;vertical-align:bottom;");
        assert_eq!(hint("<table><colgroup><col id=c width=40>", "c").unwrap(), "width:40px;");
        assert_eq!(dimension(" 12.5px"), Some("12.5px".into()));
        assert_eq!(dimension("abc"), None);
    }
}
