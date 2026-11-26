//! Tests of a `MessagePort` in the transfer list of `postMessage` between a page and its dedicated worker. Chrome is the reference: the
//! port arrives in `event.ports` and the pair works both ways. Each page loads over loopback HTTP from a throwaway server of this process.

use crate::browser_session::*;
use crate::resources::test_server::{Reply, Server, serve};
use crate::js_limits::EvalLimits;
use std::time::Duration;

fn big<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(64 << 20).spawn(f).unwrap().join().unwrap()
}

fn server(routes: Vec<(&'static str, &'static str)>) -> Server {
    serve(move |path| match routes.iter().find(|r| r.0 == path) {
        Some((p, body)) if p.ends_with(".js") => Reply::new(200, Some("text/javascript"), body.to_string()),
        Some((_, body)) => Reply::new(200, Some("text/html; charset=utf-8"), body.to_string()),
        None => Reply::new(404, Some("text/plain"), "no"),
    })
}

/// Run the page `/` of `srv` and give its `log` item in `localStorage`.
fn run(srv: &Server) -> (Option<String>, SessionResult) {
    let mut c = SessionConfig::new(&srv.url("/"));
    c.virtual_clock = true;
    c.eval = EvalLimits { max_steps: 5_000_000, ..Default::default() };
    c.page.quiet = Duration::from_millis(100);
    let mut s = Session::new(c);
    let r = s.run();
    (s.storage().get_item(false, &srv.url(""), "log"), r)
}

const LOG: &str = "function log(x){localStorage.setItem('log',(localStorage.getItem('log')||'')+x+',')}";

fn page(script: &str) -> &'static str {
    Box::leak(format!("<script>{LOG}{script}</script>").into_boxed_str())
}

#[test]
fn a_port_from_the_page_reaches_the_worker_and_works_both_ways() {
    big(|| {
        let srv = server(vec![
            ("/", page("var w = new Worker('/w.js'); var mc = new MessageChannel(); mc.port1.onmessage = function(e){ log('page ' + e.data); if (e.data === 'hi') mc.port1.postMessage('ping') }; w.onerror = function(e){ log('error') }; w.postMessage({ op: 'init', port: mc.port2 }, [mc.port2]);")),
            ("/w.js", "onmessage = function(e){ var p = e.ports[0]; if (e.data.port !== p) throw 1; p.onmessage = function(m){ p.postMessage('got ' + m.data) }; p.postMessage('hi') };"),
        ]);
        let (log, r) = run(&srv);
        assert_eq!(log.as_deref(), Some("page hi,page got ping,"), "{r:?}");
    });
}

#[test]
fn a_port_from_the_worker_reaches_the_page_and_works_both_ways() {
    big(|| {
        let srv = server(vec![
            ("/", page("var w = new Worker('/w.js'); w.onmessage = function(e){ var p = e.ports[0]; log('ports ' + e.ports.length + ' ' + (e.data.port === p)); p.onmessage = function(m){ log('page ' + m.data) }; p.postMessage('ping') };")),
            ("/w.js", "var mc = new MessageChannel(); mc.port1.onmessage = function(m){ mc.port1.postMessage('got ' + m.data) }; postMessage({ port: mc.port2 }, [mc.port2]);"),
        ]);
        let (log, r) = run(&srv);
        assert_eq!(log.as_deref(), Some("ports 1 true,page got ping,"), "{r:?}");
    });
}

#[test]
fn a_port_with_the_transfer_options_object_reaches_the_worker() {
    big(|| {
        let srv = server(vec![
            ("/", page("var w = new Worker('/w.js'); var mc = new MessageChannel(); mc.port1.onmessage = function(e){ log(e.data) }; w.postMessage('init', { transfer: [mc.port2] });")),
            ("/w.js", "self.addEventListener('message', function(e){ e.ports[0].postMessage('n' + e.ports.length) });"),
        ]);
        let (log, r) = run(&srv);
        assert_eq!(log.as_deref(), Some("n1,"), "{r:?}");
    });
}

#[test]
fn a_transfer_list_built_from_the_to_string_tag_moves_the_port() {
    big(|| {
        // Library code often finds the transferables of a message by their tag. In Chrome a port says `[object MessagePort]`.
        let srv = server(vec![
            ("/", page("log([new MessageChannel().port1, new MessageChannel()].map(function(o){ return Object.prototype.toString.call(o) }).join(' ')); var w = new Worker('/w.js'); w.onmessage = function(e){ log(e.data); if (e.ports[0]) e.ports[0].postMessage('ping') };")),
            ("/w.js", "var mc = new MessageChannel(); mc.port1.onmessage = function(m){ postMessage('got ' + m.data) }; var msg = { port: mc.port2 }; var list = Object.keys(msg).map(function(k){ return msg[k] }).filter(function(v){ return Object.prototype.toString.call(v) === '[object MessagePort]' }); try { postMessage(msg, list) } catch (x) { postMessage(x.message) }"),
        ]);
        let (log, r) = run(&srv);
        assert_eq!(log.as_deref(), Some("[object MessagePort] [object MessageChannel],[object Object],got ping,"), "{r:?}");
    });
}

/// Run `page_js` in a cross-origin isolated page. Its dedicated worker `w` runs `worker_js` on a thread of its own. The page appends
/// to the global `out`. The run ends when `out` contains `end`, or after 5 seconds. Gives `out`.
fn isolated(page_js: &str, worker_js: &str) -> String {
    use crate::browser::{Browser, BrowserLimits};
    use crate::browser_loader::{PageConfig, PageLimits};
    use crate::js_limits::HeapLimits;
    use bare_net::http::Client;
    use std::time::Instant;
    let url = "https://example.test/";
    let src = format!("data:text/javascript,{}", worker_js.replace('%', "%25").replace('#', "%23"));
    let html = format!("<script>var out = ''; var w = new Worker({src:?}); {page_js}</script>");
    let mut browser = Browser::new(bare_html::tree::parse(&html), BrowserLimits::default(), EvalLimits::default(), HeapLimits::default()).unwrap();
    browser.set_document_url(url).unwrap();
    browser.attach_isolated_agent(Default::default()).unwrap();
    let client = Client::new();
    let limits = PageLimits { quiet: Duration::ZERO, ..Default::default() };
    let (send, receive) = std::sync::mpsc::sync_channel(1);
    let _wake = bare_net::parallel::on_end(move || { let _ = send.try_send(()); });
    browser.run_page(PageConfig { url: url.into(), client: client.fork(), limits: limits.clone(), fetch: None });
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if browser.run_script("out.indexOf('end') >= 0").unwrap().as_bool() == Some(true) { break; }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() { break; }
        let _ = receive.recv_timeout(remaining.min(Duration::from_millis(10)));
        browser.resume_page(client.fork(), limits.clone(), &[]);
    }
    let n = browser.run_script("out.length").unwrap().as_int().unwrap();
    let out = (0..n).map(|i| char::from_u32(browser.run_script(&format!("out.charCodeAt({i})")).unwrap().as_int().unwrap() as u32).unwrap()).collect();
    browser.run_script("w.terminate()").unwrap();
    out
}

#[test]
fn a_port_from_the_page_reaches_a_threaded_worker_and_works_both_ways() {
    big(|| {
        let out = isolated(
            "var mc = new MessageChannel(); mc.port1.onmessage = function(e){ out += e.data + ',end' }; w.postMessage({ p: mc.port2 }, [mc.port2]);",
            "onmessage = function(e){ e.ports[0].postMessage('hi ' + (e.data.p === e.ports[0])) };",
        );
        assert_eq!(out, "hi true,end");
    });
}

#[test]
fn a_port_from_a_threaded_worker_reaches_the_page_and_works_both_ways() {
    big(|| {
        let out = isolated(
            "w.onmessage = function(e){ out += 'n' + e.ports.length + ','; e.ports[0].onmessage = function(m){ out += m.data + ',end' }; e.ports[0].postMessage('ping') };",
            "var mc = new MessageChannel(); mc.port1.onmessage = function(m){ mc.port1.postMessage('got ' + m.data) }; postMessage({ p: mc.port2 }, [mc.port2]);",
        );
        assert_eq!(out, "n1,got ping,end");
    });
}
