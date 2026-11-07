//! A box with `opacity` under 1 keeps the pixels behind it while it is drawn. After the draw, the painter must not keep
//! that copy: it is as large as the window, and a window draws it again for each frame. A counting allocator gives the
//! live heap.

use bare_css::cascade::{Origin, RuleSet, UA_CSS};
use bare_html::tree;
use bare_paint::canvas::Canvas;
use bare_paint::Painter;
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};

struct Counting;
static LIVE: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        LIVE.fetch_add(l.size(), Relaxed);
        unsafe { System.alloc(l) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        LIVE.fetch_sub(l.size(), Relaxed);
        unsafe { System.dealloc(p, l) }
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
        LIVE.fetch_add(n, Relaxed);
        LIVE.fetch_sub(l.size(), Relaxed);
        unsafe { System.realloc(p, l, n) }
    }
}

#[global_allocator]
static A: Counting = Counting;

#[test]
fn a_window_size_layer_leaves_no_copy_of_the_window_behind() {
    let (w, h) = (1280u32, 800u32);
    let html = "<body style='margin:0'><div style='opacity:.9;width:1280px;height:800px;background:#eee'><p>hi</p></div>";
    let mut doc = tree::parse(html);
    let vp = (w as f32, h as f32);
    let mut rules = RuleSet::new(vp);
    rules.add(&mut doc, UA_CSS, Origin::UserAgent);
    let styles = rules.compute(&doc);
    let layout = bare_layout::layout(&doc, &styles, vp);
    let mut canvas = Canvas::new(w, h);
    let mut painter = Painter::new();
    let before = LIVE.load(Relaxed);
    painter.paint(&doc, &styles, &layout, &mut canvas);
    let kept = LIVE.load(Relaxed) - before;
    assert!(canvas.pixel(100, 100) != 0xffff_ffff, "the layer was drawn");
    assert!(kept < (w * h) as usize, "the painter keeps {kept} B after a draw; the window is {} B", w * h * 4);
}
