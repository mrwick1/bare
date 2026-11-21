//! Tests of the box of an `<svg>` (SVG 2 sizing, read through `getBoundingClientRect`).
//! The Chrome cases (`tools/fixtures/ui/svg-cases.txt`): place and size of each box against headless Chrome 155.

use crate::browser_topic_tests::topic_test;

topic_test!(chrome_cases, "svg", 7);

use crate::browser::*;
use crate::control::Refusal;
use crate::page::{Page, Source};
use bare_html::tree;
use bare_js::eval::EvalLimits;
use bare_js::heap::HeapLimits;

/// A d3 tree page. Chrome 155 (headless, `getBoundingClientRect` and `getBBox`) gives for it: r1 60,80,80,30; n1 60,80,80,30; r2 190,115,60,30;
/// c2 212,122,16,16; p1 250,70,40,50; bbox of r1 0,0,80,30, of n2 -10,-5,60,30, of c2 12,2,16,16, of p1 0,0,40,50.
const TREE: &str = r##"<!doctype html><body style="margin:0"><div style="height:20px"></div>
<svg id=s width=400 height=200 style="display:block">
<g transform="translate(50,40)"><g class="node" id=n1 transform="translate(10,20)"><rect class="body" id=r1 x="0" y="0" width="80" height="30" fill="#ccc" stroke="#000" stroke-width="2"/><text id=t1 x="5" y="20" font-size="14" style="pointer-events:none">Node</text></g>
<g class="node" id=n2 transform="translate(150,60)"><rect class="body" id=r2 x="-10" y="-5" width="60" height="30" fill="#ccc"/><circle id=c2 cx="20" cy="10" r="8"/></g>
<path id=p1 d="M0,0 L40,10 L20,50 Z" transform="translate(200,10)"/></g></svg>
<script>window.log = []; document.querySelectorAll('g.node rect.body').forEach(function (r) { r.addEventListener('click', function (e) { log.push(r.id + ':' + e.target.id) }) });</script></body>"##;

fn page() -> Page {
    let mut b = Browser::new(tree::parse(TREE), BrowserLimits::default(), EvalLimits { max_steps: 50_000_000, ..Default::default() }, HeapLimits::default()).expect("browser");
    b.set_viewport(400.0, 300.0).unwrap();
    b.run_script(TREE.split_once("<script>").unwrap().1.split_once("</script>").unwrap().0).unwrap();
    let mut p = Page::new(Source::Browser(Box::new(b.into())), (400, 300));
    p.quiet = true;
    p
}

fn js(p: &mut Page, code: &str) -> String {
    let b = p.browser().unwrap();
    let v = b.run_script(code).unwrap();
    b.it.display(v)
}

#[test]
fn an_svg_shape_has_a_box_in_the_window() {
    std::thread::Builder::new().stack_size(64 << 20).spawn(|| {
        let mut p = page();
        let rect = |p: &mut Page, id: &str| js(p, &format!("var r = document.getElementById('{id}').getBoundingClientRect(); [r.x, r.y, r.width, r.height].join()"));
        assert_eq!(rect(&mut p, "r1"), "60,80,80,30");
        assert_eq!(rect(&mut p, "n1"), "60,80,80,30", "a g holds what is inside it");
        assert_eq!(rect(&mut p, "r2"), "190,115,60,30");
        assert_eq!(rect(&mut p, "c2"), "212,122,16,16");
        assert_eq!(rect(&mut p, "p1"), "250,70,40,50");
        assert_eq!(js(&mut p, "document.getElementById('r1').getClientRects().length"), "1");
        assert_eq!(js(&mut p, "document.getElementById('s').getClientRects().length"), "1");
        // The text has the line box of its font (the face and its numbers are not Chrome's), at its place.
        assert_eq!(js(&mut p, "var r = document.getElementById('t1').getBoundingClientRect(); [r.x, r.width > 0, r.height > 0, r.bottom > 100 - 5].join()"), "65,true,true,true");
    }).unwrap().join().unwrap();
}

#[test]
fn get_bbox_is_the_user_space_box() {
    std::thread::Builder::new().stack_size(64 << 20).spawn(|| {
        let mut p = page();
        let bbox = |p: &mut Page, id: &str| js(p, &format!("var r = document.getElementById('{id}').getBBox(); [r.x, r.y, r.width, r.height].join()"));
        assert_eq!(bbox(&mut p, "r1"), "0,0,80,30");
        assert_eq!(bbox(&mut p, "n1"), "0,0,80,30", "before its own transform");
        assert_eq!(bbox(&mut p, "n2"), "-10,-5,60,30");
        assert_eq!(bbox(&mut p, "c2"), "12,2,16,16");
        assert_eq!(bbox(&mut p, "p1"), "0,0,40,50");
    }).unwrap().join().unwrap();
}

#[test]
fn a_click_on_an_svg_shape_reaches_it() {
    std::thread::Builder::new().stack_size(64 << 20).spawn(|| {
        let mut p = page();
        let h = p.query_all("g.node rect.body").unwrap();
        assert_eq!(h.len(), 2);
        // The circle c2 lies over the centre of r2: the click is refused as covered, as Playwright does.
        assert!(matches!(p.click(h[1]), Err(Refusal::Covered { ref by, .. }) if by == "circle#c2"));
        let (x, y) = p.click(h[0]).unwrap_or_else(|r| panic!("{r:?}"));
        assert_eq!((x, y), (100.0, 95.0));
        assert_eq!(js(&mut p, "log.join()"), "r1:r1");
        // The hit test finds the shape, and the g and the svg under it.
        assert_eq!(js(&mut p, "document.elementsFromPoint(100, 95).map(function (e) { return e.id || e.tagName }).join()"), "r1,n1,g,s,BODY,HTML");
        assert_eq!(js(&mut p, "document.elementsFromPoint(240, 125).map(function (e) { return e.id || e.tagName }).join()"), "r2,n2,g,s,BODY,HTML");
        assert_eq!(js(&mut p, "document.elementFromPoint(220, 130).id"), "c2");
        // A point of the svg box that no shape covers is the svg's.
        assert_eq!(js(&mut p, "document.elementFromPoint(10, 190).id"), "s");
    }).unwrap().join().unwrap();
}
