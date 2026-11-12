//! Tests of `browser_range`: `Range` and `Selection`. The Chrome cases first, then the gates, the known gaps and the collector.

use crate::browser::*;
use crate::browser_fetch::{FetchConfig, FetchLimits};
use crate::browser_loader::*;
use crate::browser_topic_tests::topic_test;
use crate::resources::test_server::{Reply, serve};
use bare_html::tree;
use bare_js::eval::{Error, EvalLimits};
use bare_js::heap::HeapLimits;
use bare_net::http::Client;
use std::time::Duration;

topic_test!(chrome_cases, "range", 79);
topic_test!(live_cases, "liverange", 63);
topic_test!(ops_cases, "rangeops", 116);
topic_test!(rects_cases, "rects", 54);

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

const SEL_PAGE: &str = "<p id=p>hello world</p><p id=q>second</p>";
const SEL_JS: &str = "var s = getSelection(), t = document.getElementById('p').firstChild, u = document.getElementById('q').firstChild; ";

#[test]
fn set_base_and_extent_forwards_and_backwards() {
    let mut b = page(SEL_PAGE);
    js(&mut b, SEL_JS);
    assert_eq!(js(&mut b, "s.setBaseAndExtent(t, 1, t, 4); [s.anchorOffset, s.focusOffset, s.direction, s.toString(), s.rangeCount, s.type].join()"), "1,4,forward,ell,1,Range");
    assert_eq!(js(&mut b, "s.setBaseAndExtent(t, 4, t, 1); [s.anchorOffset, s.focusOffset, s.direction, s.toString(), s.getRangeAt(0).startOffset].join()"), "4,1,backward,ell,1");
    assert_eq!(js(&mut b, "s.setBaseAndExtent(u, 2, t, 3); [s.anchorNode === u, s.focusNode === t, s.direction].join()"), "true,true,backward");
    assert_eq!(js(&mut b, "s.setBaseAndExtent(t, 2, t, 2); [s.direction, s.type].join()"), "none,Caret");
    assert_eq!(js(&mut b, "s.setBaseAndExtent(t, 4, t, 1); s.collapse(t, 2); s.direction"), "none");
    assert_eq!(js(&mut b, "try { s.setBaseAndExtent(t, 99, t, 1) } catch (e) { e.name }"), "IndexSizeError");
    assert_eq!(js(&mut b, "try { s.setBaseAndExtent(null, 0, t, 1) } catch (e) { e.name }"), "TypeError");
    // Nodes outside the document: nothing happens.
    assert_eq!(js(&mut b, "s.setBaseAndExtent(t, 1, t, 3); var x = document.createElement('i'); s.setBaseAndExtent(x, 0, t, 1); s.toString()"), "el");
}

#[test]
fn extend_moves_the_focus() {
    let mut b = page(SEL_PAGE);
    js(&mut b, SEL_JS);
    assert_eq!(js(&mut b, "try { s.extend(t, 1) } catch (e) { e.name }"), "InvalidStateError");
    assert_eq!(js(&mut b, "s.collapse(t, 3); s.extend(t, 6); [s.anchorOffset, s.focusOffset, s.direction, s.toString()].join()"), "3,6,forward,lo ");
    assert_eq!(js(&mut b, "s.collapse(t, 6); s.extend(t, 2); [s.anchorOffset, s.focusOffset, s.direction, s.toString()].join()"), "6,2,backward,llo ");
    assert_eq!(js(&mut b, "s.extend(u, 3); [s.anchorNode === t, s.anchorOffset, s.focusNode === u, s.focusOffset, s.direction].join()"), "true,6,true,3,forward");
    assert_eq!(js(&mut b, "s.extend(t, 8); [s.anchorOffset, s.focusOffset, s.direction].join()"), "6,8,forward");
    assert_eq!(js(&mut b, "s.extend(t, 1); s.extend(t, 2); [s.anchorOffset, s.focusOffset, s.direction].join()"), "6,2,backward");
    assert_eq!(js(&mut b, "try { s.extend(t, 99) } catch (e) { e.name }"), "IndexSizeError");
    assert_eq!(js(&mut b, "s.extend(document.createElement('i'), 0); [s.anchorOffset, s.focusOffset].join()"), "6,2");
}

#[test]
fn contains_node_full_and_partial() {
    let mut b = page(SEL_PAGE);
    js(&mut b, SEL_JS);
    assert_eq!(js(&mut b, "document.getElementById('p').contains(t); s.containsNode(t)"), "false");
    assert_eq!(js(&mut b, "s.selectAllChildren(document.getElementById('p')); [s.containsNode(t), s.containsNode(document.getElementById('p')), s.containsNode(u), s.containsNode(u, true)].join()"), "true,true,false,false");
    assert_eq!(js(&mut b, "s.setBaseAndExtent(t, 2, u, 3); var p = document.getElementById('p'), q = document.getElementById('q'); [s.containsNode(t), s.containsNode(t, true), s.containsNode(u), s.containsNode(u, true), s.containsNode(p, true), s.containsNode(q, true), s.containsNode(document.body)].join()"), "false,true,false,true,true,true,false");
    assert_eq!(js(&mut b, "s.setBaseAndExtent(p, 0, q, 1); [s.containsNode(p), s.containsNode(t), s.containsNode(u), s.containsNode(q)].join()"), "true,true,true,true");
    assert_eq!(js(&mut b, "s.removeAllRanges(); s.containsNode(t, true)"), "false");
    assert_eq!(js(&mut b, "try { s.containsNode(null) } catch (e) { e.name }"), "TypeError");
}

#[test]
fn a_keydown_handler_can_set_the_selection_and_typing_goes_on() {
    let mut b = page("<div id=e contenteditable></div>");
    js(&mut b, "var e = document.getElementById('e'); window.n = 0; e.addEventListener('keydown', function () { var s = getSelection(); s.setBaseAndExtent(e, 0, e, 0); s.extend(e, 0); s.containsNode(e, true); n++ }); getSelection().collapse(e, 0);");
    let e = b.query("#e").unwrap().unwrap();
    b.focus(e).unwrap();
    for (i, k) in ["a", "b", "c"].iter().enumerate() {
        b.drive_key(if i == 0 { Some(e) } else { None }, k).unwrap();
    }
    assert_eq!(js(&mut b, "n"), "3");
    // The handler puts the caret back at offset 0, so each key goes before the last.
    assert_eq!(js(&mut b, "e.textContent"), "cba");
}

#[test]
fn the_globals_are_lazy_and_work_when_read() {
    let mut b = page("<p id=p>abc</p>");
    assert_eq!(js(&mut b, "[Object.getOwnPropertyDescriptor(window, 'Range'), Object.getOwnPropertyDescriptor(window, 'Selection'), Object.getOwnPropertyDescriptor(window, 'AbstractRange')].map(function (d) { return typeof d.get }).join()"), "function,function,function");
    assert_eq!(js(&mut b, "[typeof Range, typeof Selection, typeof AbstractRange, Object.getPrototypeOf(Range.prototype) === AbstractRange.prototype, Object.getPrototypeOf(Range) === AbstractRange].join()"), "function,function,function,true,true");
    assert_eq!(js(&mut b, "window.Range = 3; Range"), "3");
}

#[test]
fn the_selection_methods_that_are_left_are_gates() {
    let mut b = page("<p id=p>abc</p>");
    js(&mut b, "var r = document.createRange(); r.selectNodeContents(document.getElementById('p'))");
    for (m, what) in [("modify('move', 'forward', 'character')", "Selection.modify"), ("deleteFromDocument()", "Selection.deleteFromDocument")] {
        assert_eq!(js(&mut b, &format!("getSelection().{m}")), format!("unsupported:{what}"));
    }
    // A gate on the wrong receiver is still an illegal invocation, as for the other methods.
    assert_eq!(js(&mut b, "try { Range.prototype.deleteContents.call({}) } catch (e) { e.name + ':' + e.message }"), "TypeError:Illegal invocation");
}

#[test]
fn a_range_follows_the_dom_and_never_panics() {
    let mut b = page("<p id=p>abcdef</p><i id=i>xyz</i>");
    js(&mut b, "var t = document.getElementById('p').firstChild, r = document.createRange(); r.setStart(t, 1); r.setEnd(t, 5);");
    js(&mut b, "t.data = 'ab'");
    assert_eq!(js(&mut b, "r.startOffset + ':' + r.endOffset + ':[' + r + ']'"), "0:0:[]");
    js(&mut b, "document.getElementById('p').remove()");
    assert_eq!(js(&mut b, "(r.startContainer === document.body) + ':' + r.endOffset + ':[' + r + ']'"), "true:0:[]");
    js(&mut b, "var q = document.createRange(); q.selectNodeContents(document.body); document.body.textContent = ''");
    assert_eq!(js(&mut b, "q.endOffset + ':[' + q + ']'"), "0:[]");
}

#[test]
fn the_watcher_is_set_for_a_live_range_only() {
    let mut b = page("<p id=p>abc</p>");
    assert!(!b.dom.doc.borrow().has_watcher());
    js(&mut b, "var r = document.createRange();");
    assert!(b.dom.doc.borrow().has_watcher());
    // A dead range and its table entry go at a collect, and the watcher goes with them.
    js(&mut b, "r = null;");
    b.collect(&[]).unwrap();
    assert!(!b.dom.doc.borrow().has_watcher());
    js(&mut b, "document.createRange();");
    assert!(b.dom.doc.borrow().has_watcher());
}

#[test]
fn a_range_and_a_mutation_observer_share_the_watcher() {
    let mut b = page("<div id=r><p id=a>A</p><p id=b>B</p></div>");
    js(&mut b, "var log = []; var mo = new MutationObserver(function (rs) { log.push(rs.length) }); mo.observe(document.body, { childList: true, subtree: true });");
    js(&mut b, "var rg = document.createRange(); rg.setStart(document.getElementById('r'), 1); rg.setEnd(document.getElementById('r'), 2);");
    js(&mut b, "document.getElementById('a').remove();");
    assert_eq!(js(&mut b, "rg.startOffset + '-' + rg.endOffset"), "0-1");
    js(&mut b, "log.push(mo.takeRecords().length); mo.disconnect(); document.getElementById('b').remove();");
    assert_eq!(js(&mut b, "rg.startOffset + '-' + rg.endOffset"), "0-0");
    assert_eq!(js(&mut b, "log.join()"), "1");
    // The observer left: the range still has the watcher.
    assert!(b.dom.doc.borrow().has_watcher());
}

#[test]
fn a_page_with_a_range_changes_a_large_tree_and_keeps_the_points() {
    let mut b = page("<ul id=u></ul>");
    js(&mut b, "var u = document.getElementById('u'); for (var i = 0; i < 2000; i++) u.appendChild(document.createElement('li'));");
    js(&mut b, "var r = document.createRange(); r.setStart(u, 1000); r.setEnd(u, 1500);");
    js(&mut b, "for (var i = 0; i < 500; i++) u.removeChild(u.firstChild); for (var i = 0; i < 100; i++) u.insertBefore(document.createElement('b'), u.childNodes[600]);");
    assert_eq!(js(&mut b, "r.startOffset + '-' + r.endOffset"), "500-1100");
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
    assert!(!b.dom.doc.borrow().has_watcher());
}

#[test]
fn a_utf16_cut_inside_a_pair_gives_the_replacement_character() {
    // Known deviation: Chrome keeps the lone surrogate, the host text is UTF-8.
    let mut b = page("<p id=p>a\u{1F600}b</p>");
    assert_eq!(js(&mut b, "var t = document.getElementById('p').firstChild, r = document.createRange(); r.setStart(t, 2); r.setEnd(t, 4); r.toString().length + ':' + r.toString().charCodeAt(0)"), "2:65533");
}

/// Run `html` as a page (its scripts run) and give `log.join()` and the result of the run.
fn run_page(html: &str) -> (String, PageResult) {
    let srv = serve(|_| Reply::new(404, Some("text/plain"), "no"));
    let mut b = Browser::new(tree::parse(html), BrowserLimits::default(), EvalLimits { max_steps: 5_000_000, ..Default::default() }, HeapLimits::default()).expect("browser");
    b.use_virtual_clock();
    b.configure_fetch(FetchConfig { base_url: Some(srv.url("/page.html")), client: Client::new(), limits: FetchLimits::default() }).unwrap();
    let r = b.run_page(PageConfig { url: srv.url("/page.html"), client: Client::new(), limits: PageLimits { quiet: Duration::from_millis(100), ..Default::default() }, fetch: None });
    let v = b.run_script("log.join()").unwrap();
    (b.it.display(v), r)
}

#[test]
fn scripts_of_a_contextual_fragment_run_when_they_go_in_a_page_as_in_chrome() {
    // The counts are the answers of Chrome 155: each script runs once, when it goes into the page, however it goes in; a clone keeps that; `innerHTML`
    // makes inert scripts. (Chrome runs a script at once. This host runs it when the script that put it in is done, as for every inline script.)
    let html = r#"<!doctype html><head><script>var log = []; var c = [0, 0, 0, 0, 0, 0, 0];</script></head><body><div id=r><p id=p1>Hello</p></div><script>
        var p = document.getElementById('p1'), r = document.getElementById('r');
        var frag = function (m) { var g = document.createRange(); g.setStart(p, 0); g.setEnd(p, 1); return g.createContextualFragment(m) };
        var js = function (i) { return '<script>c[' + i + ']++<\/script>' };
        var f = frag(js(0) + '<i>a</i>'); log.push('before' + c[0]); r.appendChild(f); log.push('after' + c[0]);
        var g = frag(js(1)), s = g.firstChild; r.appendChild(g); s.remove(); r.appendChild(s);
        r.appendChild(frag('<div>' + js(2) + '</div>'));
        r.appendChild(frag(js(3)).firstChild.cloneNode(true));
        var q = document.createRange(); q.setStart(p, 0); q.insertNode(frag(js(4)));
        var d = document.createElement('div'); d.innerHTML = js(5); r.appendChild(d);
        var e = document.createElement('script'); e.textContent = 'c[6]++'; var z = document.createRange(); z.setStart(p.lastChild, 2); z.insertNode(e);
        </script><script>log.push(c.join(''))</script></body>"#;
    let (log, res) = run_page(html);
    assert_eq!(log, "before0,after0,1111101", "{res:?}");
}

#[test]
fn a_contextual_fragment_in_a_table_or_foreign_context_parses_and_a_select_context_has_the_select_mode() {
    // Table contexts use the table insertion modes, a foreign context the foreign rules, and a select context the "in select" mode (`bare_html::tree::fragment`).
    let mut b = page("<table id=t><tbody id=tb><tr id=tr><td id=td>a</td></tr></tbody></table><select id=s></select><svg id=v></svg><p id=ok>x</p>");
    let html = |b: &mut Browser, id: &str| {
        let src = format!("var r = document.createRange(); r.selectNodeContents(document.getElementById('{id}')); var f = r.createContextualFragment('<td>x</td>'); var d = document.createElement('div'); d.appendChild(f); d.innerHTML");
        js(b, &src)
    };
    assert_eq!(html(&mut b, "t"), "<tbody><tr><td>x</td></tr></tbody>");
    assert_eq!(html(&mut b, "tb"), "<tr><td>x</td></tr>");
    assert_eq!(html(&mut b, "tr"), "<td>x</td>");
    assert_eq!(html(&mut b, "td"), "x");
    let src = "var r = document.createRange(); r.selectNodeContents(document.getElementById('s')); r.createContextualFragment('<td>x</td>')";
    assert_eq!(js(&mut b, &format!("{src}.firstChild.data")), "x");
    // In an SVG context, `<path>` is an SVG element and `<td>` is ignored (the "in body" rules).
    assert_eq!(js(&mut b, "var r = document.createRange(); r.selectNodeContents(document.getElementById('v')); var f = r.createContextualFragment('<path/><td>x</td>'); f.firstChild.namespaceURI + ' ' + f.childNodes.length"), "http://www.w3.org/2000/svg 2");
    // A text in a table cell has the cell as context; a document or an `html` start has the body.
    assert_eq!(js(&mut b, "var r = document.createRange(); r.setStart(document.getElementById('td').firstChild, 0); r.createContextualFragment('<td>x</td><i>y</i>').childNodes.length"), "2");
    assert_eq!(js(&mut b, "var r = document.createRange(); r.setStart(document.documentElement, 0); r.createContextualFragment('<td>x</td><i>y</i>').childNodes.length"), "2");
}

#[test]
fn select_inner_html_uses_the_select_mode() {
    let mut b = page("<select id=s></select>");
    js(&mut b, "var s = document.getElementById('s'); s.innerHTML = '<option>1</option><optgroup><option>2</option></optgroup>';");
    assert_eq!(js(&mut b, "s.options.length"), "2");
    assert_eq!(js(&mut b, "s.innerHTML"), "<option>1</option><optgroup><option>2</option></optgroup>");
    // The standard drops `div` and `td` in a select and keeps their text.
    js(&mut b, "s.innerHTML = '<div>a<option>b</option></div><td>c</td>';");
    assert_eq!(js(&mut b, "s.innerHTML"), "a<option>b</option>c");
}

#[test]
fn the_range_methods_never_leave_a_stale_state_after_an_error() {
    // An error from insertNode or surroundContents does not undo the steps before it (the standard has no rollback): the range and the tree show them.
    let mut b = page("<div id=r><p id=p1>Hello <b id=b1>big</b> world</p></div>");
    js(&mut b, "var p = document.getElementById('p1'), r = document.createRange(); r.setStart(p.firstChild, 1); r.setEnd(p.firstChild, 4);");
    assert_eq!(js(&mut b, "try { r.surroundContents(document.createTextNode('x')) } catch (e) { e.name }"), "HierarchyRequestError");
    assert_eq!(js(&mut b, "p.outerHTML"), "<p id=\"p1\">Hxo <b id=\"b1\">big</b> world</p>");
}

#[test]
fn a_range_cut_in_a_very_deep_tree_is_a_limit_not_a_stack_overflow() {
    let n = 6000;
    let page = format!("<!doctype html><body><div id=top>{}x{}</div>", "<div>".repeat(n), "</div>".repeat(n));
    let r = std::thread::Builder::new()
        .stack_size(8 << 20)
        .spawn(move || {
            let limits = BrowserLimits { max_tree_depth: 100_000, max_nodes: 1 << 20, ..Default::default() };
            let mut b = Browser::new(tree::parse(&page), limits, EvalLimits { max_steps: 200_000_000, ..Default::default() }, HeapLimits::default()).expect("browser");
            // The start is the text in the deepest div; the end is after the top div: each level on the start path is a partly selected node.
            js(&mut b, "var t = document.getElementById('top'), x = t; while (x.firstChild && x.firstChild.nodeType === 1) x = x.firstChild; var r = document.createRange(); r.setStart(x.firstChild, 0); r.setEnd(document.body, 1);");
            js(&mut b, "try { r.deleteContents(); 'done' } catch (e) { 'threw' }")
        })
        .unwrap()
        .join()
        .unwrap();
    assert!(r == "done" || r.contains("Limit") || r.contains("limit"), "{r}");
}

fn laid(body: &str) -> Browser {
    let mut b = page(body);
    b.set_viewport(400.0, 300.0).expect("viewport");
    b
}

#[test]
fn a_page_that_reads_no_rectangles_makes_no_list_state_and_one_read_makes_it() {
    let mut b = laid("<p id=p>abc <b id=b>def</b></p>");
    assert_eq!(js(&mut b, "document.getElementById('p').getBoundingClientRect().width > 0"), "true");
    assert!(!b.dom.range.is_made());
    assert!(!b.dom.geometry.uncull.get());
    assert_eq!(js(&mut b, "document.getElementById('b').getClientRects().length"), "1");
    assert_eq!(js(&mut b, "document.getElementById('p').getClientRects().item(0) === document.getElementById('p').getClientRects()[0]"), "false");
    // One `item` function for the realm: the lists share it.
    assert_eq!(js(&mut b, "var a = document.getElementById('p').getClientRects(), c = document.getElementById('b').getClientRects(); a.item === c.item"), "true");
}

#[test]
fn rectangles_of_detached_nodes_and_ranges_are_empty() {
    let mut b = laid("<p id=p>abc</p>");
    assert_eq!(js(&mut b, "var e = document.createElement('div'); e.textContent = 'x'; var r = document.createRange(); r.selectNodeContents(e); [e.getClientRects().length, r.getClientRects().length, JSON.stringify(r.getBoundingClientRect())].join()"), r#"0,0,{"x":0,"y":0,"width":0,"height":0,"top":0,"right":0,"bottom":0,"left":0}"#);
}

#[test]
fn rectangles_of_svg_and_columns_are_gates() {
    let mut b = laid("<svg id=v><rect id=r width=10 height=10></rect></svg><table><colgroup id=cg><col id=co></colgroup><tr><td>a</td></tr></table><p id=ok>x</p>");
    assert_eq!(js(&mut b, "document.getElementById('v').getClientRects()"), "unsupported:getClientRects of an SVG or MathML element (SVG has no layout)");
    assert_eq!(js(&mut b, "document.getElementById('r').getClientRects()"), "unsupported:getClientRects of an SVG or MathML element (SVG has no layout)");
    assert_eq!(js(&mut b, "document.getElementById('co').getClientRects()"), "unsupported:getClientRects of a col or colgroup (layout makes no box for a column)");
    assert_eq!(js(&mut b, "document.getElementById('cg').getClientRects()"), "unsupported:getClientRects of a col or colgroup (layout makes no box for a column)");
    assert_eq!(js(&mut b, "var r = document.createRange(); r.selectNode(document.getElementById('v')); r.getClientRects()"), "unsupported:getClientRects of an SVG or MathML element (SVG has no layout)");
    // The gate leaves nothing behind: the next read is a normal one, and the layout flag is off.
    assert!(!b.dom.geometry.uncull.get());
    assert_eq!(js(&mut b, "document.getElementById('ok').getClientRects().length"), "1");
}

#[test]
fn a_br_and_an_empty_span_in_a_range_use_the_second_layout_and_leave_the_flag_off() {
    let mut b = laid("<div id=d>aa<br id=br>bb<span id=e></span>cc</div>");
    assert_eq!(js(&mut b, "var d = document.getElementById('d'), r = document.createRange(); r.selectNodeContents(d); r.getClientRects().length"), "5");
    assert!(!b.dom.geometry.uncull.get());
    // The second layout is only for the read: the page layout has no box for the empty span.
    assert_eq!(js(&mut b, "document.getElementById('d').getBoundingClientRect().height"), "44");
    assert_eq!(js(&mut b, "document.getElementById('e').getClientRects().length"), "1");
}
