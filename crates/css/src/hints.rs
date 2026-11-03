//! Presentational hints: the HTML attributes that act as style (HTML standard, "Rendering", tables). `<table width=85%
//! cellpadding=0 border=1 align=center>` and `<td valign=top width=50>` are the common ones in old pages.
//!
//! A hint is a declaration of the author origin with the lowest priority: any rule of the page beats it, and it beats
//! the browser's sheet. `of` gives the declarations as text, like a `style=""` attribute, and the cascade reads them
//! the same way. A cell reads `cellpadding` and `border` of the table around it (the table, and up to three levels
//! above the cell: `td`, `tr`, `tbody`). `<img width height>` gives its size. Only elements of a page that has a `<table>` are looked at.

use bare_html::dom::{Atom, Document, NodeData, NodeId};

/// The atoms of the names that a page can have. A name that the page never uses has no atom, so nothing matches it.
pub struct Hints {
    table: Atom,
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
    /// `None` for a page with no `<table>`: no element has a hint.
    pub fn new(doc: &Document) -> Option<Hints> {
        let a = |n| doc.find_atom(n);
        Some(Hints {
            table: a("table")?,
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
        if name == self.table {
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
            let table = std::iter::successors(doc.parent(node), |&n| doc.parent(n)).take(3).find(|&n| matches!(*doc.data(n), NodeData::Element { name, .. } if name == self.table));
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
        (!out.is_empty()).then_some(out)
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
    fn rows_and_columns() {
        assert_eq!(hint("<table><tr id=r valign=bottom align=middle><td>", "r").unwrap(), "text-align:center;vertical-align:bottom;");
        assert_eq!(hint("<table><colgroup><col id=c width=40>", "c").unwrap(), "width:40px;");
        assert_eq!(dimension(" 12.5px"), Some("12.5px".into()));
        assert_eq!(dimension("abc"), None);
    }
}
