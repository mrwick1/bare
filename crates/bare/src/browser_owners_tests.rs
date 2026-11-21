//! Tests of the owners of the browser tables (S0.4 PR 5). An object that the page made, and no script holds, frees its table entry at the next
//! collection (`Host::prune` gets its key). An object that a script holds keeps its entry and its references. Each test runs on a big stack.

use crate::browser::*;
use bare_html::tree;
use bare_js::eval::EvalLimits;
use bare_js::heap::HeapLimits;

fn big<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(64 << 20).spawn(f).unwrap().join().unwrap()
}

fn browser() -> Browser {
    let mut b = Browser::new(tree::parse("<!doctype html><html><body><div id=a class=c x=1>hi</div></body></html>"), BrowserLimits::default(), EvalLimits { max_steps: 50_000_000, ..Default::default() }, HeapLimits::default()).expect("browser");
    b.use_virtual_clock_at(1000.0).unwrap();
    b
}

fn js(b: &mut Browser, src: &str) -> String {
    let r = b.run_script(src).expect(src);
    b.it.display(r)
}

/// The entries of the tables whose objects are owners of the kinds of this PR, in a fixed order.
fn tables(b: &Browser) -> Vec<usize> {
    let d = &b.dom;
    vec![
        d.resize_entries(),
        d.mo_entries(),
        d.io_entries(),
        d.walk_entries(),
        d.range_entries(),
        d.attr.s.borrow().attrs.len(),
        crate::browser_web::entries(d)[0],
        crate::browser_web::entries(d)[1],
        crate::browser_web::entries(d)[2],
        d.geom.slots.borrow().len(),
        d.plat.s.borrow().mql.len(),
        d.plat.s.borrow().iters.len(),
        d.st.borrow().owners.objs.len(),
    ]
}

/// The objects of each kind that the tests make, as expressions.
const KINDS: &str = "new ResizeObserver(function () {}), new MutationObserver(function () {}), new IntersectionObserver(function () {}), document.createTreeWalker(document.body), \
    document.createRange(), document.createAttribute('x'), fd, fd.entries(), new Blob(['a']), new DOMRect(1, 2, 3, 4), new DOMPoint(), matchMedia('(min-width: 1px)'), \
    document.getElementById('a').classList.values(), new MessageChannel(), new EventTarget()";

/// Makes 20 objects of each kind and keeps none: the script ends with `void 0`, so the completion value holds nothing.
fn drop_all() -> String {
    format!("for (var i = 0; i < 20; i++) {{ var fd = new FormData(); fd.append('a', 'b'); [{KINDS}].length; }} fd = null; void 0")
}

/// The same objects, all held by `keep`.
fn keep_all() -> String {
    format!("var keep = []; for (var i = 0; i < 20; i++) {{ var fd = new FormData(); fd.append('a', 'b'); keep.push({KINDS}); }} fd = null; void 0")
}

#[test]
fn a_dropped_object_frees_its_entry() {
    big(|| {
        let mut b = browser();
        b.collect(&[]).unwrap();
        let base = tables(&b);
        js(&mut b, &drop_all());
        let made = tables(&b);
        for (i, (m, o)) in made.iter().zip(&base).enumerate() {
            assert!(m > o, "table {i} did not grow: {base:?} -> {made:?}");
        }
        b.collect(&[]).unwrap();
        assert_eq!(tables(&b), base, "every entry of a dropped object is gone");
    });
}

#[test]
fn a_held_object_keeps_its_entry_until_it_is_dropped() {
    big(|| {
        let mut b = browser();
        b.collect(&[]).unwrap();
        let base = tables(&b);
        js(&mut b, &keep_all());
        let made = tables(&b);
        b.collect(&[]).unwrap();
        b.collect(&[]).unwrap();
        assert_eq!(tables(&b), made, "a held object keeps its entry");
        js(&mut b, "keep = null; void 0");
        b.collect(&[]).unwrap();
        assert_eq!(tables(&b), base, "the entries go with the objects");
    });
}

#[test]
fn an_observer_that_observes_and_a_held_one_keep_their_callback() {
    big(|| {
        let mut b = browser();
        js(&mut b, "var log = []; var held = new MutationObserver(function (r) { log.push(r.length) }); held.observe(document.body, { childList: true }); \
            new MutationObserver(function (r) { log.push('x') }).observe(document.body, { childList: true }); void 0");
        b.collect(&[]).unwrap();
        b.collect(&[]).unwrap();
        js(&mut b, "document.body.appendChild(document.createElement('p')); void 0");
        b.drain().unwrap();
        let mut seen = js(&mut b, "log.slice().sort().join()");
        seen.retain(|c| c != '"');
        assert_eq!(seen, "1,x", "both observers kept their callbacks across two collections");
    });
}
