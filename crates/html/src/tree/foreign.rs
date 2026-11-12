//! Foreign content: `<svg>` and `<math>` in the tree builder, in the way of the HTML standard (13.2.6.5), cut down.
//!
//! What it does:
//! - `<svg>` and `<math>` start tags in the body make an element in the SVG or MathML namespace (`Document::element_ns`).
//! - While the current node is foreign, a token is read by the foreign rules, not the HTML rules. A start tag makes a foreign
//!   element in the namespace of the current node. The tag name gets the case that SVG needs (`foreignobject` becomes
//!   `foreignObject`), the attribute names too (`viewbox` becomes `viewBox`), and `xlink:href`, `xml:lang` and `xmlns` get their namespace.
//! - A self-closing foreign tag (`<path/>`) is an empty element. An HTML tag that cannot be in SVG (`<p>`, `<div>`, `<table>`, a few
//!   more) closes the foreign elements and goes on in HTML. An end tag closes the nearest open foreign element with that name.
//! - The HTML integration points read HTML: `foreignObject`, `desc` and `title` of SVG, `annotation-xml` of MathML with an `encoding`
//!   of `text/html` or `application/xhtml+xml`. The text integration points of MathML (`mi mo mn ms mtext`) read HTML text.
//! - An element in a foreign namespace is never one of the HTML elements that the other rules know by name (`p`, `a`, `table`...).
//!   The scope search stops at an integration point, as the standard says.
//!
//! - Raw text and RCDATA are chosen by the tree builder, not by the tag name: the tokenizer is told with `Tokens::enter_raw` after the
//!   HTML rules made an element (`<svg><title>` holds markup, `<svg><foreignObject><title>` holds RCDATA). CDATA is text where the
//!   current node is not an HTML element (`Tokens::set_foreign`). See `drive` in `tree.rs`. The tests are in `context_tests.rs`.
//!
//! What it does not do (known gaps, on purpose):
//! - The MathML attribute `definitionurl` is adjusted, but no other MathML adjustment exists (the standard has no more).
//! - A fragment with a foreign context element (`fragment.rs`) reads its tokens by these rules while only its root is open: the context is
//!   the adjusted current node (`Builder::adjusted`). An end tag never closes the root.

use super::{Builder, Token};
use crate::dom::Ns;
use crate::tokenizer::Attr;
use std::borrow::Cow;

/// SVG tag names with a capital letter. The tokenizer gives them in lowercase.
const SVG_TAGS: &[&str] = &[
    "altGlyph", "altGlyphDef", "altGlyphItem", "animateColor", "animateMotion", "animateTransform", "clipPath", "feBlend", "feColorMatrix",
    "feComponentTransfer", "feComposite", "feConvolveMatrix", "feDiffuseLighting", "feDisplacementMap", "feDistantLight", "feDropShadow",
    "feFlood", "feFuncA", "feFuncB", "feFuncG", "feFuncR", "feGaussianBlur", "feImage", "feMerge", "feMergeNode", "feMorphology", "feOffset",
    "fePointLight", "feSpecularLighting", "feSpotLight", "feTile", "feTurbulence", "foreignObject", "glyphRef", "linearGradient",
    "radialGradient", "textPath",
];

/// SVG attribute names with a capital letter.
const SVG_ATTRS: &[&str] = &[
    "attributeName", "attributeType", "baseFrequency", "baseProfile", "calcMode", "clipPathUnits", "diffuseConstant", "edgeMode", "filterUnits",
    "glyphRef", "gradientTransform", "gradientUnits", "kernelMatrix", "kernelUnitLength", "keyPoints", "keySplines", "keyTimes", "lengthAdjust",
    "limitingConeAngle", "markerHeight", "markerUnits", "markerWidth", "maskContentUnits", "maskUnits", "numOctaves", "pathLength",
    "patternContentUnits", "patternTransform", "patternUnits", "pointsAtX", "pointsAtY", "pointsAtZ", "preserveAlpha", "preserveAspectRatio",
    "primitiveUnits", "refX", "refY", "repeatCount", "repeatDur", "requiredExtensions", "requiredFeatures", "specularConstant",
    "specularExponent", "spreadMethod", "startOffset", "stdDeviation", "stitchTiles", "surfaceScale", "systemLanguage", "tableValues",
    "targetX", "targetY", "textLength", "viewBox", "viewTarget", "xChannelSelector", "yChannelSelector", "zoomAndPan",
];

/// HTML start tags that end foreign content.
const BREAKOUT: &[&str] = &[
    "b", "big", "blockquote", "body", "br", "center", "code", "dd", "div", "dl", "dt", "em", "embed", "h1", "h2", "h3", "h4", "h5", "h6", "head",
    "hr", "i", "img", "li", "listing", "menu", "meta", "nobr", "ol", "p", "pre", "ruby", "s", "small", "span", "strong", "strike", "sub", "sup",
    "table", "tt", "u", "ul", "var",
];

/// The attributes that are in a namespace: the qualified name and its namespace.
fn attr_namespace(name: &str) -> Option<Ns> {
    match name {
        "xlink:actuate" | "xlink:arcrole" | "xlink:href" | "xlink:role" | "xlink:show" | "xlink:title" | "xlink:type" => Some(Ns::Xlink),
        "xml:lang" | "xml:space" => Some(Ns::Xml),
        "xmlns" | "xmlns:xlink" => Some(Ns::Xmlns),
        _ => None,
    }
}

/// The name with the case of `table`, if the lowercase name is in it.
fn with_case<'a>(name: &'a str, table: &'a [&'a str]) -> &'a str {
    table.iter().find(|t| t.eq_ignore_ascii_case(name)).copied().unwrap_or(name)
}

impl Builder {
    /// Does the foreign content rules read this token? The tree builder asks before the rules of the insertion mode.
    pub(super) fn is_foreign_token(&self, token: &Token<'_>) -> bool {
        if !self.doc.has_foreign() {
            return false;
        }
        let current = self.adjusted();
        let ns = self.doc.element_ns(current);
        if ns == Ns::Html {
            return false;
        }
        let name = self.doc.element_name(current);
        let text_point = ns == Ns::MathMl && matches!(name, "mi" | "mo" | "mn" | "ms" | "mtext");
        match token {
            Token::StartTag { name: tag, .. } => {
                if text_point && !matches!(&**tag, "mglyph" | "malignmark") {
                    return false;
                }
                if ns == Ns::MathMl && name == "annotation-xml" && &**tag == "svg" {
                    return false;
                }
                !self.html_integration_point(current)
            }
            Token::Text(_) => !(text_point || self.html_integration_point(current)),
            _ => true,
        }
    }

    /// Does the tokenizer read `<![CDATA[` as text now? Only when the adjusted current node is not an HTML element. It is the node,
    /// not the insertion point: in `<svg><foreignObject>` it is allowed.
    pub(super) fn cdata_allowed(&self) -> bool {
        self.doc.has_foreign() && !self.open.is_empty() && self.doc.element_ns(self.adjusted()) != Ns::Html
    }

    /// An SVG `foreignObject`, `desc` or `title`, or a MathML `annotation-xml` that says it holds HTML.
    fn html_integration_point(&self, el: crate::dom::NodeId) -> bool {
        let name = self.doc.element_name(el);
        match self.doc.element_ns(el) {
            Ns::Svg => matches!(name, "foreignObject" | "desc" | "title"),
            Ns::MathMl => {
                name == "annotation-xml"
                    && self.doc.find_atom("encoding").and_then(|a| self.doc.attr(el, a)).is_some_and(|v| v.eq_ignore_ascii_case("text/html") || v.eq_ignore_ascii_case("application/xhtml+xml"))
            }
            _ => false,
        }
    }

    /// A node that stops the scope search, because it is a foreign integration point.
    pub(super) fn foreign_scope_border(&self, el: crate::dom::NodeId) -> bool {
        match self.doc.element_ns(el) {
            Ns::Svg => matches!(self.doc.element_name(el), "foreignObject" | "desc" | "title"),
            Ns::MathMl => matches!(self.doc.element_name(el), "mi" | "mo" | "mn" | "ms" | "mtext" | "annotation-xml"),
            _ => false,
        }
    }

    /// Add a foreign element. `name` and `attrs` come from the tokenizer (lowercase).
    pub(super) fn insert_foreign(&mut self, ns: Ns, name: &str, attrs: &[Attr<'_>], self_closing: bool) {
        let name = if ns == Ns::Svg { with_case(name, SVG_TAGS) } else { name };
        let attrs: Vec<(Option<Ns>, Cow<'_, str>, &str)> = attrs
            .iter()
            .map(|(key, value)| {
                let key: &str = match ns {
                    Ns::Svg => with_case(key, SVG_ATTRS),
                    Ns::MathMl if &**key == "definitionurl" => "definitionURL",
                    _ => key,
                };
                (attr_namespace(key), Cow::Borrowed(key), &**value)
            })
            .collect();
        let (parent, before) = self.place();
        let id = self.doc.insert_element_ns(parent, before, ns, name, &attrs);
        if !self_closing {
            self.open.push(id, false);
        }
    }

    /// Read one token by the foreign content rules.
    pub(super) fn foreign(&mut self, token: Token<'_>) {
        match token {
            Token::Text(text) => {
                if text.contains('\0') {
                    self.insert_text(&text.replace('\0', "\u{fffd}"));
                } else {
                    self.insert_text(&text);
                }
            }
            Token::Comment(text) => self.insert_comment(&text),
            Token::Doctype(_) => {}
            Token::StartTag { name, attrs, self_closing } => {
                let font = &*name == "font" && attrs.iter().any(|(k, _)| matches!(&**k, "color" | "face" | "size"));
                if font || BREAKOUT.contains(&&*name) {
                    self.leave_foreign();
                    self.process_html(Token::StartTag { name, attrs, self_closing });
                } else {
                    let ns = self.doc.element_ns(self.adjusted());
                    self.insert_foreign(ns, &name, &attrs, self_closing);
                }
            }
            Token::EndTag { name } => {
                if &*name == "br" || &*name == "p" {
                    self.leave_foreign();
                    self.process_html(Token::EndTag { name });
                    return;
                }
                // The nearest open element with this name (in any case) closes, but only while the elements are foreign.
                // An HTML element on the way hands the token to the HTML rules.
                let mut i = self.open.len();
                while i > 0 {
                    i -= 1;
                    let node = self.open[i];
                    if self.doc.element_ns(node) == Ns::Html {
                        self.process_html(Token::EndTag { name });
                        return;
                    }
                    if self.doc.element_name(node).eq_ignore_ascii_case(&name) {
                        self.open.truncate(i);
                        return;
                    }
                }
            }
        }
    }

    /// Pop foreign elements until the current node is an HTML element or an integration point.
    fn leave_foreign(&mut self) {
        while !self.open.is_empty() {
            let current = self.current();
            if self.doc.element_ns(current) == Ns::Html || self.html_integration_point(current) || self.mathml_text_point(current) {
                break;
            }
            self.open.pop();
        }
    }

    fn mathml_text_point(&self, el: crate::dom::NodeId) -> bool {
        self.doc.element_ns(el) == Ns::MathMl && matches!(self.doc.element_name(el), "mi" | "mo" | "mn" | "ms" | "mtext")
    }
}

#[cfg(test)]
mod tests {
    use crate::dom::{Document, NodeData, NodeId, Ns};
    use crate::tree::parse;

    /// The tree inside `<body>`, in the html5lib text format: `<svg path>` is an SVG element, `xlink href="x"` an attribute with a namespace.
    fn body(page: &str) -> String {
        let full = parse(&format!("<body>{page}")).dump();
        let head: String = ["<html>", "  <head>", "  <body>"].iter().map(|l| format!("| {l}\n")).collect();
        full.strip_prefix(&head).expect("a body").lines().map(|l| format!("{}\n", l.strip_prefix("|     ").unwrap_or(l))).collect()
    }

    fn lines(l: &[&str]) -> String {
        l.iter().map(|l| format!("{l}\n")).collect()
    }

    #[test]
    fn svg_names_and_attributes_get_their_case_and_namespace() {
        assert_eq!(
            body("<svg viewbox='0 0 1 1' PRESERVEASPECTRATIO=none><foreignobject><div>x</div></foreignobject><lineargradient></lineargradient><clippath></clippath></svg>"),
            lines(&[
                "<svg svg>",
                "  preserveAspectRatio=\"none\"",
                "  viewBox=\"0 0 1 1\"",
                "  <svg foreignObject>",
                "    <div>",
                "      \"x\"",
                "  <svg linearGradient>",
                "  <svg clipPath>",
            ])
        );
        assert_eq!(
            body("<svg xmlns='u' xmlns:xlink='x'><use xlink:href='#a' xml:lang='en' xlink:foo='b'/></svg>"),
            lines(&["<svg svg>", "  xmlns xlink=\"x\"", "  xmlns xmlns=\"u\"", "  <svg use>", "    xlink href=\"#a\"", "    xlink:foo=\"b\"", "    xml lang=\"en\""])
        );
        assert_eq!(body("<math definitionurl=x><mi>a</mi></math>"), lines(&["<math math>", "  definitionURL=\"x\"", "  <math mi>", "    \"a\""]));
        // HTML names keep the HTML rules: `viewbox` on a div is not changed
        assert_eq!(body("<div viewbox=1></div>"), lines(&["<div>", "  viewbox=\"1\""]));
    }

    #[test]
    fn self_closing_and_end_tags() {
        assert_eq!(body("<svg/><p>a</p>"), lines(&["<svg svg>", "<p>", "  \"a\""]));
        assert_eq!(body("<svg><path d=M0 /><rect></rect></svg>"), lines(&["<svg svg>", "  <svg path>", "    d=\"M0\"", "  <svg rect>"]));
        // an end tag closes the nearest open element of that name, and what is open inside it
        assert_eq!(body("<svg><g><rect></g>x</svg>"), lines(&["<svg svg>", "  <svg g>", "    <svg rect>", "  \"x\""]));
        // the name is compared in lowercase
        assert_eq!(body("<svg><foreignobject></FOREIGNOBJECT>x</svg>y"), lines(&["<svg svg>", "  <svg foreignObject>", "  \"x\"", "\"y\""]));
        // an end tag with no foreign match reaches the HTML element below and is read by the HTML rules
        assert_eq!(body("<div><svg><g></div>x"), lines(&["<div>", "  <svg svg>", "    <svg g>", "\"x\""]));
        assert_eq!(body("<svg></span>x</svg>"), lines(&["<svg svg>", "  \"x\""]));
    }

    #[test]
    fn html_tags_leave_foreign_content() {
        assert_eq!(body("<svg><rect></rect><p>x</p></svg>y"), lines(&["<svg svg>", "  <svg rect>", "<p>", "  \"x\"", "\"y\""]));
        assert_eq!(body("<svg><g><div>x</div></g></svg>"), lines(&["<svg svg>", "  <svg g>", "<div>", "  \"x\""]));
        assert_eq!(body("<svg><g></br></svg>"), lines(&["<svg svg>", "  <svg g>", "<br>"]));
        assert_eq!(body("<svg><font color=red>x</font></svg>"), lines(&["<svg svg>", "<font>", "  color=\"red\"", "  \"x\""]));
        // `font` with no colour, face or size stays in SVG
        assert_eq!(body("<svg><font>x</font></svg>"), lines(&["<svg svg>", "  <svg font>", "    \"x\""]));
    }

    #[test]
    fn integration_points_read_html() {
        assert_eq!(body("<svg><desc><b>x</b></desc></svg>"), lines(&["<svg svg>", "  <svg desc>", "    <b>", "      \"x\""]));
        assert_eq!(body("<svg><title>t</title></svg>"), lines(&["<svg svg>", "  <svg title>", "    \"t\""]));
        assert_eq!(
            body("<math><mi>x</mi><annotation-xml encoding='text/html'><div>h</div></annotation-xml><mglyph/></math>"),
            lines(&["<math math>", "  <math mi>", "    \"x\"", "  <math annotation-xml>", "    encoding=\"text/html\"", "    <div>", "      \"h\"", "  <math mglyph>"])
        );
        // without the encoding, `div` leaves MathML
        assert_eq!(body("<math><annotation-xml><div>h</div></annotation-xml></math>"), lines(&["<math math>", "  <math annotation-xml>", "<div>", "  \"h\""]));
        // the scope search stops at an integration point: this `p` does not close the outer one
        assert_eq!(
            body("<p>a<svg><foreignObject><p>b</foreignObject></svg>"),
            lines(&["<p>", "  \"a\"", "  <svg svg>", "    <svg foreignObject>", "      <p>", "        \"b\""])
        );
    }

    #[test]
    fn foreign_elements_are_not_the_html_elements_of_the_same_name() {
        // an SVG `a` is not in the list of active formatting elements, and `</a>` after the svg closes the HTML one
        assert_eq!(body("<a href=1>x<svg><a>y</svg>z</a>w"), lines(&["<a>", "  href=\"1\"", "  \"x\"", "  <svg svg>", "    <svg a>", "      \"y\"", "  \"z\"", "\"w\""]));
        // a table inside SVG is a breakout, and a stray `</table>` does not close an SVG element named table
        assert_eq!(body("<svg><table></table></svg>"), lines(&["<svg svg>", "<table>"]));
        // foreign content in a table is moved before the table, as any other content
        assert_eq!(body("<table><svg><rect></svg></table>"), lines(&["<svg svg>", "  <svg rect>", "<table>"]));
    }

    #[test]
    fn namespaces_survive_moves_and_clones_in_the_document() {
        let mut doc = parse("<body><svg><g id=a></g></svg><div></div>");
        let find = |doc: &Document, name: &str| {
            let mut stack = vec![doc.root()];
            while let Some(n) = stack.pop() {
                if matches!(doc.data(n), NodeData::Element { .. }) && doc.element_name(n) == name {
                    return n;
                }
                stack.extend(doc.children(n));
            }
            panic!("no {name}");
        };
        let (g, div, svg) = (find(&doc, "g"), find(&doc, "div"), find(&doc, "svg"));
        assert_eq!((doc.element_ns(svg), doc.element_ns(g), doc.element_ns(div)), (Ns::Svg, Ns::Svg, Ns::Html));
        doc.move_to(g, div, None);
        assert_eq!(doc.element_ns(g), Ns::Svg);
        let c: NodeId = doc.clone_element(g);
        assert_eq!(doc.element_ns(c), Ns::Svg);
        assert_eq!(doc.element_name(c), "g");
        let h = doc.clone_element(div);
        assert_eq!(doc.element_ns(h), Ns::Html);
        assert!(doc.is_html(div, "div") && !doc.is_html(g, "g") && !doc.is_html(c, "g"));
    }

    #[test]
    fn a_page_in_pieces_gives_the_same_foreign_tree() {
        let page = "<body><p>a<svg viewbox='0 0 1 1'><foreignobject><div>x</div></foreignobject><path d='M0'/></svg>b<math><mi>y</mi></math>";
        let whole = parse(page).dump();
        for cut in 1..page.len() {
            let mut parser = crate::tree::Parser::new();
            parser.feed(&page[..cut]);
            parser.feed(&page[cut..]);
            assert_eq!(parser.finish().dump(), whole, "cut at {cut}");
        }
    }

    #[test]
    fn junk_with_foreign_tags_keeps_the_tree_whole() {
        let pieces = [
            "<svg>", "</svg>", "<math>", "</math>", "<g>", "</g>", "<foreignObject>", "</foreignObject>", "<desc>", "</desc>", "<title>", "</title>", "<mi>", "</mi>",
            "<annotation-xml encoding=text/html>", "</annotation-xml>", "<p>", "</p>", "<div>", "</div>", "<table>", "</table>", "<tr>", "<td>", "<a>", "</a>", "<b>", "</b>",
            "<br>", "</br>", "<font color=x>", "<path/>", "x", "\0", "<!--", "-->", "<![CDATA[", "<svg xlink:href=a viewbox=1 xmlns=u>", "<body>", "</body>", "<html>",
        ];
        let mut seed: u32 = 7;
        for _ in 0..6000 {
            let mut page = String::new();
            for _ in 0..(seed % 30) {
                seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                page.push_str(pieces[(seed >> 16) as usize % pieces.len()]);
            }
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            let doc = parse(&page);
            fn check(doc: &Document, id: NodeId) {
                for c in doc.children(id) {
                    assert_eq!(doc.parent(c), Some(id));
                    check(doc, c);
                }
            }
            check(&doc, doc.root());
            let _ = doc.dump();
        }
    }
}
