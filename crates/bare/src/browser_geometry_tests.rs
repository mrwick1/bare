//! Tests of `browser_geometry`: `HTMLElement.offsetWidth` and `offsetHeight` over the native cascade and layout.
//! The expected sizes are written by hand from the CSS box model (CSS 2.1 8.1, 10.2, 10.5; CSS Sizing 3 `box-sizing`) and from the
//! monospace font of the engine (`layout/src/font.rs`: a letter is 0.6 em wide, a line of a 10px font is 11 + 3 = 14px high).
//! Native layout is never its own oracle, and Node has no layout, so it is not an oracle either. No browser fidelity is claimed.

use crate::browser::*;
use crate::browser_computed::SheetPart;
use crate::browser_geometry::GeometryLimits;
use bare_html::tree;
use bare_js::eval::{Error, EvalLimits, LimitKind};
use bare_js::heap::HeapLimits;
use bare_js::value::Value;

fn at(page: &str) -> Browser {
    Browser::new(tree::parse(page), BrowserLimits::default(), EvalLimits { max_steps: 50_000_000, ..Default::default() }, HeapLimits::default()).expect("browser")
}

fn show(b: &mut Browser, r: Result<Value, Error>) -> String {
    match r {
        Ok(v) => b.it.display(v),
        Err(Error::Throw(v)) if matches!(v, Value::Object(_)) => format!("throw:{}:{}", b.it.get_str(v, "name"), b.it.get_str(v, "message")),
        Err(Error::Throw(v)) => format!("throw-value:{}", b.it.display(v)),
        Err(Error::Limit(l)) => format!("limit:{l:?}"),
        Err(Error::Unsupported(u)) => format!("unsupported:{}", u.what),
        Err(e) => format!("other:{e:?}"),
    }
}

fn js(b: &mut Browser, src: &str) -> String {
    let r = b.run_script(src);
    show(b, r)
}

fn lim(name: &'static str) -> String {
    format!("limit:{:?}", LimitKind::Host(name))
}

fn big<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(64 << 20).spawn(f).unwrap().join().unwrap()
}

/// `offsetWidth x offsetHeight` of the element with this id.
fn size(b: &mut Browser, id: &str) -> String {
    js(b, &format!("var e = document.getElementById('{id}'); e.offsetWidth + 'x' + e.offsetHeight"))
}

fn page(css: &str, body: &str) -> Browser {
    at(&format!("<!doctype html><html><head><style>{css}</style></head><body>{body}</body></html>"))
}

#[test]
fn the_accessors_exist_and_are_readonly() {
    let mut b = page("", "<div id=t></div>");
    assert_eq!(js(&mut b, "var d = Object.getOwnPropertyDescriptor(HTMLElement.prototype, 'offsetWidth'); [typeof d.get, d.set, d.enumerable, d.configurable].join()"), "function,,true,true");
    assert_eq!(js(&mut b, "var d = Object.getOwnPropertyDescriptor(HTMLElement.prototype, 'offsetHeight'); [typeof d.get, d.set, d.enumerable, d.configurable].join()"), "function,,true,true");
    assert_eq!(js(&mut b, "Object.getOwnPropertyDescriptor(Element.prototype, 'offsetWidth') === undefined"), "true");
    // a write is ignored in sloppy code, and a TypeError in strict code
    assert_eq!(js(&mut b, "var e = document.getElementById('t'); var before = e.offsetWidth; e.offsetWidth = 5; before === e.offsetWidth && !e.hasOwnProperty('offsetWidth')"), "true");
    assert!(js(&mut b, "(function () { 'use strict'; document.getElementById('t').offsetHeight = 5 })()").starts_with("throw:TypeError"));
}

#[test]
fn the_getters_check_the_html_element_brand() {
    let mut b = page("", "<div id=t></div>");
    let get = "var g = Object.getOwnPropertyDescriptor(HTMLElement.prototype, 'offsetWidth').get; function t(v) { try { g.call(v); return 'ok' } catch (e) { return e.name } }";
    js(&mut b, get);
    assert_eq!(js(&mut b, "[t(undefined), t(null), t({}), t(document), t(document.createTextNode('x')), t(document.createElementNS('http://www.w3.org/2000/svg', 'svg')), t(document.getElementById('t'))].join()"), "TypeError,TypeError,TypeError,TypeError,TypeError,TypeError,ok");
    assert_eq!(js(&mut b, "try { g.call({}) } catch (e) { e.message }"), "Illegal invocation");
}

#[test]
fn content_box_formula_from_authored_css() {
    let css = "#a { width: 100px; height: 50px; padding: 10px; border: 5px solid } \
               #b { width: 100px; height: 40px; padding: 1px 2px 3px 4px; border-style: solid; border-width: 5px 6px 7px 8px } \
               #c { width: 100px; height: 50px; margin: 9px 11px 13px 17px }";
    let mut b = page(css, "<div id=a></div><div id=b></div><div id=c></div>");
    // 100 + 2 * 10 + 2 * 5, 50 + 2 * 10 + 2 * 5
    assert_eq!(size(&mut b, "a"), "130x80");
    // width 100 + (2 + 4) + (6 + 8) = 120, height 40 + (1 + 3) + (5 + 7) = 56
    assert_eq!(size(&mut b, "b"), "120x56");
    // margins are outside the border box
    assert_eq!(size(&mut b, "c"), "100x50");
}

#[test]
fn border_box_formula_from_authored_css() {
    let css = "#a { box-sizing: border-box; width: 100px; height: 50px; padding: 10px; border: 5px solid } \
               #b { box-sizing: border-box; width: 20px; height: 10px; padding: 10px; border: 5px solid }";
    let mut b = page(css, "<div id=a></div><div id=b></div>");
    assert_eq!(size(&mut b, "a"), "100x50");
    // the content box cannot be negative: the border box is then padding + border (10 + 10 + 5 + 5)
    assert_eq!(size(&mut b, "b"), "30x30");
}

#[test]
fn auto_width_fills_the_body_and_follows_the_viewport() {
    let mut b = page("#a { height: 10px } #v { width: 50vw; height: 25vh }", "<div id=a></div><div id=v></div>");
    // 1280 - 2 * 8 (the body margin of the browser sheet)
    assert_eq!(size(&mut b, "a"), "1264x10");
    assert_eq!(size(&mut b, "v"), "640x200");
    // held element, new viewport: no stale value
    js(&mut b, "var a = document.getElementById('a'), v = document.getElementById('v');");
    assert_eq!(b.set_viewport(800.0, 600.0).unwrap(), 0);
    assert_eq!(js(&mut b, "a.offsetWidth + 'x' + a.offsetHeight + ',' + v.offsetWidth + 'x' + v.offsetHeight"), "784x10,400x150");
}

#[test]
fn held_elements_follow_class_id_style_and_style_text_changes() {
    let css = "#t { width: 10px; height: 10px } .w { width: 20px } #u { width: 30px } ";
    let mut b = page(css, "<div id=t></div><style id=s></style>");
    js(&mut b, "var t = document.getElementById('t');");
    assert_eq!(js(&mut b, "t.offsetWidth + 'x' + t.offsetHeight"), "10x10");
    // a class rule of the same specificity as the id rule loses; `.w` is less specific than `#t`
    assert_eq!(js(&mut b, "t.className = 'w'; t.offsetWidth"), "10");
    assert_eq!(js(&mut b, "t.id = 'u'; t.offsetWidth + 'x' + t.offsetHeight"), "30x0");
    assert_eq!(js(&mut b, "t.id = 't'; t.style.width = '44px'; t.style.height = '5px'; t.offsetWidth + 'x' + t.offsetHeight"), "44x5");
    assert_eq!(js(&mut b, "t.removeAttribute('style'); document.getElementById('s').textContent = '#t { width: 7px; height: 9px !important }'; t.offsetWidth + 'x' + t.offsetHeight"), "7x9");
    // the text of the `<style>` changes and the document revision does not: the read is still current
    assert_eq!(js(&mut b, "document.getElementById('s').textContent = '#t { width: 8px }'; t.offsetWidth + 'x' + t.offsetHeight"), "8x10");
}

fn part(t: &str) -> SheetPart {
    SheetPart { text: t.into(), layer: None }
}

#[test]
fn external_sheets_embedded_sheets_and_inline_styles_cascade_in_order() {
    let mut b = at("<!doctype html><html><head><link rel=stylesheet href=a.css><style>#t { height: 30px } #x { width: 1px }</style></head><body><div id=t></div><div id=x style='width: 12px'></div></body></html>");
    // a linked sheet that is not supplied is a gate, never "no rules"
    let gate = "unsupported:computed style needs an external stylesheet that the embedder has not supplied (Browser::style_availability lists them)";
    assert_eq!(size(&mut b, "t"), gate);
    b.set_external_stylesheet("a.css", vec![part("#t { width: 77px; height: 11px }")]).unwrap();
    // the embedded sheet comes later in the document, so its height wins
    assert_eq!(size(&mut b, "t"), "77x30");
    // the `style` attribute beats the sheets
    assert_eq!(js(&mut b, "document.getElementById('x').offsetWidth"), "12");
    b.set_external_stylesheet("a.css", vec![part("#t { width: 88px }")]).unwrap();
    assert_eq!(size(&mut b, "t"), "88x30");
    assert!(b.remove_external_stylesheet("a.css"));
    assert_eq!(size(&mut b, "t"), gate);
    assert_eq!(js(&mut b, "typeof document.getElementById('x').offsetWidth"), gate);
}

#[test]
fn text_decides_the_width_of_an_inline_block() {
    let css = "#a, #b, #c { display: inline-block; font-size: 10px; margin: 0 } #c { padding: 3px } .row { white-space: nowrap }";
    let mut b = page(css, "<div class=row><span id=a>abcde</span><span id=b>abcdefghij</span><span id=c>abcde</span></div>");
    js(&mut b, "document.getElementById('a').style.display = 'inline-block';");
    // 5 letters of 6px, one line of 11 + 3
    assert_eq!(size(&mut b, "a"), "30x14");
    assert_eq!(size(&mut b, "b"), "60x14");
    // padding 3px on each side
    assert_eq!(size(&mut b, "c"), "36x20");
    // a text change is seen by the held element
    assert_eq!(js(&mut b, "var a = document.getElementById('a'); a.firstChild.data = 'abc'; a.offsetWidth"), "18");
}

#[test]
fn an_element_with_no_box_is_zero() {
    let css = "#n { display: none; width: 50px; height: 50px } #k { width: 50px; height: 50px }";
    let mut b = page(css, "<div id=n><p id=in><span id=deep>x</span></p></div><div id=k></div>");
    assert_eq!(size(&mut b, "n"), "0x0");
    // hidden below display:none, where the engine has no style at all
    assert_eq!(size(&mut b, "in"), "0x0");
    assert_eq!(size(&mut b, "deep"), "0x0");
    assert_eq!(size(&mut b, "k"), "50x50");
    // the browser sheet hides `head` and `style`
    assert_eq!(js(&mut b, "document.head.offsetWidth + ',' + document.querySelector('style').offsetHeight"), "0,0");
    // detached, in a fragment, and removed
    assert_eq!(js(&mut b, "var d = document.createElement('div'); d.style.width = '9px'; d.offsetWidth + ',' + d.offsetHeight"), "0,0");
    assert_eq!(js(&mut b, "var k = document.getElementById('k'); k.remove(); k.offsetWidth + ',' + k.offsetHeight"), "0,0");
    assert_eq!(js(&mut b, "document.body.appendChild(k); k.offsetWidth + ',' + k.offsetHeight"), "50,50");
}

#[test]
fn a_shadow_tree_element_has_a_size_from_its_own_scope() {
    // The page rule `i { width: 5px }` must not reach in; the shadow rule `i { width: 30px }` does. An unattached tree has no box.
    let mut b = page("i { width: 5px; height: 5px; display: block } #k { width: 50px }", "<div id=h></div><div id=k></div>");
    let src = "var r = document.getElementById('h').attachShadow({ mode: 'open' }); r.innerHTML = '<style>i { width: 30px; height: 20px; display: block }</style><i id=in></i>'; r.getElementById('in').offsetWidth + 'x' + r.getElementById('in').offsetHeight";
    assert_eq!(js(&mut b, src), "30x20");
    assert_eq!(size(&mut b, "k"), "50x0");
    assert_eq!(js(&mut b, "var h2 = document.createElement('div'); var r2 = h2.attachShadow({ mode: 'open' }); r2.appendChild(document.createElement('b')).offsetWidth"), "0");
}

#[test]
fn a_slotted_node_is_laid_out_in_the_slot_and_the_fallback_shows_when_nothing_is_slotted() {
    let mut b = page("#a { width: 40px; height: 10px }", "<div id=h><div id=a></div></div><div id=g></div>");
    let src = "var r = document.getElementById('h').attachShadow({ mode: 'open' }); r.innerHTML = '<style>#w { padding: 3px }</style><div id=w><slot></slot></div>'; document.getElementById('a').offsetWidth + 'x' + document.getElementById('a').offsetHeight + ' ' + document.getElementById('h').offsetHeight";
    // `#a` is a light node: the page rule gives its size; the shadow padding of `#w` adds 6px to the host height.
    assert_eq!(js(&mut b, src), "40x10 16");
    // the fallback shows while nothing is slotted
    let src = "var g = document.getElementById('g').attachShadow({ mode: 'open' }); g.innerHTML = '<slot><div id=f style=\\'height:7px\\'></div></slot>'; var f = g.getElementById('f'); var n = f.offsetHeight; document.getElementById('g').appendChild(document.createElement('p')).style.height = '9px'; n + ',' + f.offsetHeight + ',' + document.getElementById('g').offsetHeight";
    assert_eq!(js(&mut b, src), "7,0,9");
}

#[test]
fn a_shadow_host_is_laid_out_from_its_flat_tree() {
    // The page rule for `.c` must not reach into the shadow tree. The line height comes to the shadow text through the host.
    let css = ".c { height: 70px } #h { width: 100px; line-height: 40px } #k { width: 50px; height: 50px }";
    let mut b = page(css, "<div id=h><p style='height: 100px'>light</p></div><div id=k></div>");
    let attach = "var r = document.getElementById('h').attachShadow({ mode: 'open' }); var c = document.createElement('div'); c.className = 'c'; r.appendChild(c);";
    assert_eq!(js(&mut b, &format!("{attach} document.getElementById('h').offsetWidth + 'x' + document.getElementById('h').offsetHeight")), "100x0");
    // shadow text, then a shadow block of 30px: 40 + 30. The light `p` is not rendered (no slot takes it).
    assert_eq!(js(&mut b, "r.insertBefore(document.createTextNode('x'), c); var d = r.appendChild(document.createElement('div')); d.style.height = '30px'; document.getElementById('h').offsetHeight"), "70");
    assert_eq!(size(&mut b, "k"), "50x50");
}

#[test]
fn the_next_route_announcer_shape_does_not_block_geometry() {
    // Next.js appends an inline custom element whose shadow tree holds one absolutely positioned 1px region.
    let mut b = page("#k { width: 50px; height: 50px }", "<div id=k></div>");
    let src = "var e = document.createElement('next-route-announcer'); var t = document.createElement('div'); t.style.cssText = 'position:absolute;border:0;height:1px;margin:-1px;padding:0;width:1px;clip:rect(0 0 0 0);overflow:hidden;white-space:nowrap;word-wrap:normal'; e.attachShadow({ mode: 'open' }).appendChild(t); document.body.appendChild(e); var k = document.getElementById('k'); k.offsetWidth + 'x' + k.offsetHeight";
    assert_eq!(js(&mut b, src), "50x50");
}

#[test]
fn an_inline_box_on_two_lines_is_the_union_of_its_fragments() {
    // `br` makes the lines. Line 1 is 4 letters (24px) and line 2 is 6 letters (36px); both start at x = 0, each line box is 22px high.
    let css = "#s { font-size: 10px; background: red }";
    let mut b = page(css, "<div style='width: 200px'><span id=s>aaaa<br>bbbbbb</span></div><span id=p>x</span><br id=br>");
    // the line box is as high as the strut of the 16px div (11 + 3 at 10px, but 17 + 5 = 22 at 16px): the box of line 1 sits at 6..20 and the box of line 2 at 28..42
    assert_eq!(size(&mut b, "s"), "36x36");
    // native layout makes no fragment for an inline box with nothing to paint: its size is that of its text (one 16px letter: 9.6 x 17 + 5)
    assert_eq!(size(&mut b, "p"), "10x22");
    // a `br` is not a box (Chrome: `LayoutBR` is a text, not a `LayoutBoxModelObject`), so zero
    assert_eq!(size(&mut b, "br"), "0x0");
}

#[test]
fn an_inline_box_with_nothing_to_paint_has_the_size_of_its_lines() {
    // A 10px font: a letter is 6px wide and a line is 14px high. `#w` wraps: "aaaa" (24) and "bbbbb" (30) on two lines of 14px.
    let css = "body { font-size: 10px } .box { width: 50px }";
    let body = "<div><span id=a>aaaa</span></div><div class=box><span id=w>aaaa bbbbb</span></div><div>x<span id=e></span></div><br id=br>";
    let mut b = page(css, body);
    assert_eq!(size(&mut b, "a"), "24x14");
    assert_eq!(size(&mut b, "w"), "30x28");
    // an empty inline: no width, the height of its font
    assert_eq!(size(&mut b, "e"), "0x14");
    assert_eq!(size(&mut b, "br"), "0x0");
    // the same boxes as `getClientRects` (one for each line) and `getBoundingClientRect` (their union)
    let rects = "var w = document.getElementById('w'); var r = w.getClientRects(); var u = w.getBoundingClientRect(); \
                 [r.length, r[0].width, r[0].height, r[1].width, r[1].top - r[0].top, u.width, u.height].join()";
    assert_eq!(js(&mut b, rects), "2,24,14,30,14,30,28");
    let one = "var a = document.getElementById('a'); var r = a.getClientRects(); [r.length, r[0].width, r[0].height].join()";
    assert_eq!(js(&mut b, one), "1,24,14");
    let empty = "var e = document.getElementById('e'); var r = e.getClientRects(); var u = e.getBoundingClientRect(); [r.length, r[0].width, r[0].height, u.width, u.height].join()";
    assert_eq!(js(&mut b, empty), "1,0,14,0,14");
}

#[test]
fn an_inline_box_that_wraps_is_the_union_of_its_fragments() {
    // 50px wide, 10px font: "aaaa" (24) and "bbbbb" (30) do not fit with the space (24 + 6 + 30 = 60), so two lines
    let css = "#s { font-size: 10px; background: red } .box { width: 50px }";
    let mut b = page(css, "<div class=box><span id=s>aaaa bbbbb</span></div>");
    assert_eq!(js(&mut b, "document.getElementById('s').offsetHeight"), "36");
    assert_eq!(js(&mut b, "document.getElementById('s').offsetWidth"), "30");
}

#[test]
fn position_and_transform_do_not_change_the_offset_size() {
    let css = "#r { position: relative; top: 7px; left: 9px; width: 10px; height: 12px } #x { transform: scale(3); width: 10px; height: 12px } \
               #a { position: absolute; width: 21px; height: 22px } #f { float: left; width: 31px; height: 32px }";
    let mut b = page(css, "<div id=r></div><div id=x></div><div id=a></div><div id=f></div>");
    assert_eq!(size(&mut b, "r"), "10x12");
    assert_eq!(size(&mut b, "x"), "10x12");
    assert_eq!(size(&mut b, "a"), "21x22");
    assert_eq!(size(&mut b, "f"), "31x32");
}

#[test]
fn a_button_with_authored_css_has_that_size() {
    let css = "button { box-sizing: border-box; width: 16px; height: 16px; padding: 0; border: 0 } #p { padding: 2px; border: 1px solid; width: 10px; height: 10px }";
    let mut b = page(css, "<button id=c type=button></button><button id=p></button>");
    assert_eq!(size(&mut b, "c"), "16x16");
    // `button` sets border-box above, so `#p` is 10 wide with its padding and border inside
    assert_eq!(size(&mut b, "p"), "10x10");
}

#[test]
fn results_are_whole_numbers_that_round_to_nearest() {
    let mut b = page("#a { width: 10.4px; height: 10.5px } #b { width: 10.6px; height: 0.2px } #n { width: 1e30px; height: 5px }", "<div id=a></div><div id=b></div><div id=n></div>");
    assert_eq!(size(&mut b, "a"), "10x11");
    assert_eq!(size(&mut b, "b"), "11x0");
    // layout clamps a length to 33554431px (`layout::block::MAX_PX`), which an f32 holds as 33554432; the result is a finite long
    assert_eq!(js(&mut b, "document.getElementById('n').offsetWidth"), "33554432");
}

#[test]
fn a_loopback_session_script_reads_the_size_that_a_linked_sheet_gives() {
    use crate::browser_loader::Stop;
    use crate::browser_session::{Session, SessionConfig, SessionStop};
    use crate::resources::test_server::{Reply, serve};
    big(|| {
        let srv = serve(|path| match path {
            "/" => Reply::new(
                200,
                Some("text/html; charset=utf-8"),
                "<!doctype html><link rel=stylesheet href=/a.css><button id=c type=button></button><script>\
                 var c = document.getElementById('c'); localStorage.setItem('a', c.offsetWidth + 'x' + c.offsetHeight);\
                 c.className = 'big'; localStorage.setItem('b', c.offsetWidth + 'x' + c.offsetHeight);</script>",
            ),
            "/a.css" => Reply::css("button { box-sizing: border-box; width: 16px; height: 18px; padding: 0; border: 0 } .big { width: 40px; border: 2px solid }"),
            _ => Reply::new(404, Some("text/plain"), "no"),
        });
        let mut cfg = SessionConfig::new(&srv.url("/"));
        cfg.virtual_clock = true;
        cfg.eval = EvalLimits { max_steps: 50_000_000, ..Default::default() };
        let mut s = Session::new(cfg);
        let r = s.run();
        assert_eq!(r.stop, SessionStop::Page(Stop::Settled), "{r:?}");
        assert_eq!(r.pages[0].script_throws, 0, "{r:?}");
        let get = |k: &str| s.storage().get_item(false, &srv.url(""), k);
        assert_eq!(get("a").as_deref(), Some("16x18"));
        // border-box: the border is inside the 40px
        assert_eq!(get("b").as_deref(), Some("40x18"));
    });
}

// ---------------------------------------------------------------------------------------------- limits

fn stats(b: &Browser) -> (u64, u64) {
    let s = b.geometry_stats();
    (s.layouts, s.last_bound)
}

fn refuse(page: &str, l: GeometryLimits, name: &'static str) {
    let mut b = at(page);
    b.set_geometry_limits(l);
    let r = js(&mut b, "var log = []; try { document.getElementById('t').offsetWidth; log.push('after') } catch (e) { log.push('catch') } finally { log.push('finally') }");
    assert_eq!(r, lim(name), "{name}");
    // no catch and no finally ran, and no layout finished
    assert_eq!(js(&mut b, "log.join()"), "");
    assert_eq!(b.geometry_stats().layouts, 0, "{name}");
}

#[test]
fn limits_refuse_before_native_layout_starts_or_stop_it() {
    big(|| {
        let nest = |n: usize| format!("{}<i id=t>x</i>{}", "<div>".repeat(n), "</div>".repeat(n));
        let d = GeometryLimits::default();
        refuse(&nest(20), GeometryLimits { max_depth: 10, ..d }, "geometry-depth");
        refuse(&format!("<div id=t></div>{}", "<p></p>".repeat(50)), GeometryLimits { max_nodes: 20, ..d }, "geometry-nodes");
        refuse(&format!("<div id=t>{}</div>", "x".repeat(300)), GeometryLimits { max_text: 100, ..d }, "geometry-text");
        // 10 nested inline boxes around 20 letters: each one can be on 21 lines, so the bound is far above 100
        refuse(&format!("<div id=t>{}{}{}</div>", "<b>".repeat(10), "ab".repeat(10), "</b>".repeat(10)), GeometryLimits { max_fragments: 100, ..d }, "geometry-fragments");
        // layout counts only what it does again: 10 stretched rows lay out 10 times again, so 4 units are not enough
        refuse(&stretched_rows(10), GeometryLimits { max_work: 4, ..d }, "geometry-work");
    });
}

#[test]
fn the_step_budget_is_reserved_before_layout() {
    big(|| {
        let mut b = Browser::new(tree::parse(&format!("<body>{}<p id=t>x</p>", "<div></div>".repeat(1000))), BrowserLimits::default(), EvalLimits { max_steps: 2000, ..Default::default() }, HeapLimits::default()).unwrap();
        assert_eq!(js(&mut b, "document.getElementById('t').offsetWidth"), "limit:Steps");
        assert_eq!(b.geometry_stats().layouts, 0);
    });
}

/// `n` flex rows inside one another, with a sibling at each level that is taller the nearer it is to the outside, so every row is stretched.
fn stretched_rows(n: usize) -> String {
    let mut page = String::from("<body style='margin:0'>");
    for k in 0..n {
        page += &format!("<div{} style='display:flex'>", if k == 0 { " id=o" } else if k == n / 2 { " id=m" } else { "" });
    }
    page += "<span id=t>text</span>";
    for k in (0..n).rev() {
        page += &format!("<div style='height:{}px'></div></div>", 10 + 20 * (n - 1 - k));
    }
    page
}

#[test]
fn work_is_charged_as_steps_after_layout_counted_it() {
    big(|| {
        // The walk of this page costs about 150 steps, and its layout about 40,000 units. 200 steps pay the walk and not the layout.
        let mut b = Browser::new(tree::parse(&stretched_rows(30)), BrowserLimits::default(), EvalLimits { max_steps: 200, ..Default::default() }, HeapLimits::default()).unwrap();
        assert_eq!(js(&mut b, "document.getElementById('o').offsetHeight"), "limit:Steps");
        assert_eq!(b.geometry_stats().layouts, 0);
        let mut b = at(&stretched_rows(30));
        assert_eq!(js(&mut b, "document.getElementById('o').offsetHeight"), "590");
        // 63 boxes: each is laid out once for each height that reaches it, and not 2^30 times. The work grows with the cube of the levels.
        assert!(b.geometry_stats().last_work < 63 * 30 * 30, "{}", b.geometry_stats().last_work);
    });
}

#[test]
fn deeply_nested_stretched_flex_rows_read_in_bounded_work() {
    big(|| {
        // 80 levels is under `max_depth` (96). Each row is as tall as the tallest sibling, 10 + 20 * 79. The work is under `max_work`.
        let mut b = at(&stretched_rows(80));
        assert_eq!(js(&mut b, "document.getElementById('o').offsetHeight + ',' + document.getElementById('m').offsetHeight"), "1590,1590");
        assert!(b.geometry_stats().last_work < 163 * 80 * 80, "{}", b.geometry_stats().last_work);
    });
}

#[test]
fn scroll_containers_nested_in_stretched_rows_stop_with_a_work_limit_in_bounded_time() {
    big(|| {
        // Every level is a scroll container that needs a bar inside a stretched row. The first pass of a box with a bar runs
        // without the map of results, so this page doubles at each level: 2^30 layouts. `max_work` stops it.
        let mut page = String::from("<body style='margin:0'>");
        for _ in 0..30 {
            page += "<div style='display:flex'><div style='overflow:auto;max-height:300px'>";
        }
        page += "<span id=t>text</span>";
        for k in (0..30).rev() {
            page += &format!("</div><div style='height:{}px'></div></div>", 10 + 20 * (29 - k));
        }
        let mut b = at(&page);
        let start = std::time::Instant::now();
        assert_eq!(js(&mut b, "document.getElementById('t').offsetWidth"), lim("geometry-work"));
        // The work limit is the proof. This bound only catches a hang, so it is loose. Measured on 2026-11-06 in a debug build:
        // 4 s alone, 8.4 s with two stress workers on the same core. A 20 s bound failed once at load 54 to 76 (reported, not reproduced).
        assert!(start.elapsed().as_secs() < 120, "{:?}", start.elapsed());
        assert_eq!(b.geometry_stats().layouts, 0);
    });
}

/// A page with the shape of a Next.js and Tailwind dashboard that was measured on 2026-11-06: 9 levels of boxes that layout can lay out
/// more than once (flex, grid, table, inline-flex, sticky and relative boxes) in 17 levels of elements. Invented content and styles.
/// The first geometry read of it used to stop with `geometry-nesting` (more than 4 levels), and `geometry-work` would have been 4^9 steps.
const DASHBOARD: &str = include_str!("../../../tools/fixtures/ui/geometry-dashboard.html");

#[test]
fn a_dashboard_shaped_page_reads_geometry_with_the_default_limits() {
    big(|| {
        let mut b = at(DASHBOARD);
        // header: height 52 with border-box sizing. cell: 160 x 12. dot: 7 x 7. section: a column of 1fr is not less than the least width
        // of its table (160 + 5 * 64 + 6 * 2 * 8 = 576), and the section adds 2 * (1 + 16): 610, over the 608 that 1fr alone gives. app: the body margin is 0.
        let r = js(&mut b, "var q = function (s) { var e = document.querySelector(s); return e.offsetWidth + 'x' + e.offsetHeight }; [q('header.bar'), q('.cell'), q('.dot'), document.querySelector('section').offsetWidth, document.getElementById('app').offsetWidth].join()");
        assert_eq!(r, "1280x52,160x12,7x7,610,1280");
        let s = b.geometry_stats();
        assert!(s.layouts >= 1 && s.last_work < 2000, "{s:?}");
        // a read of an element deep in the page, after a change, is a new layout
        assert_eq!(js(&mut b, "var c = document.querySelector('.cell'); c.style.width = '33px'; c.offsetWidth"), "33");
    });
}

#[test]
fn the_bound_is_above_the_fragments_that_layout_made() {
    big(|| {
        let pages = [
            "<div id=t><span>aaaa <b>bbbb cccc</b> dddd<br>eeee</span><ul><li>one<li>two</ul></div>",
            "<div id=t style='display:flex'><div>a</div><div style='display:flex'><div>b</div><div>c d e</div></div></div>",
            "<div id=t style='display:grid;grid-template-columns:1fr 1fr'><div>a</div><div>b c</div><div>d</div></div>",
            "<table id=t><tr><td>a<td>b<tr><td>c<td>d</table>",
            "<div id=t style='overflow:auto;height:20px'><p>a</p><p>b</p><p style='overflow:auto;height:5px'>c<br>d<br>e</p></div>",
            "<div id=t><span style='display:inline-block'>a <span style='display:inline-block'>b c <i>d</i></span></span> <span style='float:left'>f</span><p style='position:absolute'>g</p></div>",
        ];
        for p in pages {
            let mut b = page("", p);
            assert_eq!(js(&mut b, "typeof document.getElementById('t').offsetWidth"), "number", "{p}");
            let s = b.geometry_stats();
            assert_eq!(s.layouts, 1, "{p}");
            assert!(s.last_fragments > 0 && s.last_fragments <= s.last_bound, "{p}: {} fragments, bound {}", s.last_fragments, s.last_bound);
        }
    });
}

#[test]
fn reads_of_an_unchanged_page_share_one_layout() {
    let mut b = page("#t { width: 5px }", "<div id=t></div>");
    assert_eq!(size(&mut b, "t"), "5x0");
    assert_eq!(size(&mut b, "t"), "5x0");
    // two reads of two numbers: one layout
    assert_eq!(stats(&b).0, 1);
    // a change of the page lays it out again, and the next read sees it
    assert_eq!(js(&mut b, "document.getElementById('t').style.height = '7px'; 1"), "1");
    assert_eq!(size(&mut b, "t"), "5x7");
    assert_eq!(stats(&b).0, 2);
}

/// Load model for the capped runs (`SCALE=1|10|100`, `SHAPE=wide|deep|text|flex`, `--ignored`).
#[test]
#[ignore]
fn load_model() {
    let scale: usize = std::env::var("SCALE").ok().and_then(|s| s.parse().ok()).unwrap_or(1);
    let shape = std::env::var("SHAPE").unwrap_or_else(|_| "wide".into());
    let stack = if cfg!(debug_assertions) { 64 << 20 } else { 8 << 20 };
    std::thread::Builder::new().stack_size(stack).spawn(move || {
        let body = match shape.as_str() {
            "deep" => format!("{}<div id=t>x</div>{}", "<div>".repeat(scale * 10), "</div>".repeat(scale * 10)),
            "text" => format!("<p id=t>{}</p>", "word ".repeat(scale * 100)),
            // the largest accepted work: 4 levels of boxes that layout can lay out again around one long word, and around many items
            "word" => format!("<div style='display:flex'><span style='display:inline-block'><div style='display:grid'><span style='display:inline-block' id=t>{}</span></div></span></div>", "a".repeat(scale.min(65_000))),
            "many" => format!("<div style='display:flex;flex-wrap:wrap'><div style='display:grid'><div style='overflow:auto;height:50px' id=t>{}</div></div></div>", "<span style='display:inline-block'>ab</span> ".repeat(scale.min(3000))),
            "flex" => format!("{}<div id=t>x</div>{}", "<div style='display:flex'>".repeat(scale.min(4)), "</div>".repeat(scale.min(4))),
            _ => (0..scale * 100).map(|i| format!("<div id=d{i} style='width:{}px;height:3px'><p>x</p></div>", 10 + i % 50)).collect::<String>() + "<div id=t>x</div>",
        };
        let started = std::time::Instant::now();
        let mut b = Browser::new(tree::parse(&format!("<body>{body}")), BrowserLimits::default(), EvalLimits { max_steps: 2_000_000_000, ..Default::default() }, HeapLimits::default()).unwrap();
        let r = js(&mut b, "document.getElementById('t').offsetWidth + 'x' + document.getElementById('t').offsetHeight");
        let s = b.geometry_stats();
        if shape == "wide" && [1, 10, 100].contains(&scale) {
            if scale == 100 {
                assert_eq!(r, lim("geometry-nodes"));
                assert_eq!(s.layouts, 0);
            } else {
                // UA body margins: 1280 - 2*8. The 16px font has round(16*1.069) + round(16*.293) = 22px lines.
                assert_eq!(r, "1264x22");
                assert_eq!(s.layouts, 2);
                assert_eq!(s.last_fragments, (300 * scale + 4) as u64);
                assert!(s.last_fragments <= s.last_bound);
            }
        }
        println!("{shape} scale {scale}: {r}, layouts {}, fragments {} (bound {}), {:?}", s.layouts, s.last_fragments, s.last_bound, started.elapsed());
    }).unwrap().join().unwrap();
}


/// Stack probe (`DEPTH`, `SHAPE=div|span|flex|ib|table|list`, `STACK_KB`, `--ignored`): layout at that depth on a stack of that size.
#[test]
#[ignore]
fn stack_probe() {
    let n: usize = std::env::var("DEPTH").ok().and_then(|s| s.parse().ok()).unwrap_or(96);
    let kb: usize = std::env::var("STACK_KB").ok().and_then(|s| s.parse().ok()).unwrap_or(8192);
    let shape = std::env::var("SHAPE").unwrap_or_else(|_| "div".into());
    let (open, close) = match shape.as_str() {
        "span" => ("<span style='border:1px solid'>", "</span>"),
        "flex" => ("<div style='display:flex'>", "</div>"),
        "ib" => ("<span style='display:inline-block'>", "</span>"),
        "table" => ("<table><tr><td>", "</table>"),
        "auto" => ("<div style='overflow:auto;height:5px'>", "</div>"),
        "list" => ("<ul><li>", "</ul>"),
        _ => ("<div>", "</div>"),
    };
    std::thread::Builder::new().stack_size(kb << 10).spawn(move || {
        let page = format!("<body>{}<div id=t>x</div>{}", open.repeat(n), close.repeat(n));
        let mut b = Browser::new(tree::parse(&page), BrowserLimits::default(), EvalLimits { max_steps: u64::MAX / 2, ..Default::default() }, HeapLimits::default()).unwrap();
        b.set_geometry_limits(GeometryLimits { max_depth: 10_000, max_work: u64::MAX, max_nodes: 1 << 20, ..Default::default() });
        println!("{shape} depth {n}: {}", js(&mut b, "document.getElementById('t').parentNode.offsetWidth"));
    }).unwrap().join().unwrap();
}

#[test]
fn hidden_subtrees_do_not_consume_layout_bounds() {
    big(|| {
        let mut b = page("#t { width: 16px; height: 18px } #hidden { display: none }", &format!("<button id=t></button><script>{}</script><div id=hidden>{}</div>", "x".repeat(70_000), "y".repeat(70_000)));
        assert_eq!(size(&mut b, "t"), "16x18");
    });
}

#[test]
fn hidden_shadow_hosts_do_not_gate_visible_geometry() {
    let mut b = page("#t { width: 16px; height: 18px } #hidden { display: none }", "<button id=t></button><div id=hidden></div>");
    assert_eq!(js(&mut b, "document.getElementById('hidden').attachShadow({mode:'open'}); 'ok'"), "ok");
    assert_eq!(size(&mut b, "t"), "16x18");
    assert_eq!(js(&mut b, "var host = document.createElement('div'); document.body.appendChild(host); host.attachShadow({mode:'open'}); 'ok'"), "ok");
    assert_eq!(size(&mut b, "hidden"), "0x0");
}

/// A loop of reads over a page that does not change lays it out once, and each read after the first charges one step. The step limit
/// here is far under the cost of 5,000 full reads (a cascade of 600 nodes against 40 rules, and a layout, for each read).
#[test]
fn a_loop_of_reads_on_an_unchanged_page_lays_it_out_once() {
    big(|| {
        let rules: String = (0..40).map(|i| format!(".c{i} {{ margin-left: {i}px }}")).collect();
        let body: String = (0..300).map(|i| format!("<div id=d{i} class=c{}><p>text {i}</p></div>", i % 40)).collect();
        let src = format!("<!doctype html><html><head><style>{rules}</style></head><body>{body}</body></html>");
        let mut b = Browser::new(tree::parse(&src), BrowserLimits::default(), EvalLimits { max_steps: 5_000_000, ..Default::default() }, HeapLimits::default()).expect("browser");
        let r = js(&mut b, "var n = 0; for (var i = 0; i < 5000; i++) { var e = document.getElementById('d' + (i % 300)); n += e.scrollWidth + e.offsetWidth + e.getBoundingClientRect().width } n > 0");
        assert_eq!(r, "true");
        // One layout, and a second one at the narrower size when the page bar appears (`layout_areas`).
        let first = b.geometry_stats().layouts;
        assert!(first <= 2, "{first} layouts");
        // A change of the page is seen by the next read, and lays it out again. The reads after it use that layout.
        assert_eq!(js(&mut b, "var e = document.getElementById('d0'); e.style.width = '50px'; e.offsetWidth + ',' + e.scrollWidth + ',' + e.offsetWidth"), "50,50,50");
        let second = b.geometry_stats().layouts;
        assert!(second > first && second <= first + 2, "{first} then {second} layouts");
        assert_eq!(js(&mut b, "document.getElementById('d1').offsetWidth > 50"), "true");
        assert_eq!(b.geometry_stats().layouts, second);
    });
}
