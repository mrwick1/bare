//! A page that closes hears `visibilitychange` (the document is hidden) and then `pagehide`, as in Chrome when a tab closes, and the
//! requests that its handlers make go out before it is gone (issue #471). Web apps send their pending changes from these handlers.

mod common;

use bare_drive::run::{Options, Redactor, run};
use bare_drive::steps::parse;
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

static GOT: Mutex<Vec<String>> = Mutex::new(Vec::new());

const BEACONS: &str = r#"<!doctype html><title>t</title><script>
var n = 0
function send(e) { navigator.sendBeacon('/beacon?s=1&n=' + (++n) + '&e=' + e.type + '&v=' + document.visibilityState + '&h=' + document.hidden, 'x') }
document.addEventListener('visibilitychange', send)
addEventListener('pagehide', function (e) { send(e); fetch('/keep?s=1&p=' + e.persisted, { method: 'POST', body: 'y', keepalive: true }) })
</script>"#;

const HUNG: &str = r#"<!doctype html><title>t</title><script>
addEventListener('pagehide', function () { fetch('/hold?s=2', { method: 'POST', body: 'y', keepalive: true }) })
</script>"#;

fn route(_: &str, target: &str, _: &str, _: Option<&str>) -> (u16, &'static str, Vec<u8>) {
    match target {
        "/beacons" => return (200, "text/html", BEACONS.as_bytes().to_vec()),
        "/hung" => return (200, "text/html", HUNG.as_bytes().to_vec()),
        _ => {}
    }
    GOT.lock().unwrap().push(target.to_string());
    if target.starts_with("/hold") {
        std::thread::sleep(Duration::from_secs(20));
    }
    (200, "text/plain", b"ok".to_vec())
}

/// Open `path`, then close the page. Gives the report and how long the whole run took.
fn open_and_close(path: &str) -> (bool, Duration) {
    let origin = common::serve_route(route);
    let src = format!("open {origin}{path}\nexpect js \"document.readyState == 'complete'\"\n");
    std::thread::Builder::new()
        .stack_size(64 << 20)
        .spawn(move || {
            let steps = parse(&src, &HashMap::new()).unwrap();
            let opt = Options { size: (800, 600), timeout: Duration::from_secs(5), out: None, window: false, progress: false };
            let t = Instant::now();
            let rep = run(&steps, &opt, &mut Redactor::default());
            (rep.ok, t.elapsed())
        })
        .unwrap()
        .join()
        .unwrap()
}

fn got(tag: &str) -> Vec<String> {
    GOT.lock().unwrap().iter().filter(|t| t.contains(tag)).cloned().collect()
}

#[test]
fn a_closing_page_hears_visibilitychange_then_pagehide_and_its_requests_go_out() {
    let (ok, _) = open_and_close("/beacons");
    assert!(ok);
    // The requests go out side by side, so they can arrive in any order: `n` gives the order of the events.
    let mut got = got("s=1");
    got.sort();
    assert_eq!(
        got,
        ["/beacon?s=1&n=1&e=visibilitychange&v=hidden&h=true", "/beacon?s=1&n=2&e=pagehide&v=hidden&h=true", "/keep?s=1&p=false"],
        "the requests that the server got"
    );
}

#[test]
fn a_request_that_hangs_in_pagehide_holds_the_close_at_most_two_seconds() {
    let (ok, took) = open_and_close("/hung");
    assert!(ok);
    assert_eq!(got("s=2"), ["/hold?s=2"]);
    assert!(took < Duration::from_secs(4), "the close took {took:?}");
}
