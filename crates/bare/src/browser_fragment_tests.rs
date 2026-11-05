//! Tests of `Element.prototype.innerHTML` (`browser_fragment.rs`). Expectations are written by hand from the HTML Standard
//! (fragment parsing, fragment serialization, the script "already started" flag) and from WebIDL (brand check, null handling,
//! ToString order). Node has no DOM, so none of this is an oracle, and nothing here was compared with Chrome.
//! The last tests run real pages through the loader to show that fragment scripts stay inert.

use crate::browser::*;
use crate::browser_fetch::{FetchConfig, FetchLimits};
use crate::browser_loader::*;
use crate::resources::test_server::{Reply, Server, serve};
use bare_html::tree;
use bare_js::eval::{Error, EvalLimits, LimitKind};
use bare_js::heap::HeapLimits;
use bare_js::value::Value;
use bare_net::http::Client;
use std::time::Duration;

fn big<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(64 << 20).spawn(f).unwrap().join().unwrap()
}

const PAGE: &str = "<!doctype html><html><head><title>t</title></head><body><div id=a><p id=p>hi</p></div></body></html>";

fn browser_with(page: &str, lim: BrowserLimits, steps: u64) -> Browser {
    let eval = EvalLimits { max_steps: steps, ..Default::default() };
    Browser::new(tree::parse(page), lim, eval, HeapLimits::default()).expect("browser")
}

fn browser() -> Browser {
    browser_with(PAGE, BrowserLimits::default(), 5_000_000)
}

fn js(b: &mut Browser, src: &str) -> String {
    match b.run_script(src) {
        Ok(v) => b.it.display(v),
        Err(Error::Throw(v)) if matches!(v, Value::Object(_)) => format!("throw:{}:{}", b.it.get_str(v, "name"), b.it.get_str(v, "message")),
        Err(Error::Throw(v)) => format!("throw-value:{}", b.it.display(v)),
        Err(Error::Limit(l)) => format!("limit:{l:?}"),
        Err(e) => format!("other:{e:?}"),
    }
}

fn lim(name: &'static str) -> String {
    format!("limit:{:?}", LimitKind::Host(name))
}

/// Set `innerHTML` of a fresh detached `div` to `html` (a JS string literal body) and read it back.
fn round(b: &mut Browser, html: &str) -> String {
    js(b, &format!("var t = document.createElement('div'); t.innerHTML = '{html}'; t.innerHTML"))
}

// ---------------------------------------------------------------------------------------------- the React shape

#[test]
fn the_react_script_shape_gives_a_detachable_native_script() {
    let mut b = browser();
    // The exact expression of the cached bundle.
    assert_eq!(
        js(&mut b, "var e; (e = document.createElement('div')).innerHTML = '<script></script>'; e = e.removeChild(e.firstChild); [e.tagName, e.namespaceURI, e.parentNode === null, e.childNodes.length].join()"),
        "SCRIPT,http://www.w3.org/1999/xhtml,true,0"
    );
    assert_eq!(js(&mut b, "var d = document.createElement('div'); d.innerHTML = '<script></script>'; var s = d.firstChild; [s.nodeType, s.parentNode === d, d.childNodes.length, d.removeChild(s) === s, d.firstChild].join()"), "1,true,1,true,");
    // Still the same native script when it moves into the document.
    assert_eq!(js(&mut b, "document.body.appendChild(s); [s.isConnected, s.parentNode === document.body, document.getElementsByTagName('script').length].join()"), "true,true,1");
}

#[test]
fn the_property_is_an_accessor_on_element_only() {
    let mut b = browser();
    assert_eq!(js(&mut b, "var d = Object.getOwnPropertyDescriptor(Element.prototype, 'innerHTML'); [typeof d.get, typeof d.set, d.enumerable, d.configurable, d.get.length, d.set.length].join()"), "function,function,true,true,0,1");
    assert_eq!(js(&mut b, "[typeof document.innerHTML, 'innerHTML' in Document.prototype, 'innerHTML' in Node.prototype, typeof document.createTextNode('x').innerHTML, typeof document.createElement('div').innerHTML].join()"), "undefined,false,false,undefined,string");
}

// ---------------------------------------------------------------------------------------------- parsing and serialization

#[test]
fn ordinary_html_entities_comments_and_text_round_trip() {
    let mut b = browser();
    assert_eq!(round(&mut b, "a&amp;b &lt;i&gt; &nbsp;&copy;<!--c-->z"), "a&amp;b &lt;i&gt; &nbsp;\u{a9}<!--c-->z");
    assert_eq!(round(&mut b, "<p class=\"a b\" id=x>t</p>"), "<p class=\"a b\" id=\"x\">t</p>");
    assert_eq!(round(&mut b, "<a href=\"?a=1&amp;b=2&quot;\" title=&lt;&gt;>x</a>"), "<a href=\"?a=1&amp;b=2&quot;\" title=\"&lt;&gt;\">x</a>");
    // The attribute `title="<>"` is escaped as `&lt;&gt;` by the current serialization rules (to verify against a browser).
    assert_eq!(js(&mut b, "t.firstChild.getAttribute('title') + '|' + t.firstChild.getAttribute('href')"), "<>|?a=1&b=2\"");
}

#[test]
fn void_elements_have_no_end_tag_and_rawtext_is_not_escaped() {
    let mut b = browser();
    assert_eq!(round(&mut b, "<br><img src=x alt=\"\"><hr><input disabled><wbr>"), "<br><img src=\"x\" alt=\"\"><hr><input disabled=\"\"><wbr>");
    assert_eq!(round(&mut b, "<script>if (a<b && c>d) { x = '&amp;' }</script><style>p>a{content:'&amp;'}</style>"), "<script>if (a<b && c>d) { x = '&amp;' }</script><style>p>a{content:'&amp;'}</style>");
    assert_eq!(js(&mut b, "t.firstChild.textContent + '|' + t.lastChild.textContent"), "if (a<b && c>d) { x = '&amp;' }|p>a{content:'&amp;'}");
    // Markup made through the DOM API serializes by the same rules.
    assert_eq!(
        js(&mut b, "var q = document.createElement('div'); q.appendChild(document.createTextNode('1 < 2 & 3 > 0\\u00a0.')); var sc = q.appendChild(document.createElement('script')); sc.appendChild(document.createTextNode('a<b&c')); q.appendChild(document.createComment('k')); q.appendChild(document.createElement('br')); q.innerHTML"),
        "1 &lt; 2 &amp; 3 &gt; 0&nbsp;.<script>a<b&c</script><!--k--><br>"
    );
    assert_eq!(js(&mut b, "document.createElement('div').innerHTML"), "");
}

#[test]
fn nested_and_misnested_markup_follows_the_tree_builder() {
    let mut b = browser();
    assert_eq!(round(&mut b, "<p>a<p>b"), "<p>a</p><p>b</p>");
    assert_eq!(round(&mut b, "<ul><li>1<li>2</ul></div></span>"), "<ul><li>1</li><li>2</li></ul>");
    assert_eq!(round(&mut b, "<b>1<p>2</b>3</p>"), "<b>1</b><p><b>2</b>3</p>");
    assert_eq!(round(&mut b, "<table><tr><td>x</table>"), "<table><tbody><tr><td>x</td></tr></tbody></table>");
    assert_eq!(round(&mut b, "<table>x<tr><td>y</table>"), "x<table><tbody><tr><td>y</td></tr></tbody></table>");
    // The html, head and body tags and the doctype are not part of a fragment.
    assert_eq!(round(&mut b, "<!doctype html><html><head><body class=x>a</body></html><!--c-->b"), "a<!--c-->b");
    assert_eq!(round(&mut b, "<title>t &amp; <b></title>"), "<title>t &amp; &lt;b&gt;</title>");
    assert_eq!(js(&mut b, "t.firstChild.textContent"), "t & <b>");
    // A form inside a form is ignored only when the context is inside a form.
    assert_eq!(round(&mut b, "<form id=x>a</form>b"), "<form id=\"x\">a</form>b");
    assert_eq!(js(&mut b, "var f = document.createElement('form'), g = f.appendChild(document.createElement('div')); g.innerHTML = '<form id=x>a</form>b'; g.innerHTML"), "ab");
}

#[test]
fn svg_and_mathml_keep_their_namespaces_through_the_import() {
    let mut b = browser();
    let svg = "http://www.w3.org/2000/svg";
    let xlink = "http://www.w3.org/1999/xlink";
    assert_eq!(
        js(&mut b, "var t = document.createElement('div'); t.innerHTML = '<svg viewBox=\"0 0 1 1\"><circle r=1 /><a xlink:href=\"#h\"></a><foreignObject><p>x</p></foreignObject></svg><math><mi>x</mi></math>'; t.innerHTML"),
        "<svg viewBox=\"0 0 1 1\"><circle r=\"1\"></circle><a xlink:href=\"#h\"></a><foreignObject><p>x</p></foreignObject></svg><math><mi>x</mi></math>"
    );
    assert_eq!(
        js(&mut b, &format!("var s = t.firstChild; [s.namespaceURI, s.firstChild.namespaceURI, s.getAttribute('viewBox'), s.childNodes[1].getAttributeNS('{xlink}', 'href'), s.childNodes[1].getAttribute('href'), s.lastChild.firstChild.namespaceURI, t.lastChild.namespaceURI, t.lastChild.firstChild.localName, s.tagName, s.lastChild.tagName].join()")),
        format!("{svg},{svg},0 0 1 1,#h,null,http://www.w3.org/1999/xhtml,http://www.w3.org/1998/Math/MathML,mi,svg,foreignObject")
    );
    // A selector sees the SVG element by its (case-kept) local name.
    assert_eq!(js(&mut b, "t.querySelectorAll('circle, mi, p').length"), "3");
}

#[test]
fn raw_text_and_rcdata_contexts_are_parser_controlled() {
    let mut b = browser();
    // script data: no references, and an end tag does not end it (no start tag was seen).
    assert_eq!(js(&mut b, "var s = document.createElement('script'); s.innerHTML = '<b>x</b>&amp;</script>y'; [s.childNodes.length, s.textContent, s.innerHTML].join('|')"), "1|<b>x</b>&amp;</script>y|<b>x</b>&amp;</script>y");
    assert_eq!(js(&mut b, "var s2 = document.createElement('style'); s2.innerHTML = '<b>&lt;'; s2.textContent"), "<b>&lt;");
    for name in ["xmp", "iframe", "noembed", "noframes"] {
        assert_eq!(js(&mut b, &format!("var r = document.createElement('{name}'); r.innerHTML = '<b>&lt;</i>'; r.childNodes.length + r.textContent")), "1<b>&lt;</i>", "{name}");
    }
    // RCDATA: references work, tags do not. A leading newline stays (it is dropped only after a start tag).
    assert_eq!(js(&mut b, "var ta = document.createElement('textarea'); ta.innerHTML = '\\n<b>&lt;&amp;</b>'; [ta.childNodes.length, ta.textContent].join('|')"), "1|\n<b><&</b>");
    assert_eq!(js(&mut b, "ta.innerHTML"), "\n&lt;b&gt;&lt;&amp;&lt;/b&gt;");
    assert_eq!(js(&mut b, "var ti = document.createElement('title'); ti.innerHTML = 'a&amp;<i>'; ti.textContent"), "a&<i>");
    assert_eq!(js(&mut b, "var pt = document.createElement('plaintext'); pt.innerHTML = 'a</plaintext><b>&amp;'; pt.childNodes.length + pt.textContent"), "1a</plaintext><b>&amp;");
    // An element that is not one of these reads markup.
    assert_eq!(js(&mut b, "var dv = document.createElement('span'); dv.innerHTML = '<b>&amp;</b>'; dv.firstChild.tagName + dv.textContent"), "B&");
}

// ---------------------------------------------------------------------------------------------- gates

#[test]
fn unsupported_contexts_fail_before_any_change() {
    for name in ["table", "thead", "tbody", "tfoot", "tr", "td", "th", "caption", "colgroup", "select", "template", "html", "frameset"] {
        let mut b = browser();
        let before = b.document().borrow().len();
        let r = js(&mut b, &format!("var e = document.createElement('{name}'); var k = e.appendChild(document.createElement('i')); e.innerHTML = '<b>x</b>'; 'done'"));
        assert!(r.starts_with("other:Unsupported"), "{name}: {r}");
        // The old child is still there. Only the two nodes of the script were made.
        assert_eq!(b.document().borrow().len(), before + 2, "{name}");
        assert_eq!(js(&mut b, "[e.childNodes.length, k.parentNode === e].join()"), "1,true", "{name}");
        // Unsupported is not a JS exception: try/catch/finally cannot see it.
        let mut b = browser();
        let r = js(&mut b, &format!("var fin = 0, caught = 0; try {{ document.createElement('{name}').innerHTML = ''; }} catch (x) {{ caught = 1 }} finally {{ fin = 1 }}"));
        assert!(r.starts_with("other:Unsupported"), "{name}: {r}");
        assert_eq!(js(&mut b, "fin + ',' + caught"), "0,0", "{name}");
    }
    // A foreign context element is refused too, also an SVG `title` or a MathML element.
    for src in ["document.createElementNS('http://www.w3.org/2000/svg', 'g')", "document.createElementNS('http://www.w3.org/1998/Math/MathML', 'mi')", "document.createElementNS('http://www.w3.org/2000/svg', 'foreignObject')"] {
        let mut b = browser();
        assert!(js(&mut b, &format!("var e = {src}; e.appendChild(document.createElement('i')); e.innerHTML = 'x'")).starts_with("other:Unsupported"), "{src}");
        assert_eq!(js(&mut b, "e.childNodes.length + e.firstChild.tagName"), "1I", "{src}");
    }
}

#[test]
fn markup_the_tree_builder_cannot_read_correctly_is_refused_before_any_change() {
    for html in ["<p>a</p><template><b></b></template>", "<noscript><p>x</p></noscript>", "<frameset><frame></frameset>", "<div><template>"] {
        let mut b = browser();
        let r = js(&mut b, &format!("var e = document.getElementById('a'); e.innerHTML = '{html}'"));
        assert!(r.starts_with("other:Unsupported"), "{html}: {r}");
        assert_eq!(js(&mut b, "e.childNodes.length + e.firstChild.id"), "1p", "{html}");
    }
    // These words are plain text or comments, so they are fine.
    let mut b = browser();
    assert_eq!(round(&mut b, "&lt;template&gt;<!--<noscript>-->"), "&lt;template&gt;<!--<noscript>-->");
}

#[test]
fn getter_refuses_template_and_noscript_in_the_tree() {
    let mut b = browser();
    for name in ["template", "noscript"] {
        let r = js(&mut b, &format!("var q = document.createElement('div'); q.appendChild(document.createElement('{name}')); q.innerHTML"));
        assert!(r.starts_with("other:Unsupported"), "{name}: {r}");
    }
}

// ---------------------------------------------------------------------------------------------- replacement

#[test]
fn replacing_children_detaches_them_and_keeps_wrappers_listeners_and_attrs_valid() {
    let mut b = browser();
    assert_eq!(
        js(
            &mut b,
            "var d = document.createElement('div'); d.innerHTML = '<span id=k class=c>one</span><b>two</b>'; var k = d.firstChild, kb = d.lastChild, tx = k.firstChild, at = k.getAttributeNode('class'); \
             var hit = 0; k.addEventListener('ping', function () { hit++ }); \
             d.innerHTML = '<i>new</i>'; \
             [k.parentNode, kb.parentNode, tx.parentNode === k, k.nextSibling, kb.previousSibling, k.isConnected, d.childNodes.length, d.firstChild === k, d.firstChild.tagName].join()"
        ),
        ",,true,,,false,1,false,I"
    );
    assert_eq!(js(&mut b, "k.dispatchEvent(new Event('ping')); [hit, k.id, k.className, k.textContent, at.ownerElement === k, k.getAttributeNode('class') === at, at.value, k.getRootNode() === k].join()"), "1,k,c,one,true,true,c,true");
    // The detached subtree can go back in.
    assert_eq!(js(&mut b, "d.appendChild(k); [d.firstChild.tagName, d.lastChild === k, k.parentNode === d].join()"), "I,true,true");
}

#[test]
fn an_attached_element_replaces_its_children_and_the_page_sees_them() {
    let mut b = browser();
    assert_eq!(js(&mut b, "var a = document.getElementById('a'), old = document.getElementById('p'); a.innerHTML = '<em id=n>x</em>tail'; [document.getElementById('p'), document.getElementById('n').parentNode === a, a.childNodes.length, old.isConnected, old.parentNode, a.querySelector('em').textContent].join()"), ",true,2,false,,x");
    assert_eq!(js(&mut b, "a.innerHTML = ''; a.childNodes.length + ',' + a.innerHTML + ',' + a.firstChild"), "0,,null");
    // The element is its own context: replacing the content of body keeps head and the element itself.
    assert_eq!(js(&mut b, "document.body.innerHTML = '<p id=q>z</p>'; [document.body.children.length, document.getElementById('a'), document.head.firstChild.tagName].join()"), "1,,TITLE");
}

// ---------------------------------------------------------------------------------------------- coercion and brand

#[test]
fn null_is_empty_and_everything_else_goes_through_tostring() {
    let mut b = browser();
    assert_eq!(js(&mut b, "var d = document.createElement('div'); d.innerHTML = 'x'; d.innerHTML = null; d.childNodes.length + ',' + d.innerHTML"), "0,");
    assert_eq!(js(&mut b, "d.innerHTML = undefined; d.innerHTML"), "undefined");
    assert_eq!(js(&mut b, "d.innerHTML = 5; d.innerHTML"), "5");
    assert_eq!(js(&mut b, "d.innerHTML = { toString: function () { return '<i>o</i>' } }; d.innerHTML"), "<i>o</i>");
    assert_eq!(js(&mut b, "d.innerHTML = [1, 2]; d.innerHTML"), "1,2");
    assert_eq!(js(&mut b, "var r = (d.innerHTML = '<b>'); r"), "<b>");
    assert_eq!(js(&mut b, "try { d.innerHTML = Symbol('s') } catch (e) { e.name }"), "TypeError");
    assert_eq!(js(&mut b, "d.innerHTML"), "<b></b>", "unchanged after a TypeError");
}

#[test]
fn tostring_runs_before_the_target_is_read_and_may_change_it() {
    let mut b = browser();
    // The conversion may add and remove children of the target, or throw. The final state follows the setter's own result.
    assert_eq!(
        js(&mut b, "var d = document.createElement('div'); var extra = d.appendChild(document.createElement('u')); d.innerHTML = { toString: function () { d.appendChild(document.createElement('s')); d.removeChild(extra); return '<i></i>' } }; [d.innerHTML, extra.parentNode].join()"),
        "<i></i>,"
    );
    // Moving the target into the document inside toString changes nothing about the result.
    assert_eq!(js(&mut b, "d.innerHTML = { toString: function () { document.body.appendChild(d); return '<q>1</q>' } }; [d.innerHTML, d.isConnected].join()"), "<q>1</q>,true");
    // A throwing toString leaves the children as they were, and the error is the page's own.
    assert_eq!(js(&mut b, "var kept = d.firstChild; var m = 'none'; try { d.innerHTML = { toString: function () { throw new RangeError('boom') } } } catch (e) { m = e.name + e.message } [m, d.firstChild === kept].join()"), "RangeErrorboom,true");
    // A lone surrogate cannot be stored: Unsupported, and nothing changed (valid pairs are fine).
    assert_eq!(js(&mut b, "d.innerHTML = '\\ud83d\\ude00'; d.textContent.length"), "2");
    let r = js(&mut b, "var kept2 = d.firstChild; d.innerHTML = 'a\\ud800b'");
    assert!(r.starts_with("other:Unsupported"), "{r}");
    assert_eq!(js(&mut b, "[d.firstChild === kept2, d.textContent.length].join()"), "true,2");
}

#[test]
fn the_brand_is_checked_before_anything_runs() {
    let mut b = browser();
    for this in ["{}", "document", "document.createTextNode('x')", "document.createComment('x')", "document.getElementById('a').getAttributeNode('id')", "window", "null", "undefined", "Element.prototype"] {
        assert_eq!(
            js(&mut b, &format!("var ran = 0, ds = Object.getOwnPropertyDescriptor(Element.prototype, 'innerHTML'); var out = []; try {{ ds.set.call({this}, {{ toString: function () {{ ran++; return 'x' }} }}) }} catch (e) {{ out.push(e.name) }} try {{ ds.get.call({this}) }} catch (e) {{ out.push(e.name) }} out.join() + ran")),
            "TypeError,TypeError0",
            "{this}"
        );
    }
    // An Attr is not an Element: no accessor, and its owner is untouched.
    assert_eq!(js(&mut b, "var at = document.getElementById('a').getAttributeNode('id'); [typeof at.innerHTML, document.getElementById('a').innerHTML.indexOf('<p id=\"p\">')].join()"), "undefined,0");
}

// ---------------------------------------------------------------------------------------------- limits

#[test]
fn caps_fail_before_the_target_changes_and_cannot_be_caught() {
    let cases: Vec<(&str, BrowserLimits, String, &'static str)> = vec![
        ("nodes", BrowserLimits { max_nodes: 30, ..Default::default() }, "'<i></i>'.repeat(40)".into(), "dom-nodes"),
        ("text", BrowserLimits { max_text_bytes: 400, ..Default::default() }, "'<i>' + 'x'.repeat(500) + '</i>'".into(), "dom-text"),
        ("attrs", BrowserLimits { max_attr_slots: 20, ..Default::default() }, "'<i a=1 b=2 c=3 d=4 e=5 f=6 g=7 h=8 j=9 k=0 l=1 m=2 n=3 o=4 p=5 q=6 r=7 s=8 t=9 u=0 v=1 w=2></i>'".into(), "dom-attrs"),
        ("names", BrowserLimits { max_names: 40, ..Default::default() }, "'<i ' + Array.from({ length: 80 }, function (_, i) { return 'n' + i + '=1' }).join(' ') + '></i>'".into(), "dom-names"),
        ("depth", BrowserLimits { max_tree_depth: 6, ..Default::default() }, "'<div>'.repeat(7)".into(), "dom-depth"),
        ("string", BrowserLimits { max_string_bytes: 100, ..Default::default() }, "'x'.repeat(200)".into(), "dom-string"),
    ];
    for (what, limits, expr, kind) in cases {
        let mut b = browser_with(PAGE, limits, 5_000_000);
        let src = format!("var d = document.createElement('div'); d.innerHTML = 'k'; var k = d.firstChild; var fin = 0, caught = 0, big = {expr}; try {{ d.innerHTML = big }} catch (e) {{ caught = 1 }} finally {{ fin = 1 }}");
        assert_eq!(js(&mut b, &src), lim(kind), "{what}");
        // The old child is still in place, and neither catch nor finally ran.
        assert_eq!(js(&mut b, "[d.firstChild === k, d.childNodes.length, d.textContent, fin, caught].join()"), "true,1,k,0,0", "{what}");
    }
}

#[test]
fn the_depth_bound_counts_the_depth_of_the_target() {
    let limits = BrowserLimits { max_tree_depth: 6, ..Default::default() };
    let mut b = browser_with(PAGE, limits, 5_000_000);
    // A detached target is at depth 0: six levels fit, seven do not (the same rule as `appendChild`).
    assert_eq!(js(&mut b, "var d = document.createElement('div'); d.innerHTML = '<div>'.repeat(6); var n = 0, c = d; while ((c = c.firstChild)) n++; n"), "6");
    // `#a` is at depth 3 (html, body, div): only three levels fit under it.
    assert_eq!(js(&mut b, "var a = document.getElementById('a'); a.innerHTML = '<div>'.repeat(3); a.firstChild.firstChild.firstChild.tagName"), "DIV");
    assert_eq!(js(&mut b, "a.innerHTML = '<div>'.repeat(4)"), lim("dom-depth"));
    assert_eq!(js(&mut b, "a.firstChild.firstChild.firstChild.tagName"), "DIV");
    // Text counts as one level, like an appended text node.
    assert_eq!(js(&mut b, "a.innerHTML = '<i><i><i>x'"), lim("dom-depth"));
}

#[test]
fn a_small_step_budget_stops_a_large_parse_before_it_runs() {
    let mut b = browser_with(PAGE, BrowserLimits::default(), 200_000);
    assert_eq!(js(&mut b, "var d = document.createElement('div'); var s = '<i>x</i>'.repeat(10); d.innerHTML = s; d.childNodes.length"), "10");
    let nodes = b.document().borrow().len();
    let r = js(&mut b, "var big = '<b>1</b>'.repeat(120000); var kept = d.firstChild; d.innerHTML = big; 'no'");
    assert_eq!(r, format!("limit:{:?}", LimitKind::Steps), "{r}");
    // The parse never made a node in the page's document, and the old children stayed.
    assert!(b.document().borrow().len() <= nodes + 1, "no node of the fragment");
}

// ---------------------------------------------------------------------------------------------- GC and cross-script callbacks

#[test]
fn held_detached_nodes_survive_collection_and_unheld_ones_are_pruned() {
    let mut b = browser();
    js(&mut b, "var d = document.createElement('div'); d.innerHTML = '<p id=h>a</p><p>b</p><p>c</p>'; window.keep = d.firstChild; window.hits = 0; keep.addEventListener('x', function () { window.hits++ }); var all = d.childNodes; d.innerHTML = '<s></s>'; all = null;");
    b.collect(&[]).unwrap();
    // A later script uses the held node, its listener, and its (detached) parent chain.
    assert_eq!(js(&mut b, "keep.dispatchEvent(new Event('x')); [hits, keep.id, keep.parentNode, keep.isConnected, keep.tagName, d.firstChild.tagName].join()"), "1,h,,false,P,S");
    let before = b.stats().wrappers;
    js(&mut b, "window.keep = null; d.innerHTML = '<u></u>'; d = null;");
    b.collect(&[]).unwrap();
    assert!(b.stats().wrappers < before, "{} then {}", before, b.stats().wrappers);
    assert_eq!(js(&mut b, "typeof keep"), "object");
}

#[test]
fn a_callback_from_another_script_can_mutate_the_target_while_it_converts() {
    let mut b = browser();
    js(&mut b, "window.mk = function (d) { return { toString: function () { d.innerHTML = '<a></a><a></a>'; d.firstChild.addEventListener('e', function () { window.ev = 1 }); return '<i>second</i>' } } }");
    assert_eq!(js(&mut b, "var d = document.createElement('div'); d.innerHTML = mk(d); [d.childNodes.length, d.firstChild.tagName, d.textContent].join()"), "1,I,second");
}

// ---------------------------------------------------------------------------------------------- inert scripts through the loader

fn server(routes: Vec<(&'static str, String)>) -> Server {
    serve(move |path| match routes.iter().find(|r| r.0 == path) {
        Some((_, body)) => Reply::new(200, Some("application/javascript"), body.as_bytes()),
        None => Reply::new(404, Some("text/plain"), "no"),
    })
}

fn run(html: &str, routes: Vec<(&'static str, String)>) -> (String, PageResult, Server) {
    let srv = server(routes);
    let mut b = Browser::new(tree::parse(html), BrowserLimits::default(), EvalLimits { max_steps: 5_000_000, ..Default::default() }, HeapLimits::default()).expect("browser");
    b.use_virtual_clock();
    b.configure_fetch(FetchConfig { base_url: Some(srv.url("/page.html")), client: Client::new(), limits: FetchLimits::default() }).unwrap();
    let r = b.run_page(PageConfig { url: srv.url("/page.html"), client: Client::new(), limits: PageLimits { quiet: Duration::from_millis(100), ..Default::default() }, fetch: None });
    let v = b.run_script("log.join('|')").unwrap();
    (b.it.display(v), r, srv)
}

#[test]
fn fragment_scripts_stay_inert_in_the_document_and_start_no_fetch() {
    big(|| {
        let html = "<!doctype html><head><script>var log = []; function L(x) { log.push(x) }</script></head><body><script>
            var d = document.createElement('div');
            d.innerHTML = '<script>L(\"frag-inline\")<\\/script><script src=\"/frag.js\"><\\/script><img src=\"/img.png\" onerror=\"L(1)\"><i onclick=\"L(2)\"></i>';
            document.body.appendChild(d);
            var s = d.firstChild; d.removeChild(s); document.head.appendChild(s); document.body.insertBefore(s, null);
            s.setAttribute('type', 'text/javascript'); s.text = 'L(\"edited\")';
            document.body.innerHTML = '<script>L(\"body-inline\")<\\/script><script src=\"/body.js\"><\\/script>';
            document.body.appendChild(d);
            var c = document.createElement('script'); c.textContent = 'L(\"control\")'; document.body.appendChild(c);
            L('end');
            </script></body>";
        let (log, r, srv) = run(html, vec![("/frag.js", "L('frag-ext')".into()), ("/body.js", "L('body-ext')".into()), ("/img.png", String::new())]);
        // Only the script made with createElement ran (and the two page scripts). The fragment scripts, also moved and edited, did not.
        assert_eq!(log, "end|control", "{r:?}");
        assert_eq!((srv.hits("/frag.js"), srv.hits("/body.js"), srv.hits("/img.png")), (0, 0, 0));
        assert_eq!((r.script_throws, r.load_errors, r.fetches), (0, 0, 0), "{r:?}");
    });
}
