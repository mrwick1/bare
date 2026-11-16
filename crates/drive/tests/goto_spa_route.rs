//! A hash-routed single-page app (issue #564: Gmail inbox search). A `goto` to the address that the page already has fires no
//! `hashchange` (as in Chrome), so the app routes once: one results grid, and the search box holds the query. A scan repeats the same
//! window address, so before the fix the app routed twice.

mod common;

use bare_drive::run::{Options, Redactor, Session};
use bare_drive::steps::parse;
use std::collections::HashMap;
use std::time::Duration;

// The app renders a route after a request, as a mail client does, and replaces the old view.
const APP: &str = r#"<!doctype html><title>spa</title><input id=q><div id=view></div><script>
var routes = 0;
function render(q) {
  routes++;
  var old = document.querySelector('.grid');
  var g = document.createElement('div'); g.className = 'grid'; g.textContent = q;
  fetch('/data?q=' + encodeURIComponent(q)).then(function (r) { return r.text() }).then(function () {
    if (old) old.remove();
    document.getElementById('view').appendChild(g);
    document.getElementById('q').value = q;
  });
}
if (!location.hash) location.replace('#inbox');
function route() { var m = /#search\/(.*)/.exec(location.hash); if (m) render(decodeURIComponent(m[1])) }
addEventListener('hashchange', route);
route();
</script>"#;

fn route(_: &str, target: &str, _: &str, _: Option<&str>) -> (u16, &'static str, Vec<u8>) {
    if target.starts_with("/data") {
        return (200, "text/plain", b"ok".to_vec());
    }
    (200, "text/html", APP.as_bytes().to_vec())
}

#[test]
fn goto_with_another_fragment_ends_with_the_route_applied() {
    let origin = common::serve_route(route);
    std::thread::Builder::new()
        .stack_size(64 << 20)
        .spawn(move || {
            let opt = Options { size: (800, 600), timeout: Duration::from_secs(10), out: None, window: false, progress: false };
            let mut red = Redactor::default();
            let mut s = Session::new(&opt);
            s.ready_wait = true;
            let vars = HashMap::from([("ORIGIN".to_string(), origin)]);
            let probe = "document.querySelectorAll('.grid').length + '|' + document.getElementById('q').value + '|' + routes";
            // The window start is the same on every scan: the same address again is no fragment change, so the app must not route again.
            for i in 0..4 {
                let q = "same";
                let u = 0;
                let line = if i == 0 { format!("open ${{ORIGIN}}/u/{u}/#search/{q}") } else { format!("goto ${{ORIGIN}}/u/{u}/#search/{q}") };
                for st in parse(&line, &vars).unwrap() {
                    assert!(s.step(&st, &opt, &mut red), "{line}");
                }
                assert_eq!(s.eval(probe.into()).unwrap(), format!("1|{q}|1"), "after {line}");
            }
        })
        .unwrap()
        .join()
        .unwrap();
}
