//! An eval that reloads the page and then awaits: the promise belongs to the old document. Before the fix the next look at it read a
//! handle of the old engine in the new one, and the call panicked.

mod common;

use bare_drive::run::{Options, Redactor, Session};
use bare_drive::steps::parse;
use std::collections::HashMap;
use std::time::Duration;

fn route(_: &str, _: &str, _: &str, _: Option<&str>) -> (u16, &'static str, Vec<u8>) {
    (200, "text/html", b"<!doctype html><title>page</title><p>hi</p>".to_vec())
}

#[test]
fn eval_that_reloads_before_its_promise_settles_reports_the_navigation() {
    let origin = common::serve_route(route);
    std::thread::Builder::new()
        .stack_size(64 << 20)
        .spawn(move || {
            let opt = Options { size: (800, 600), timeout: Duration::from_secs(10), out: None, window: false, progress: false };
            let mut red = Redactor::default();
            let mut s = Session::new(&opt);
            let vars = HashMap::from([("ORIGIN".to_string(), origin)]);
            for st in parse("open ${ORIGIN}/", &vars).unwrap() {
                assert!(s.step(&st, &opt, &mut red));
            }
            let e = s.eval("location.reload(); await new Promise(r => setTimeout(r, 300)); return document.title".into()).unwrap_err();
            assert!(e.contains("navigated"), "{e}");
            assert_eq!(s.eval("document.title".into()).unwrap(), "page");
        })
        .unwrap()
        .join()
        .unwrap();
}
