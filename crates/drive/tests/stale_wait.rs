//! The `wait` phase of a network record (first of `phases_ms`) is the time that a request queued. A document that a page did not
//! start (`open` of a plain file after another page) counted it from the start of that earlier page: issue #388 showed 9.6 s for a
//! request that took milliseconds. The fixture server keeps connections alive and serves one request at a time for each of them.

mod common;

use bare_drive::run::{Options, Redactor, run};
use bare_drive::steps::parse;
use std::collections::HashMap;
use std::time::Duration;

fn route(_: &str, target: &str, _: &str, _: Option<&str>) -> (u16, &'static str, Vec<u8>) {
    match target {
        "/" => (200, "text/html", b"<!DOCTYPE html><title>t</title><p id=r>early</p><script>setTimeout(function () { document.getElementById('r').textContent = 'late' }, 700)</script>".to_vec()),
        _ => (200, "text/plain", b"plain notes".to_vec()),
    }
}

#[test]
fn open_of_a_plain_file_does_not_count_the_wait_from_an_earlier_page() {
    let origin = common::serve_route(route);
    let json = std::thread::Builder::new()
        .stack_size(64 << 20)
        .spawn(move || {
            let vars = HashMap::from([("ORIGIN".to_string(), origin)]);
            let steps = parse("open ${ORIGIN}/\nexpect css=\"#r\" text=\"late\"\nopen ${ORIGIN}/notes.txt\nexpect css=\"body\" text=\"plain notes\"", &vars).unwrap();
            let opt = Options { size: (800, 600), timeout: Duration::from_secs(5), out: None, window: false, progress: false };
            let mut red = Redactor::default();
            run(&steps, &opt, &mut red).json(&red)
        })
        .unwrap()
        .join()
        .unwrap();
    let record = json.split("\"network\":[").nth(1).unwrap_or(&json).split("\"url\":").find(|r| r.contains("/notes.txt\"")).unwrap_or_else(|| panic!("no record of /notes.txt: {json}"));
    let wait: f64 = record.split("\"phases_ms\":[").nth(1).unwrap_or_else(|| panic!("no phases: {json}")).split(',').next().unwrap().parse().unwrap();
    assert!(wait < 100.0, "wait phase {wait} ms: {record}");
}
