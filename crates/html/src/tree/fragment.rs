//! Fragment parsing: the HTML fragment parsing algorithm (13.4) for `innerHTML`, cut down to the contexts that the tree builder reads correctly.
//!
//! How it works: the same tokenizer and the same tree builder as for a page. The builder starts with the stack `[html]` (the root of a temporary
//! `Document`) in the "in body" mode. The tokenizer starts in the state that the context element needs (raw text, RCDATA, script data, PLAINTEXT).
//! The result is that root and its children. A host copies them where it wants (`Document::import_node`).
//!
//! Supported contexts: an HTML element that the "in body" rules read: `div`, `span`, `body`, `head`, `p`, `ul`, `a`, `button`, `script`, `style`,
//! `textarea`, `title`, `xmp`, `iframe`, `noembed`, `noframes`, `plaintext`, `template`, `noscript`, and every element that is not in the gate list.
//! When the context is in a `form`, `in_form` is true: the form element pointer is set, so a nested `<form>` start tag is ignored.
//! A `template` context starts in the "in template" mode, so `<tr>` and `<td>` keep their tags. The host puts the result in the contents of the template.
//!
//! Table contexts: "reset the insertion mode appropriately" gives "in table" (`table`), "in table body" (`tbody`, `thead`, `tfoot`), "in row" (`tr`)
//! and "in column group" (`colgroup`). The builder has these modes only for a template (see the header of `tree.rs`), so the root of the fragment acts
//! as a template whose template insertion mode is already set to that mode. The rules are the same: in both cases the stack has no table element,
//! so text is not foster parented and an end tag like `</table>` finds no table in scope. "In cell" (`td`, `th`) and "in caption" (`caption`) differ
//! from "in body" only for tags that need a cell or a caption on the stack, and the stack of a fragment has none; so these contexts, and `col`, read as "in body".
//! The 91 html5lib-tests fragment cases with these contexts give the same tree (`html5lib_tests.rs`).
//!
//! The scripting flag (`scripting`) is true when the context is in the document of the page. Then a `noscript` start tag, and the context
//! `noscript`, have raw text. It is false for a context that is a `template` or in the contents of one (an inert document): `noscript` is an element.
//!
//! Foreign contexts: an SVG or MathML context element (`g.innerHTML = '<path/>'`) is the adjusted current node while only the root is open.
//! So the foreign rules read the tokens: `<path>` is an SVG element, and an HTML tag like `<div>` breaks out to HTML. The mode is "in body".
//!
//! Select context: the "in select" mode (classic rules). `option`, `optgroup`, `hr`, text, comments, `script` and `template` stay. Other tags (`div`, `td`) are dropped, and their text stays.
//!
//! Gates (`Unsupported`, nothing is parsed): the HTML contexts `html`, `frameset`, `frame` (the standard needs the before-head and frameset modes). A start tag `frameset` or
//! `frame` in the input is gated the same way, because the builder would make a tree that a browser does not make. Nested `table` and `select`
//! are read by the page rules (see the known gaps of the tree builder): the same tree as for a page.
//!
//! Limits: nothing in the temporary document may pass the caps of `FragmentLimits`. They are checked after each token, so one token can pass them only
//! by its own size (a start tag, a text, or the copies of formatting elements, which are bounded by the nodes that exist). The input has its own cap.
//! `tick` is called after each token with the work it did, so the host can charge a step budget; it can stop the parse.

use super::{Builder, Mode, Tm, Token};
use crate::dom::{Document, NodeId, Ns};
use crate::tokenizer::{Mark, Tokens};

/// The caps of one fragment parse. The counts are for the fragment only (not for the top node and the root `html` of the temporary document).
#[derive(Debug, Clone, Copy)]
pub struct FragmentLimits {
    /// Bytes of the input.
    pub max_source: usize,
    pub max_nodes: usize,
    /// Bytes in the text buffer of the temporary document (it counts the bytes of texts that grew, too).
    pub max_text_bytes: usize,
    pub max_attr_slots: usize,
    pub max_names: usize,
    /// How many elements may be open at once, and so the depth of the fragment.
    pub max_depth: usize,
}

/// Why a fragment was not parsed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FragmentError {
    /// The context or the markup needs something that this parser does not have. The text names it.
    Unsupported(&'static str),
    /// A cap was passed. The text is the name of the cap: `dom-string`, `dom-nodes`, `dom-text`, `dom-attrs`, `dom-names` or `dom-depth`.
    Limit(&'static str),
    /// The host's `tick` said stop (its budget is over).
    Stopped,
}

/// The tree of a parsed fragment: the nodes are the children of `root`.
pub struct Fragment {
    pub doc: Document,
    pub root: NodeId,
}

/// The reason why this context element (an HTML element, by its lowercase name) is not supported, if it is not.
pub fn context_gate(name: &str) -> Option<&'static str> {
    Some(match name {
        "html" | "frameset" | "frame" => "fragment context: before-head, after-head and frameset insertion modes",
        _ => return None,
    })
}

/// The reason why this start tag in a fragment is not supported, if it is not.
fn tag_gate(name: &str) -> Option<&'static str> {
    Some(match name {
        "frameset" | "frame" => "frameset in a fragment",
        _ => return None,
    })
}

/// Parse `input` as the children of an element named `context` in namespace `ns` (lowercase local name for HTML). `in_form` is true when the context
/// element is a `form` or has a `form` ancestor. `scripting` is the scripting flag. Gives the fragment, or says why not. See the header for the gates and the caps.
pub fn parse_fragment(
    input: &str,
    context: &str,
    ns: Ns,
    in_form: bool,
    scripting: bool,
    lim: &FragmentLimits,
    tick: &mut dyn FnMut(u64) -> bool,
) -> Result<Fragment, FragmentError> {
    let html = ns == Ns::Html;
    if html && let Some(why) = context_gate(context) {
        return Err(FragmentError::Unsupported(why));
    }
    if input.len() > lim.max_source {
        return Err(FragmentError::Limit("dom-string"));
    }
    let mut b = Builder::new();
    let root = b.insert_element("html", &[], true);
    b.mode = Mode::InBody;
    // "Reset the insertion mode appropriately" with the context element. A template context is "in template". The table modes reuse the template
    // insertion modes: the root of the fragment acts as a template whose mode is already set. `td`, `th`, `caption` and `col` read as "in body".
    let tm = match context {
        _ if !html => None,
        "template" => Some(Tm::Template),
        "table" => Some(Tm::Table),
        "tbody" | "thead" | "tfoot" => Some(Tm::TableBody),
        "tr" => Some(Tm::Row),
        "colgroup" => Some(Tm::ColGroup),
        _ => None,
    };
    if let Some(tm) = tm {
        b.tmpl.push(tm);
        b.mode = super::mode_for(tm);
    } else if html && context == "select" {
        b.select_fragment = true;
        b.mode = Mode::InSelect;
    }
    b.scripting = scripting;
    if in_form {
        b.form = Some(root);
    }
    // A foreign context (SVG `g`, MathML `mi`) is a copy with no parent: the foreign rules read it as the adjusted current node.
    // Its name keeps its case (`foreignObject`). Its attributes are not copied, so an `annotation-xml` context is never an HTML integration point.
    if !html {
        b.context = Some(b.doc.create_element_ns(ns, None, context));
    }
    let mut tokens = Tokens::for_tree(input, Mark::default(), false);
    // The tokenizer states of the standard are for HTML context elements only: an SVG `style` or `script` holds markup.
    if html && (scripting || context != "noscript") {
        tokens.enter_fragment_raw(context);
    }
    tokens.set_foreign(b.cdata_allowed());
    // The top node, the root `html` and a foreign context are not part of the fragment.
    let base = b.doc.len();
    let mut seen = base;
    while let Some(token) = tokens.next() {
        if let Token::StartTag { name, .. } = &token
            && let Some(why) = tag_gate(name)
        {
            return Err(FragmentError::Unsupported(why));
        }
        b.process(token);
        if let Some(name) = b.raw.take() {
            tokens.enter_raw(name);
        }
        tokens.set_foreign(b.cdata_allowed());
        let (len, made) = (b.doc.len(), b.doc.len() - seen);
        seen = len;
        if !tick(1 + made as u64) {
            return Err(FragmentError::Stopped);
        }
        // The root `html` is on the stack.
        let over = if len > lim.max_nodes + base {
            Some("dom-nodes")
        } else if b.doc.text_bytes() > lim.max_text_bytes {
            Some("dom-text")
        } else if b.doc.attr_slots() > lim.max_attr_slots {
            Some("dom-attrs")
        } else if b.doc.name_count() > lim.max_names {
            Some("dom-names")
        } else if b.open.len() > lim.max_depth + 1 {
            Some("dom-depth")
        } else {
            None
        };
        if let Some(cap) = over {
            return Err(FragmentError::Limit(cap));
        }
    }
    b.finish();
    b.fill_templates();
    Ok(Fragment { doc: b.doc, root })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dom::NodeData;

    const BIG: FragmentLimits = FragmentLimits { max_source: 1 << 20, max_nodes: 1 << 16, max_text_bytes: 1 << 20, max_attr_slots: 1 << 16, max_names: 1 << 12, max_depth: 64 };

    fn parse(input: &str, context: &str) -> Result<Fragment, FragmentError> {
        parse_fragment(input, context, Ns::Html, false, true, &BIG, &mut |_| true)
    }

    /// The children of the root in the html5lib text format (the lines of `Document::dump`, without the root `html` line and one indent level).
    fn tree(input: &str, context: &str) -> String {
        parse(input, context).expect("parsed").doc.dump().lines().skip(1).map(|l| format!("| {}\n", &l[4..])).collect()
    }

    #[test]
    fn a_div_context_reads_body_content_and_ignores_page_tags() {
        assert_eq!(tree("<p>a<p>b", "div"), "| <p>\n|   \"a\"\n| <p>\n|   \"b\"\n");
        assert_eq!(tree("<!doctype html><html><head><body class=x>a</body></html>b", "div"), "| \"ab\"\n");
        assert_eq!(tree("a&amp;b<!--c-->", "span"), "| \"a&b\"\n| <!-- c -->\n");
    }

    #[test]
    fn raw_contexts_start_in_their_tokenizer_state_and_have_no_appropriate_end_tag() {
        assert_eq!(tree("<b>x</b>&amp;</script>y", "script"), "| \"<b>x</b>&amp;</script>y\"\n");
        assert_eq!(tree("<b>&amp;</i>", "textarea"), "| \"<b>&</i>\"\n");
        assert_eq!(tree("<b>&amp;</i>", "style"), "| \"<b>&amp;</i>\"\n");
        assert_eq!(tree("a</plaintext><b>", "plaintext"), "| \"a</plaintext><b>\"\n");
        let f = parse("\nx", "textarea").unwrap();
        let t = f.doc.children(f.root).next().unwrap();
        assert!(matches!(f.doc.data(t), NodeData::Text(sp) if f.doc.text(*sp) == "\nx"));
        assert_eq!(tree("<b>", "title"), "| \"<b>\"\n");
        // an unclosed raw text start tag inside a normal context still ends with the input
        assert_eq!(tree("<script>a<b", "div"), "| <script>\n|   \"a<b\"\n");
    }

    #[test]
    fn foreign_content_inside_a_fragment_keeps_its_namespace() {
        let f = parse("<svg><circle r=1 /></svg><math><mi>x</mi></math>", "div").unwrap();
        let top: Vec<_> = f.doc.children(f.root).collect();
        assert_eq!(top.len(), 2);
        assert_eq!((f.doc.element_ns(top[0]), f.doc.element_ns(top[1])), (Ns::Svg, Ns::MathMl));
        assert!(matches!(f.doc.data(top[0]), NodeData::Element { .. }));
    }

    #[test]
    fn table_contexts_use_the_table_insertion_modes() {
        assert_eq!(tree("<td>a</td>", "tr"), "| <td>\n|   \"a\"\n");
        assert_eq!(tree("<tr><td>a", "tbody"), "| <tr>\n|   <td>\n|     \"a\"\n");
        assert_eq!(tree("<tr><td>a", "thead"), tree("<tr><td>a", "tfoot"));
        // text in a table goes to the end of the fragment: there is no table to foster parent it before
        assert_eq!(tree("x<tr>y", "table"), "| \"x\"\n| <tbody>\n|   <tr>\n| \"y\"\n");
        assert_eq!(tree("<td>a", "table"), "| <tbody>\n|   <tr>\n|     <td>\n|       \"a\"\n");
        assert_eq!(tree("<col> <div>x</div><col>", "colgroup"), "| <col>\n| \" \"\n| <col>\n");
        // cells, captions and `col` read as "in body": table parts are ignored
        for ctx in ["td", "th", "caption"] {
            assert_eq!(tree("<div>a</div><td>b<tr>", ctx), "| <div>\n|   \"a\"\n| \"b\"\n", "{ctx}");
        }
        assert_eq!(tree("<div>a</div>", "col"), "| <div>\n|   \"a\"\n");
    }

    #[test]
    fn a_foreign_context_reads_tokens_by_the_foreign_rules() {
        let run = |input: &str, ctx: &str, ns: Ns| {
            let f = parse_fragment(input, ctx, ns, false, true, &BIG, &mut |_| true).expect("parsed");
            f.doc.dump().lines().skip(1).map(|l| format!("| {}\n", &l[4..])).collect::<String>()
        };
        // the innerHTML of a Codex icon: SVG paths in an SVG `g`
        assert_eq!(run("<path d=\"M0\"/><title>t</title>", "g", Ns::Svg), "| <svg path>\n|   d=\"M0\"\n| <svg title>\n|   \"t\"\n");
        // an end tag never closes the root; an HTML tag breaks out to HTML
        assert_eq!(run("</g><lineargradient/><div>x</div>", "g", Ns::Svg), "| <svg linearGradient>\n| <div>\n|   \"x\"\n");
        // an SVG `style` is not raw text, and CDATA is text
        assert_eq!(run("<b>x</b><![CDATA[<y>]]>", "style", Ns::Svg), "| <b>\n|   \"x\"\n| \"<y>\"\n");
        // a MathML text integration point reads HTML; `mi` holds MathML
        assert_eq!(run("<i>x</i>", "mi", Ns::MathMl), "| <i>\n|   \"x\"\n");
        assert_eq!(run("<mi>x</mi>", "math", Ns::MathMl), "| <math mi>\n|   \"x\"\n");
    }

    #[test]
    fn a_select_context_reads_the_select_mode() {
        assert_eq!(tree("<option>1<option>2<optgroup><option>3<optgroup>4", "select"), "| <option>\n|   \"1\"\n| <option>\n|   \"2\"\n| <optgroup>\n|   <option>\n|     \"3\"\n| <optgroup>\n|   \"4\"\n");
        assert_eq!(tree("<div>a</div><td>b</td><select><input><hr>c", "select"), "| \"ab\"\n| <hr>\n| \"c\"\n");
    }

    #[test]
    fn the_gates_refuse_before_parsing() {
        for name in ["html", "frameset", "frame"] {
            assert!(matches!(parse("x", name), Err(FragmentError::Unsupported(_))), "{name}");
        }
        for html in ["<frameset>", "<frame>"] {
            assert!(matches!(parse(html, "div"), Err(FragmentError::Unsupported(_))), "{html}");
        }
        assert!(parse("&lt;template&gt;<!--<noscript>--><style><template></style>", "div").is_ok());
    }

    #[test]
    fn noscript_is_raw_text_when_scripting_is_on_and_an_element_when_it_is_off() {
        let run = |input: &str, ctx: &str, scripting: bool| {
            let f = parse_fragment(input, ctx, Ns::Html, false, scripting, &BIG, &mut |_| true).expect("parsed");
            f.doc.dump().lines().skip(1).map(|l| format!("| {}\n", &l[4..])).collect::<String>()
        };
        assert_eq!(run("<noscript><p>a&amp;</noscript><i>", "div", true), "| <noscript>\n|   \"<p>a&amp;\"\n| <i>\n");
        assert_eq!(run("<noscript><b>a&amp;</b></noscript><i>", "div", false), "| <noscript>\n|   <b>\n|     \"a&\"\n| <i>\n");
        // the context `noscript`: text only with scripting; without it, `</noscript>` is a stray end tag (the open `p` is special, so it is ignored)
        assert_eq!(run("<p>x</noscript>y", "noscript", true), "| \"<p>x</noscript>y\"\n");
        assert_eq!(run("<p>x</noscript>y", "noscript", false), "| <p>\n|   \"xy\"\n");
        // a `template` context reads like `div`
        assert_eq!(run("<b>x</b>y", "template", false), "| <b>\n|   \"x\"\n| \"y\"\n");
    }

    #[test]
    fn the_children_of_a_template_go_into_its_contents() {
        let f = parse("<template><b>x</b><template><i></i></template></template><template></template>", "div").unwrap();
        let top: Vec<_> = f.doc.children(f.root).collect();
        assert_eq!(top.len(), 2);
        assert!(f.doc.children(top[0]).next().is_none());
        let c = f.doc.template_content(top[0]).expect("contents");
        let kids: Vec<_> = f.doc.children(c).collect();
        assert_eq!((kids.len(), f.doc.element_name(kids[0])), (2, "b"));
        assert!(f.doc.template_content(kids[1]).is_some());
        assert!(f.doc.template_content(top[1]).is_none());
    }

    #[test]
    fn a_template_context_keeps_table_parts() {
        let t = |input: &str| {
            let f = parse(input, "template").unwrap();
            f.doc.dump_with_contents().lines().skip(1).map(|l| format!("| {}\n", &l[4..])).collect::<String>()
        };
        assert_eq!(t("<tr><td>a</td></tr>"), "| <tr>\n|   <td>\n|     \"a\"\n");
        assert_eq!(t("<td>a</td><tr><td>b"), "| <td>\n|   \"a\"\n| <td>\n|   \"b\"\n");
        assert_eq!(t("<col><div>x"), "| <col>\n");
        // the text in a row goes to the end of the fragment, as the standard says when there is no table
        assert_eq!(t("x<tr>y"), "| \"x\"\n| <tr>\n| \"y\"\n");
        assert_eq!(t("<tr><td>a</td></tr></template><td>b"), "| <tr>\n|   <td>\n|     \"a\"\n| <tr>\n|   <td>\n|     \"b\"\n");
        // a `div` context still drops them
        assert_eq!(tree("<tr><td>a</td></tr>", "div"), "| \"a\"\n");
    }

    #[test]
    fn a_form_in_a_form_is_ignored_with_the_form_pointer() {
        let r = parse_fragment("<form>a</form>b", "div", Ns::Html, true, true, &BIG, &mut |_| true).unwrap();
        assert_eq!(r.doc.children(r.root).count(), 1);
        assert_eq!(parse("<form>a</form>b", "div").unwrap().doc.children(parse("<form>a</form>b", "div").unwrap().root).count(), 2);
    }

    #[test]
    fn caps_stop_the_parse_early_and_tick_can_stop_it() {
        let lim = |f: fn(&mut FragmentLimits)| {
            let mut l = BIG;
            f(&mut l);
            l
        };
        let run = |input: &str, l: &FragmentLimits| parse_fragment(input, "div", Ns::Html, false, true, l, &mut |_| true).err();
        assert_eq!(run(&"<i></i>".repeat(50), &lim(|l| l.max_nodes = 40)), Some(FragmentError::Limit("dom-nodes")));
        assert_eq!(run(&"<i></i>".repeat(20), &lim(|l| l.max_nodes = 20)), None);
        assert_eq!(run(&"x".repeat(500), &lim(|l| l.max_text_bytes = 100)), Some(FragmentError::Limit("dom-text")));
        assert_eq!(run("<i a=1 b=2 c=3>", &lim(|l| l.max_attr_slots = 2)), Some(FragmentError::Limit("dom-attrs")));
        assert_eq!(run("<i a=1 b=2 c=3>", &lim(|l| l.max_names = 3)), Some(FragmentError::Limit("dom-names")));
        assert_eq!(run(&"<div>".repeat(10), &lim(|l| l.max_depth = 9)), Some(FragmentError::Limit("dom-depth")));
        assert_eq!(run(&"<div>".repeat(9), &lim(|l| l.max_depth = 9)), None);
        assert_eq!(run("abcdef", &lim(|l| l.max_source = 5)), Some(FragmentError::Limit("dom-string")));
        let mut budget = 10u64;
        let stopped = parse_fragment(&"<i>".repeat(100), "div", Ns::Html, false, true, &BIG, &mut |n| {
            budget = budget.saturating_sub(n);
            budget > 0
        });
        assert_eq!(stopped.err(), Some(FragmentError::Stopped));
    }

    #[test]
    fn import_node_copies_names_namespaces_prefixes_and_attribute_namespaces() {
        let f = parse("<svg viewBox=\"0 0 1 1\"><a xlink:href=\"#h\" xml:lang=en>t</a></svg>", "div").unwrap();
        let mut target = Document::new();
        let svg = f.doc.children(f.root).next().unwrap();
        let a = f.doc.children(svg).next().unwrap();
        assert!(f.doc.new_names_in(&target) >= 4);
        let (s2, a2) = (target.import_node(&f.doc, svg).unwrap(), target.import_node(&f.doc, a).unwrap());
        let root = target.root();
        target.move_to(s2, root, None);
        target.move_to(a2, s2, None);
        assert_eq!((target.element_ns(s2), target.element_ns(a2), target.element_name(a2)), (Ns::Svg, Ns::Svg, "a"));
        assert_eq!(target.find_attr_ns(a2, Some(Ns::Xlink), "href").map(|n| target.attr(a2, n)), Some(Some("#h")));
        assert_eq!(target.find_attr_ns(a2, Some(Ns::Xml), "lang").map(|n| target.attr(a2, n)), Some(Some("en")));
        assert_eq!(target.find_attr_ns(s2, None, "viewBox").map(|n| target.attr(s2, n)), Some(Some("0 0 1 1")));
        // only the name of the root `html` of the temporary document was not copied
        assert_eq!(f.doc.new_names_in(&target), 1);
        assert!(target.import_node(&f.doc, f.doc.root()).is_none());
    }
}
