//! Tests of the page basics (`browser_basics.rs`): the document, window and navigator names that live pages read, `window.postMessage`
//! and `navigator.sendBeacon`. Each test runs a real script in a real realm. Expectations come from the HTML Standard and from Chrome 155 on Linux.

use crate::browser::*;
use crate::browser_fetch::{FetchConfig, FetchLimits};
use crate::browser_fetch_tests::{at, big, js, mk, ok, serve};
use bare_html::tree;
use bare_js::eval::EvalLimits;
use bare_js::heap::HeapLimits;
use bare_net::http::Client;

fn page(html: &str, base: &str) -> Browser {
    let mut b = Browser::new(tree::parse(html), BrowserLimits::default(), EvalLimits { max_steps: 20_000_000, ..Default::default() }, HeapLimits::default()).expect("browser");
    b.configure_fetch(FetchConfig { base_url: Some(base.into()), client: Client::new(), limits: FetchLimits::default() }).unwrap();
    b
}

#[test]
fn the_document_has_the_basics_that_pages_read() {
    big(|| {
        let mut b = page("<!DOCTYPE html><html><head></head><body></body></html>", "https://example.com/");
        assert_eq!(js(&mut b, "[typeof document.referrer, document.referrer.length].join()"), "string,0");
        assert_eq!(js(&mut b, "document.referrer.search('x')"), "-1");
        assert_eq!(js(&mut b, "[document.visibilityState, document.hidden, document.designMode].join()"), "visible,false,off");
        assert_eq!(js(&mut b, "[document.characterSet, document.charset, document.inputEncoding, document.contentType].join()"), "UTF-8,UTF-8,UTF-8,text/html");
        assert_eq!(js(&mut b, "document.compatMode"), "CSS1Compat");
        assert_eq!(js(&mut b, "JSON.stringify(document.dir)"), "\"\"");
        assert_eq!(js(&mut b, "/^\\d\\d\\/\\d\\d\\/\\d{4} \\d\\d:\\d\\d:\\d\\d$/.test(document.lastModified)"), "true");
    });
}

#[test]
fn compat_mode_dir_and_charset_follow_the_document() {
    big(|| {
        let mut b = page("<html dir=RTL><head><meta charset=iso-8859-1></head><body></body></html>", "https://example.com/");
        assert_eq!(js(&mut b, "document.compatMode"), "BackCompat");
        assert_eq!(js(&mut b, "document.dir"), "rtl");
        assert_eq!(js(&mut b, "document.characterSet + ',' + document.inputEncoding"), "windows-1252,windows-1252");
        assert_eq!(js(&mut b, "document.documentElement.setAttribute('dir', 'bogus'); document.dir"), "");
    });
}

#[test]
fn the_window_has_the_basics_that_pages_read() {
    big(|| {
        let mut b = page("<body></body>", "https://example.com/");
        assert_eq!(js(&mut b, "[devicePixelRatio, screenX, screenY, crossOriginIsolated, closed, opener, frameElement].join()"), "1,0,0,false,false,,");
        assert_eq!(js(&mut b, "[window.opener === null, window.frameElement === null].join()"), "true,true");
        assert_eq!(js(&mut b, "[outerWidth === innerWidth, outerHeight === innerHeight, outerWidth > 0].join()"), "true,true,true");
        assert_eq!(js(&mut b, "[alert('x'), confirm('x'), prompt('x'), print(), focus(), blur(), open('about:blank')].map(String).join()"), "undefined,false,null,undefined,undefined,undefined,null");
        assert_eq!(js(&mut b, "[alert, confirm, prompt, print, focus, blur, open, postMessage].map(function (f) { return typeof f }).join()"), "function,function,function,function,function,function,function,function");
        assert_eq!(js(&mut b, "window.devicePixelRatio === self.devicePixelRatio && window.isSecureContext"), "true");
    });
}

#[test]
fn is_secure_context_is_true_for_https_and_loopback_only() {
    big(|| {
        for (base, want) in [("https://example.com/", "true"), ("http://127.0.0.1:8080/", "true"), ("http://localhost/", "true"), ("http://example.com/", "false")] {
            let mut b = page("<body></body>", base);
            assert_eq!(js(&mut b, "isSecureContext"), want, "{base}");
        }
    });
}

#[test]
fn the_navigator_has_the_basics_that_pages_read() {
    big(|| {
        let mut b = page("<body></body>", "https://example.com/");
        assert_eq!(js(&mut b, "[navigator.vendor, navigator.maxTouchPoints, navigator.webdriver, navigator.pdfViewerEnabled, navigator.javaEnabled()].join()"), "Google Inc.,0,false,true,false");
        assert_eq!(js(&mut b, "[navigator.appName, navigator.product, navigator.productSub, JSON.stringify(navigator.vendorSub), navigator.doNotTrack].join()"), "Netscape,Gecko,20030107,\"\",");
        assert_eq!(js(&mut b, "navigator.doNotTrack === null"), "true");
        assert_eq!(js(&mut b, "navigator.appVersion === navigator.userAgent.slice('Mozilla/'.length)"), "true");
        assert_eq!(js(&mut b, "[navigator.plugins.length, navigator.mimeTypes.length, navigator.plugins === navigator.plugins, navigator.plugins[0].name, navigator.mimeTypes[0].type].join()"), "5,2,true,PDF Viewer,application/pdf");
        assert_eq!(js(&mut b, "typeof navigator.sendBeacon"), "function");
    });
}

fn events(b: &mut Browser) {
    js(b, "var out = []; addEventListener('message', function (e) { out.push([JSON.stringify(e.data), e.origin, e.source === window, e.lastEventId, e.ports.length].join('/')) });");
}

#[test]
fn post_message_queues_a_message_event_for_a_later_task() {
    big(|| {
        let mut b = page("<body></body>", "http://127.0.0.1:9/app");
        events(&mut b);
        assert_eq!(js(&mut b, "String(postMessage({ a: [1, 'x'], b: null }, '*'))"), "undefined");
        assert_eq!(js(&mut b, "out.length"), "0");
        b.run_scheduled_tasks(10).unwrap();
        assert_eq!(js(&mut b, "out.join('|')"), "{\"a\":[1,\"x\"],\"b\":null}/http://127.0.0.1:9/true//0");
    });
}

#[test]
fn post_message_clones_the_message() {
    big(|| {
        let mut b = page("<body></body>", "http://127.0.0.1:9/app");
        js(&mut b, "var src = { n: { m: 1 } }, got; addEventListener('message', function (e) { got = e.data }); postMessage(src, '*'); src.n.m = 2;");
        b.run_scheduled_tasks(10).unwrap();
        assert_eq!(js(&mut b, "[got !== src, got.n.m].join()"), "true,1");
        assert_eq!(js(&mut b, "try { postMessage(function () {}, '*') } catch (e) { e.name }"), "DataCloneError");
    });
}

#[test]
fn post_message_checks_the_target_origin() {
    big(|| {
        let mut b = page("<body></body>", "http://127.0.0.1:9/app");
        events(&mut b);
        js(&mut b, "postMessage(1, '*'); postMessage(2, '/'); postMessage(3, 'http://127.0.0.1:9'); postMessage(4, 'http://127.0.0.1:9/other/path'); postMessage(5, 'http://example.com'); postMessage(6, 'https://127.0.0.1:9'); postMessage(7, 'http://127.0.0.1:10'); postMessage(8); postMessage(9, { targetOrigin: 'http://example.com' }); postMessage(10, { targetOrigin: '*' })");
        b.run_scheduled_tasks(20).unwrap();
        assert_eq!(js(&mut b, "out.map(function (s) { return s.split('/')[0] }).join()"), "1,2,3,4,8,10");
        assert_eq!(js(&mut b, "try { postMessage(1, 'not a url') } catch (e) { e.name }"), "SyntaxError");
        assert_eq!(js(&mut b, "try { postMessage() } catch (e) { e.name }"), "TypeError");
    });
}

#[test]
fn post_message_reaches_onmessage_and_keeps_order() {
    big(|| {
        let mut b = page("<body></body>", "https://example.com/");
        js(&mut b, "var seen = []; onmessage = function (e) { seen.push(e.data) }; postMessage('a', '*'); setTimeout(function () { seen.push('t') }, 0); postMessage('b', '*');");
        b.run_scheduled_tasks(10).unwrap();
        assert_eq!(js(&mut b, "seen.join()"), "a,t,b");
    });
}

#[test]
fn send_beacon_posts_the_data_and_returns_true() {
    big(|| {
        let srv = serve(|_| ok("{}"));
        let mut b = at(srv.port);
        assert_eq!(js(&mut b, "var r = [navigator.sendBeacon('/beacon', 'hello'), navigator.sendBeacon('/beacon2', new Blob(['xyz'], { type: 'text/x' })), navigator.sendBeacon('/beacon3')]; r.join()"), "true,true,true");
        b.drain().unwrap();
        while b.pending_fetches() > 0 {
            b.run_network_tasks(100).unwrap();
        }
        let recs = srv.recs();
        assert_eq!(recs.len(), 3);
        assert_eq!((recs[0].method.as_str(), recs[0].path.as_str(), recs[0].body.as_slice()), ("POST", "/beacon", &b"hello"[..]));
        assert_eq!(recs[0].header("content-type"), ["text/plain;charset=UTF-8"]);
        assert_eq!((recs[1].path.as_str(), recs[1].body.as_slice(), recs[1].header("content-type")), ("/beacon2", &b"xyz"[..], vec!["text/x".to_string()]));
        assert_eq!(js(&mut b, "typeof Promise.resolve()"), "object");
    });
}

#[test]
fn send_beacon_returns_false_over_the_keepalive_quota() {
    big(|| {
        let srv = serve(|_| ok("{}"));
        let mut b = at(srv.port);
        assert_eq!(js(&mut b, "navigator.sendBeacon('/a', 'x'.repeat(40000))"), "true");
        assert_eq!(js(&mut b, "navigator.sendBeacon('/b', 'x'.repeat(40000))"), "false");
        assert_eq!(js(&mut b, "navigator.sendBeacon('/c', 'x'.repeat(65537))"), "false");
        assert_eq!(js(&mut b, "navigator.sendBeacon('/d', 'x'.repeat(25536))"), "true");
        assert_eq!(js(&mut b, "navigator.sendBeacon('/e', 'y')"), "false");
        while b.pending_fetches() > 0 {
            b.run_network_tasks(100).unwrap();
        }
        assert_eq!(srv.recs().iter().map(|r| r.path.clone()).collect::<Vec<_>>(), ["/a", "/d"]);
    });
}

#[test]
fn send_beacon_throws_a_type_error_for_a_bad_url_and_needs_an_argument() {
    big(|| {
        let mut b = mk(Some("https://example.com/".into()), FetchLimits::default());
        assert_eq!(js(&mut b, "try { navigator.sendBeacon('ftp://example.com/x', 'a') } catch (e) { e.name }"), "TypeError");
        assert_eq!(js(&mut b, "try { navigator.sendBeacon() } catch (e) { e.name }"), "TypeError");
    });
}
