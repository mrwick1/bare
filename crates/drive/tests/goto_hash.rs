//! The address bar with the document URL and another fragment stays in the same document, as in Chrome (issue #468): the page
//! keeps its state and its requests, and gets `hashchange`. Another path loads a new document.

mod common;

use bare::page::Page;
use std::time::{Duration, Instant};

const INDEX: &str = r#"<!doctype html><title>t</title><script>
var kept = 'same document', changes = [];
addEventListener('hashchange', function (e) { changes.push(e.newURL.split('#')[1]) });
</script>"#;

fn route(_: &str, target: &str, _: &str, _: Option<&str>) -> (u16, &'static str, Vec<u8>) {
    let _ = target;
    (200, "text/html", INDEX.as_bytes().to_vec())
}

fn js(p: &mut Page, src: &str) -> String {
    let b = p.browser().unwrap();
    let v = b.run_script(src).unwrap();
    b.it.display(v)
}

fn tick_until(p: &mut Page, limit: Duration, mut done: impl FnMut(&mut Page) -> bool) -> bool {
    let t = Instant::now();
    while t.elapsed() < limit {
        if p.browser().is_some() && done(p) {
            return true;
        }
        let (ran, _) = p.tick();
        if !ran {
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    false
}

#[test]
fn goto_with_another_fragment_stays_in_the_document() {
    let origin = common::serve_route(route);
    std::thread::Builder::new()
        .stack_size(64 << 20)
        .spawn(move || {
            let mut p = Page::open(&format!("{origin}/app#inbox"), (800, 600)).unwrap();
            p.quiet = true;
            assert!(tick_until(&mut p, Duration::from_secs(5), |p| p.idle().is_ok()), "the page did not settle");
            js(&mut p, "kept = 'changed in this document'");
            p.navigate(Some(&format!("{origin}/app#search/x"))).unwrap();
            assert!(tick_until(&mut p, Duration::from_secs(5), |p| js(p, "changes.join()") == "search/x"), "no hashchange");
            assert_eq!(js(&mut p, "kept + '|' + location.hash + '|' + history.length"), "changed in this document|#search/x|2");
            // Another path is a new document.
            p.navigate(Some(&format!("{origin}/other#search/x"))).unwrap();
            assert!(tick_until(&mut p, Duration::from_secs(5), |p| js(p, "location.pathname") == "/other"), "the new document did not load");
            assert_eq!(js(&mut p, "kept"), "same document");
        })
        .unwrap()
        .join()
        .unwrap();
}
