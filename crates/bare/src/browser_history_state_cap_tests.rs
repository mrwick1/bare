//! The size caps of a `history` state: one state, and all states of a document. A state over a cap is a `DataCloneError` that the page can catch
//! (a page-controlled size never stops the page). The numbers are in `NavLimits` and `docs/decisions.md`.

use crate::browser::*;
use crate::browser_navigation::NavLimits;
use bare_html::tree;
use bare_js::eval::{Error, EvalLimits};
use bare_js::heap::HeapLimits;
use bare_js::value::Value;

fn big<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(64 << 20).spawn(f).unwrap().join().unwrap()
}

fn at(url: &str) -> Browser {
    let page = tree::parse("<!doctype html><html><head></head><body></body></html>");
    let mut b = Browser::new(page, BrowserLimits::default(), EvalLimits { max_steps: 50_000_000, ..Default::default() }, HeapLimits::default()).expect("browser");
    b.set_document_url(url).expect("url");
    b
}

fn js(b: &mut Browser, src: &str) -> String {
    match b.run_script(src) {
        Ok(v) => b.it.display(v),
        Err(Error::Throw(v)) => format!("throw:{}", b.it.display(v)),
        Err(e) => format!("other:{e:?}"),
    }
}

/// `pushState(v)` in a `try`: "ok", or the name of the error, its code and whether it is a `DOMException`.
const TRY: &str = "function push(v) { try { history.pushState(v, ''); return 'ok' } catch (e) { return e.name + ':' + e.code + ':' + (e instanceof DOMException) } }
    function rep(s, n) { var o = ''; while (n > 0) { if (n & 1) o += s; s += s; n >>= 1 } return o }";

#[test]
fn the_default_caps_take_a_state_of_the_size_that_npmjs_pushes() {
    big(|| {
        let mut b = at("https://a.test/");
        js(&mut b, TRY);
        // npmjs.com pushes one state of about 1.5 MB (9,301 objects) on load
        assert_eq!(js(&mut b, "push(rep('x', 800000))"), "ok");
        assert_eq!(js(&mut b, "var a = []; for (var i = 0; i < 12000; i++) a.push({i: i}); push(a)"), "ok");
        assert_eq!(b.navigation_stats().history_len, 3);
    });
}

#[test]
fn a_state_over_a_cap_is_a_data_clone_error_that_the_page_catches() {
    big(|| {
        let mut b = at("https://a.test/");
        b.set_navigation_limits(NavLimits { max_state_bytes: 1000, max_total_state_bytes: 2400, max_state_nodes: 20, ..Default::default() });
        js(&mut b, TRY);
        assert_eq!(js(&mut b, "push(rep('x', 600))"), "DataCloneError:25:true");
        assert_eq!(js(&mut b, "var a = []; for (var i = 0; i < 30; i++) a.push({}); push(a)"), "DataCloneError:25:true");
        assert_eq!(b.navigation_stats().history_len, 1, "nothing committed");
        // the total over all entries of the document
        assert_eq!(js(&mut b, "push(rep('y', 400)) + push(rep('y', 400))"), "okok");
        let n = b.navigation_stats();
        assert_eq!(js(&mut b, "push(rep('y', 400))"), "DataCloneError:25:true");
        assert_eq!(b.navigation_stats().state_bytes, n.state_bytes);
        assert_eq!(js(&mut b, "try { history.replaceState(rep('z', 400), ''); 'ok' } catch (e) { e.name }"), "ok");
        // the page goes on after the error
        assert_eq!(js(&mut b, "history.pushState(1, ''); history.length"), "4");
    });
}

#[test]
fn the_default_caps_bound_one_state_and_the_total() {
    let l = NavLimits::default();
    assert_eq!((l.max_state_bytes, l.max_total_state_bytes, l.max_state_nodes), (16 << 20, 32 << 20, 1 << 18));
}
