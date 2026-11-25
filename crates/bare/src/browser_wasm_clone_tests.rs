//! Tests of the structured clone of a `WebAssembly.Module` (`browser_wasm.rs`, `browser_web::clone_out`). WhatsApp compiles a module on
//! the page and posts it to its backend worker.

use crate::browser::{Browser, BrowserLimits};
use crate::browser_loader::{PageConfig, PageLimits};
use crate::js_limits::{EvalLimits, HeapLimits};
use bare_net::http::Client;
use crate::eng_new::Peek;
use std::time::{Duration, Instant};

/// `add(i32, i32) -> i32`, as a JS expression of its bytes.
const ADD: &str = "new Uint8Array([0,97,115,109,1,0,0,0,1,7,1,96,2,127,127,1,127,3,2,1,0,7,7,1,3,97,100,100,0,0,10,9,1,7,0,32,0,32,1,106,11])";

#[test]
fn a_module_clone_instantiates() {
    let got = crate::browser_wasm_tests::go(&format!("var m = new WebAssembly.Module({ADD}); var c = structuredClone(m);
        out.push(c instanceof WebAssembly.Module, c !== m, new WebAssembly.Instance(c).exports.add(2, 3), WebAssembly.Module.exports(c)[0].name);"));
    assert_eq!(got, "true|true|5|add");
}

#[test]
fn a_module_posted_to_an_isolated_worker_instantiates_there() {
    std::thread::Builder::new().stack_size(32 << 20).spawn(|| {
        let url = "https://example.test/";
        let html = format!(r#"
            <script>
            var result = '';
            var worker = new Worker("data:text/javascript," + encodeURIComponent(`
                onmessage = e => postMessage(String(e.data.m instanceof WebAssembly.Module) + ' ' + new WebAssembly.Instance(e.data.m).exports.add(20, 22));
            `));
            worker.onmessage = e => {{ result = e.data; }};
            worker.onerror = e => {{ result = 'error ' + e.message; }};
            worker.postMessage({{ m: new WebAssembly.Module({ADD}) }});
            </script>
        "#);
        let mut browser = Browser::new(bare_html::tree::parse(&html), BrowserLimits::default(), EvalLimits::default(), HeapLimits::default()).unwrap();
        browser.set_document_url(url).unwrap();
        browser.attach_isolated_agent(Default::default()).unwrap();
        let client = Client::new();
        let limits = PageLimits { quiet: Duration::ZERO, ..Default::default() };
        let (send, receive) = std::sync::mpsc::sync_channel(1);
        let _wake = bare_net::parallel::on_end(move || { let _ = send.try_send(()); });
        browser.run_page(PageConfig { url: url.into(), client: client.fork(), limits: limits.clone(), fetch: None });
        let deadline = Instant::now() + Duration::from_secs(5);
        let got = loop {
            let v = browser.run_script("result").unwrap();
            let s = browser.it.display(v);
            if !s.is_empty() { break s; }
            let remaining = deadline.saturating_duration_since(Instant::now());
            assert!(!remaining.is_zero(), "the worker did not answer");
            let _ = receive.recv_timeout(remaining.min(Duration::from_millis(10)));
            browser.resume_page(client.fork(), limits.clone(), &[]);
        };
        assert_eq!(got, "true 42");
    }).unwrap().join().unwrap();
}
