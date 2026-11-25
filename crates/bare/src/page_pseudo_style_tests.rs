//! A `::before` or `::after` node keeps its style in the frames after the one that made it (`Styles::style`, `generated::sync`).

use crate::browser::*;
use crate::eng_new::Peek;
use crate::js_limits::{EvalLimits, HeapLimits};
use crate::page::{Page, Source};
use bare_html::tree;

fn big<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(64 << 20).spawn(f).unwrap().join().unwrap()
}

fn page(html: &str, w: u32, h: u32) -> Page {
    let mut b = Browser::new(tree::parse(html), BrowserLimits::default(), EvalLimits { max_steps: 50_000_000, ..Default::default() }, HeapLimits::default()).expect("browser");
    b.use_virtual_clock();
    b.set_viewport(w as f32, h as f32).expect("viewport");
    b.set_document_url("http://localhost/cover.html").expect("url");
    let mut p = Page::new(Source::Browser(Box::new(b.into())), (w, h));
    p.whole = 0;
    p
}

fn js(p: &mut Page, src: &str) -> String {
    let b = p.browser().unwrap();
    let v = b.run_script(src).unwrap_or_else(|e| panic!("{src}: {e:?}"));
    let out = b.it.display(v);
    p.touched();
    out
}

/// A `::before` node made by one frame keeps its style in the next frames, after a change of the tree.
#[test]
fn a_pseudo_node_keeps_its_style_after_a_change() {
    big(|| {
        let mut p = page("<!doctype html><style>.a::before{content:'x'} .a::after{content:'y';display:block} .a:hover{color:red} body{margin:0}</style><body><div id=r><div class=a>t</div></div>", 300, 200);
        p.paint();
        p.check_frame().unwrap();
        for (i, src) in ["document.getElementById('r').append(document.createElement('div')); 0", "document.querySelector('.a').className = 'a b'; 0", "document.getElementById('r').prepend(document.createElement('p')); 0", "document.body.append(document.createElement('div')); document.body.className = 'z'; 0"].iter().enumerate() {
            js(&mut p, src);
            p.paint();
            p.check_frame().unwrap_or_else(|e| panic!("step {i}: {e}"));
        }
        // A hover that changes only a color: a new cascade with the same layout (`Page::render_all`).
        p.handle(bare_window::Event::PointerMove { x: 5.0, y: 30.0, buttons: 0, mods: bare_window::Mods { shift: false, ctrl: false, alt: false, meta: false } });
        p.paint();
        p.check_frame().unwrap_or_else(|e| panic!("hover: {e}"));
    });
}
