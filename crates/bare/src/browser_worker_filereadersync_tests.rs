//! Tests of `FileReaderSync` (File API, worker only; `shims/filereadersync.js`): the four `read*` methods in a dedicated worker and in a
//! threaded worker, and no `FileReaderSync` on a window. WhatsApp Web reads a video or an audio file with it in a worker before it sends it.

use crate::browser::{Browser, BrowserLimits};
use crate::browser_loader::{PageConfig, PageLimits};
use crate::browser_session::*;
use crate::js_limits::{EvalLimits, HeapLimits};
use crate::resources::test_server::{Reply, Server, serve};
use bare_net::http::Client;
use std::time::{Duration, Instant};

/// The worker script. It posts the results of the reads, joined with `|`.
const WORKER: &str = "var r = new FileReaderSync(), out = [typeof FileReaderSync, Object.prototype.toString.call(r)];
    out.push(r.readAsText(new Blob(['h\\u00e9llo'])));
    out.push(new Uint8Array(r.readAsArrayBuffer(new Blob(['abc']))).join('.'));
    out.push(r.readAsDataURL(new Blob(['hi'], { type: 'text/plain' })));
    out.push(r.readAsBinaryString(new Blob(['h\\u00e9'])).length);
    out.push(r.readAsText(new Blob([new Uint8Array([0xc3, 0xa9])]), 'utf-8'));
    try { r.readAsText('x'); out.push('no throw') } catch (e) { out.push(e.name) }
    postMessage(out.join('|'))";

const EXPECTED: &str = "function|[object FileReaderSync]|h\u{e9}llo|97.98.99|data:text/plain;base64,aGk=|3|\u{e9}|TypeError";

fn big<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(64 << 20).spawn(f).unwrap().join().unwrap()
}

#[test]
fn a_dedicated_worker_reads_blobs_and_a_window_has_no_file_reader_sync() {
    let got = big(|| {
        let srv: Server = serve(move |path| match path {
            "/" => Reply::new(200, Some("text/html; charset=utf-8"), "<script>var w = typeof FileReaderSync; new Worker('/w.js').onmessage = function(e){ localStorage.setItem('log', w + '#' + e.data) }</script>".to_string()),
            "/w.js" => Reply::new(200, Some("text/javascript"), WORKER.to_string()),
            _ => Reply::new(404, Some("text/plain"), "no".to_string()),
        });
        let mut c = SessionConfig::new(&srv.url("/"));
        c.virtual_clock = true;
        c.eval = EvalLimits { max_steps: 5_000_000, ..Default::default() };
        c.page.quiet = Duration::from_millis(100);
        let mut s = Session::new(c);
        s.run();
        s.storage().get_item(false, &srv.url(""), "log")
    });
    assert_eq!(got.as_deref(), Some(format!("undefined#{EXPECTED}").as_str()));
}

#[test]
fn a_threaded_worker_reads_blobs() {
    big(|| {
        let url = "https://example.test/";
        let html = format!("<script>var got = null; new Worker('data:text/javascript,' + encodeURIComponent({:?})).onmessage = function (e) {{ got = e.data }}</script>", WORKER);
        let mut browser = Browser::new(bare_html::tree::parse(&html), BrowserLimits::default(), EvalLimits::default(), HeapLimits::default()).unwrap();
        browser.set_document_url(url).unwrap();
        browser.attach_isolated_agent(Default::default()).unwrap();
        let client = Client::new();
        let limits = PageLimits { quiet: Duration::ZERO, ..Default::default() };
        let (send, receive) = std::sync::mpsc::sync_channel(1);
        let _wake = bare_net::parallel::on_end(move || { let _ = send.try_send(()); });
        browser.run_page(PageConfig { url: url.into(), client: client.fork(), limits: limits.clone(), fetch: None });
        let deadline = Instant::now() + Duration::from_secs(5);
        let check = format!("got === {:?}", EXPECTED);
        loop {
            if browser.run_script("got !== null").unwrap().as_bool() == Some(true) { break; }
            let remaining = deadline.saturating_duration_since(Instant::now());
            assert!(!remaining.is_zero(), "the worker did not answer");
            let _ = receive.recv_timeout(remaining.min(Duration::from_millis(10)));
            browser.resume_page(client.fork(), limits.clone(), &[]);
        }
        assert_eq!(browser.run_script(&check).unwrap().as_bool(), Some(true), "the reads differ from {EXPECTED}");
    });
}
