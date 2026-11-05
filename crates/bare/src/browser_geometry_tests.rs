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
fn a_shadow_tree_is_a_gate_and_not_a_guess() {
    let mut b = page("", "<div id=h></div>");
    let gate = "unsupported:computed style in a shadow tree (shadow cascade is not implemented)";
    assert_eq!(js(&mut b, "var r = document.getElementById('h').attachShadow({ mode: 'open' }); var i = r.appendChild(document.createElement('i')); i.offsetWidth"), gate);
    // a host with a shadow root is rendered from the flat tree, which is not built
    assert_eq!(js(&mut b, "document.getElementById('h').offsetWidth"), "unsupported:geometry of a shadow host (flat tree layout is not built)");
    // a shadow tree that is not attached to the document has no box
    assert_eq!(js(&mut b, "var h2 = document.createElement('div'); var r2 = h2.attachShadow({ mode: 'open' }); r2.appendChild(document.createElement('b')).offsetWidth"), "0");
}

#[test]
fn an_inline_box_on_two_lines_is_the_union_of_its_fragments() {
    // `br` makes the lines. Line 1 is 4 letters (24px) and line 2 is 6 letters (36px); both start at x = 0, each line box is 22px high.
    let css = "#s { font-size: 10px; background: red }";
    let mut b = page(css, "<div style='width: 200px'><span id=s>aaaa<br>bbbbbb</span></div><span id=p>x</span><br id=br>");
    // the line box is as high as the strut of the 16px div (11 + 3 at 10px, but 17 + 5 = 22 at 16px): the box of line 1 sits at 6..20 and the box of line 2 at 28..42
    assert_eq!(size(&mut b, "s"), "36x36");
    // native layout makes no fragment for an inline box with nothing to paint: a gate, not a zero
    let gate = "unsupported:geometry of an inline box with no background and no border (native layout makes no fragment for it)";
    assert_eq!(size(&mut b, "p"), gate);
    assert_eq!(size(&mut b, "br"), gate);
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
    // no catch and no finally ran, and native layout did not start
    assert_eq!(js(&mut b, "log.join()"), "");
    assert_eq!(b.geometry_stats().layouts, 0, "{name}");
}

#[test]
fn limits_refuse_before_native_layout_starts() {
    big(|| {
        let nest = |n: usize| format!("{}<i id=t>x</i>{}", "<div>".repeat(n), "</div>".repeat(n));
        let d = GeometryLimits::default();
        refuse(&nest(20), GeometryLimits { max_depth: 10, ..d }, "geometry-depth");
        refuse(&format!("<div id=t></div>{}", "<p></p>".repeat(50)), GeometryLimits { max_nodes: 20, ..d }, "geometry-nodes");
        refuse(&format!("<div id=t>{}</div>", "x".repeat(300)), GeometryLimits { max_text: 100, ..d }, "geometry-text");
        // 10 nested inline boxes around 20 letters: each one can be on 21 lines, so the bound is far above 100
        refuse(&format!("<div id=t>{}{}{}</div>", "<b>".repeat(10), "ab".repeat(10), "</b>".repeat(10)), GeometryLimits { max_fragments: 100, ..d }, "geometry-fragments");
        // inline-blocks inside inline-blocks: each level can lay its content out again
        let blocks = format!("<div id=t>{}x{}</div>", "<span style='display:inline-block'>".repeat(5), "</span>".repeat(5));
        refuse(&blocks, GeometryLimits { max_nesting: 3, ..d }, "geometry-nesting");
        refuse(&nest(3), GeometryLimits { max_work: 4, ..d }, "geometry-work");
    });
}

#[test]
fn the_step_budget_is_reserved_before_layout() {
    big(|| {
        let mut b = Browser::new(tree::parse(&format!("<body>{}<p id=t>x</p>", "<div></div>".repeat(200))), BrowserLimits::default(), EvalLimits { max_steps: 2000, ..Default::default() }, HeapLimits::default()).unwrap();
        assert_eq!(js(&mut b, "document.getElementById('t').offsetWidth"), "limit:Steps");
        assert_eq!(b.geometry_stats().layouts, 0);
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
fn nothing_is_kept_between_reads() {
    let mut b = page("#t { width: 5px }", "<div id=t></div>");
    assert_eq!(size(&mut b, "t"), "5x0");
    assert_eq!(size(&mut b, "t"), "5x0");
    // two reads of two numbers: one layout for each
    assert_eq!(stats(&b).0, 4);
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
        b.set_geometry_limits(GeometryLimits { max_depth: 10_000, max_nesting: 10_000, max_work: u64::MAX, max_nodes: 1 << 20, ..Default::default() });
        println!("{shape} depth {n}: {}", js(&mut b, "document.getElementById('t').parentNode.offsetWidth"));
    }).unwrap().join().unwrap();
}
