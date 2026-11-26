//! Tests of a page in the background (`Browser::set_background`): `document.visibilityState`, `document.hidden`,
//! `document.hasFocus()`, the `blur`, `visibilitychange` and `focus` events, and the 1 s alignment of the timers of a hidden page.
//! Chrome is the reference. The event order is the one of the Page Lifecycle API: active, passive (`blur`), hidden
//! (`visibilitychange`), and back in reverse order.

use crate::browser::*;
use crate::browser_fetch::{FetchConfig, FetchLimits};
use crate::browser_navigation_storage::Permission;
use crate::eng_new::NewFault as Error;
use crate::eng_new::Peek;
use crate::js_limits::EvalLimits;
use crate::js_limits::HeapLimits;
use bare_html::tree;
use bare_net::http::Client;

fn big<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(64 << 20).spawn(f).unwrap().join().unwrap()
}

fn browser() -> Browser {
    let url = "https://example.test/a.html";
    let mut b = Browser::new(tree::parse("<body></body>"), BrowserLimits::default(), EvalLimits { max_steps: 5_000_000, ..Default::default() }, HeapLimits::default()).expect("browser");
    b.use_virtual_clock();
    b.configure_fetch(FetchConfig { base_url: Some(url.into()), client: Client::new(), limits: FetchLimits::default() }).expect("fetch");
    b.set_document_url(url).expect("url");
    b
}

fn js(b: &mut Browser, src: &str) -> String {
    match b.run_script(src) {
        Ok(v) => b.it.display(v),
        Err(Error::Throw(v)) if crate::eng_new::is_object(v) => format!("throw:{}:{}", b.it.get_str(v, "name"), b.it.get_str(v, "message")),
        Err(e) => format!("other:{e:?}"),
    }
}

const STATE: &str = "document.visibilityState + ':' + document.hidden + ':' + document.hasFocus()";

#[test]
fn background_fires_blur_then_visibilitychange_and_back_fires_visibilitychange_then_focus() {
    big(|| {
        let mut b = browser();
        js(&mut b, &format!("var log = []; function on(t, ty) {{ t.addEventListener(ty, function (e) {{ log.push(ty + ':' + e.isTrusted + ':' + (e.target === t) + ':' + {STATE}); }}); }} on(document, 'visibilitychange'); on(window, 'blur'); on(window, 'focus');"));
        assert_eq!(js(&mut b, STATE), "visible:false:true");
        b.set_background(true).expect("background");
        b.set_background(true).expect("again");
        assert_eq!(js(&mut b, "log.join()"), "blur:true:true:visible:false:false,visibilitychange:true:true:hidden:true:false");
        assert_eq!(js(&mut b, STATE), "hidden:true:false");
        js(&mut b, "log = [];");
        b.set_background(false).expect("foreground");
        b.set_background(false).expect("again");
        assert_eq!(js(&mut b, "log.join()"), "visibilitychange:true:true:visible:false:false,focus:true:true:visible:false:true");
        assert_eq!(js(&mut b, STATE), "visible:false:true");
    });
}

#[test]
fn a_closed_page_stays_hidden() {
    big(|| {
        let mut b = browser();
        b.set_background(true).expect("background");
        b.hide().expect("hide");
        b.set_background(false).expect("foreground");
        assert_eq!(js(&mut b, "document.visibilityState + ':' + document.hidden"), "hidden:true");
    });
}

#[test]
fn timers_of_a_hidden_page_align_to_the_next_whole_second() {
    big(|| {
        let mut b = browser();
        b.advance_to(1000.5, 1000).expect("start between two seconds");
        js(&mut b, "var log = []; function at(name) { return function () { log.push(name + '@' + performance.now()); }; } setTimeout(at('before'), 300);");
        b.set_background(true).expect("background");
        js(&mut b, "setTimeout(at('t1500'), 1500); setTimeout(at('t0'), 0);");
        b.advance_to(1999.0, 1000).expect("advance");
        assert_eq!(js(&mut b, "log.join()"), "");
        b.advance_to(2000.0, 1000).expect("advance");
        assert_eq!(js(&mut b, "log.join()"), "before@2000,t0@2000");
        b.advance_to(3000.0, 1000).expect("advance");
        assert_eq!(js(&mut b, "log.join()"), "before@2000,t0@2000,t1500@3000");
        b.set_background(false).expect("foreground");
        b.advance_to(3000.5, 1000).expect("advance");
        js(&mut b, "log = []; setTimeout(at('t100'), 100);");
        b.advance_to(3100.5, 1000).expect("advance");
        assert_eq!(js(&mut b, "log.join()"), "t100@3100.5");
    });
}

#[test]
fn an_interval_of_a_hidden_page_runs_once_a_second() {
    big(|| {
        let mut b = browser();
        b.advance_to(1000.0, 1000).expect("start at a whole second");
        b.set_background(true).expect("background");
        js(&mut b, "var log = []; setInterval(function () { log.push(performance.now()); }, 250);");
        b.advance_to(4000.0, 1000).expect("advance");
        assert_eq!(js(&mut b, "log.join()"), "2000,3000,4000");
    });
}

#[test]
fn a_message_of_a_hidden_page_is_not_aligned() {
    big(|| {
        let mut b = browser();
        b.advance_to(1000.5, 1000).expect("start");
        b.set_background(true).expect("background");
        js(&mut b, "var log = []; var c = new MessageChannel(); c.port1.onmessage = function (e) { log.push(e.data + '@' + performance.now()); }; c.port2.postMessage('m');");
        b.advance_time(10.0, 1000).expect("advance");
        assert_eq!(js(&mut b, "log.join()"), "m@1000.5");
    });
}

#[test]
fn a_click_on_a_notification_brings_the_page_back_unless_canceled() {
    big(|| {
        let mut b = browser();
        b.set_permission("https://example.test", "notifications", Some(Permission::Granted)).expect("grant");
        js(&mut b, "var a = new Notification('a'); var c = new Notification('c'); c.onclick = function (e) { e.preventDefault(); };");
        b.advance_time(10.0, 1000).expect("advance");
        b.set_background(true).expect("background");
        assert_eq!(b.click_notification(2).expect("click"), Some(true), "canceled");
        assert_eq!(js(&mut b, STATE), "hidden:true:false");
        assert_eq!(b.click_notification(1).expect("click"), Some(false));
        assert_eq!(js(&mut b, STATE), "visible:false:true");
    });
}
