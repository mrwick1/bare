//! Tests of `browser_range`: `Range` and `Selection`. The Chrome cases first, then the gates, the known gaps and the collector.

use crate::browser::*;
use crate::browser_topic_tests::topic_test;
use bare_html::tree;
use bare_js::eval::{Error, EvalLimits};
use bare_js::heap::HeapLimits;

topic_test!(chrome_cases, "range", 79);

fn page(body: &str) -> Browser {
    Browser::new(tree::parse(&format!("<!doctype html><body>{body}</body>")), BrowserLimits::default(), EvalLimits { max_steps: 50_000_000, ..Default::default() }, HeapLimits::default()).expect("browser")
}

fn js(b: &mut Browser, src: &str) -> String {
    match b.run_script(src) {
        Ok(v) => b.it.display(v),
        Err(Error::Unsupported(u)) => format!("unsupported:{}", u.what),
        Err(e) => format!("{e:?}"),
    }
}

#[test]
fn the_globals_are_lazy_and_work_when_read() {
    let mut b = page("<p id=p>abc</p>");
    assert_eq!(js(&mut b, "[Object.getOwnPropertyDescriptor(window, 'Range'), Object.getOwnPropertyDescriptor(window, 'Selection'), Object.getOwnPropertyDescriptor(window, 'AbstractRange')].map(function (d) { return typeof d.get }).join()"), "function,function,function");
    assert_eq!(js(&mut b, "[typeof Range, typeof Selection, typeof AbstractRange, Object.getPrototypeOf(Range.prototype) === AbstractRange.prototype, Object.getPrototypeOf(Range) === AbstractRange].join()"), "function,function,function,true,true");
    assert_eq!(js(&mut b, "window.Range = 3; Range"), "3");
}

#[test]
fn the_editing_methods_and_the_boxes_are_gates() {
    let mut b = page("<p id=p>abc</p>");
    js(&mut b, "var r = document.createRange(); r.selectNodeContents(document.getElementById('p'))");
    for (m, what) in [
        ("deleteContents()", "Range.deleteContents"),
        ("extractContents()", "Range.extractContents"),
        ("cloneContents()", "Range.cloneContents"),
        ("insertNode(document.createElement('i'))", "Range.insertNode"),
        ("surroundContents(document.createElement('i'))", "Range.surroundContents"),
        ("createContextualFragment('<i>')", "Range.createContextualFragment"),
        ("getBoundingClientRect()", "Range.getBoundingClientRect"),
        ("getClientRects()", "Range.getClientRects"),
        ("compareBoundaryPoints(0, r)", "Range.compareBoundaryPoints"),
        ("comparePoint(document.body, 0)", "Range.comparePoint"),
        ("isPointInRange(document.body, 0)", "Range.isPointInRange"),
        ("intersectsNode(document.body)", "Range.intersectsNode"),
    ] {
        assert_eq!(js(&mut b, &format!("r.{m}")), format!("unsupported:{what}"));
    }
    for (m, what) in [("extend(document.body, 0)", "Selection.extend"), ("setBaseAndExtent(document.body, 0, document.body, 1)", "Selection.setBaseAndExtent"), ("containsNode(document.body)", "Selection.containsNode"), ("modify('move', 'forward', 'character')", "Selection.modify"), ("deleteFromDocument()", "Selection.deleteFromDocument")] {
        assert_eq!(js(&mut b, &format!("getSelection().{m}")), format!("unsupported:{what}"));
    }
    // A gate on the wrong receiver is still an illegal invocation, as for the other methods.
    assert_eq!(js(&mut b, "try { Range.prototype.deleteContents.call({}) } catch (e) { e.name + ':' + e.message }"), "TypeError:Illegal invocation");
}

#[test]
fn a_range_does_not_follow_the_dom_and_never_panics() {
    // Known gap: Chrome moves the boundary points when the DOM changes. Here a point keeps its node and offset.
    let mut b = page("<p id=p>abcdef</p><i id=i>xyz</i>");
    js(&mut b, "var t = document.getElementById('p').firstChild, r = document.createRange(); r.setStart(t, 1); r.setEnd(t, 5);");
    js(&mut b, "t.data = 'ab'");
    assert_eq!(js(&mut b, "r.startOffset + ':' + r.endOffset + ':[' + r + ']'"), "1:5:[b]");
    js(&mut b, "document.getElementById('p').remove()");
    assert_eq!(js(&mut b, "(r.startContainer === t) + ':' + r.endOffset + ':[' + r + ']'"), "true:5:[b]");
    js(&mut b, "var q = document.createRange(); q.selectNodeContents(document.body); document.body.textContent = ''");
    assert_eq!(js(&mut b, "q.endOffset + ':[' + q + ']'"), "1:[]");
}

#[test]
fn a_selection_to_string_is_the_text_of_the_text_nodes() {
    // Known deviation: Chrome gives rendered text, with line breaks between blocks.
    let mut b = page("<p id=a>one</p><p id=b>two</p>");
    assert_eq!(js(&mut b, "var s = getSelection(); s.selectAllChildren(document.body); s.toString()"), "onetwo");
}

#[test]
fn ranges_and_the_selection_range_survive_a_collect() {
    let mut b = page("<p id=p>abcdef</p>");
    js(&mut b, "var s = getSelection(); (function () { var r = document.createRange(); r.setStart(document.getElementById('p').firstChild, 1); r.setEnd(document.getElementById('p').firstChild, 4); s.addRange(r) })();");
    js(&mut b, "for (var i = 0; i < 1000; i++) document.createRange()");
    b.collect(&[]).unwrap();
    assert_eq!(js(&mut b, "s.rangeCount + ' ' + s.getRangeAt(0).toString() + ' ' + s.anchorOffset + s.focusOffset"), "1 bcd 14");
    // The ranges of 1000 dead objects are gone (the table is pruned): the bound is not met by them.
    assert_eq!(js(&mut b, "for (var i = 0; i < 60000; i++) document.createRange(); 'ok'"), "ok");
}

#[test]
fn the_range_bound_is_a_limit() {
    let mut b = page("");
    let r = b.run_script("var keep = []; for (var i = 0; i < 70000; i++) keep.push(document.createRange())");
    assert!(matches!(r, Err(Error::Limit(_))), "{r:?}");
}

#[test]
fn a_page_that_never_reads_the_globals_makes_no_state() {
    let mut b = page("<p id=p>abc</p>");
    assert_eq!(js(&mut b, "document.getElementById('p').textContent"), "abc");
    assert!(!b.dom.range.is_made());
}

#[test]
fn a_utf16_cut_inside_a_pair_gives_the_replacement_character() {
    // Known deviation: Chrome keeps the lone surrogate, the host text is UTF-8.
    let mut b = page("<p id=p>a\u{1F600}b</p>");
    assert_eq!(js(&mut b, "var t = document.getElementById('p').firstChild, r = document.createRange(); r.setStart(t, 2); r.setEnd(t, 4); r.toString().length + ':' + r.toString().charCodeAt(0)"), "2:65533");
}
