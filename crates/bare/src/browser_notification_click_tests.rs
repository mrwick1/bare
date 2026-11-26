//! Tests of a click and a close by the user on a notification (`Browser::click_notification`, `Browser::close_notification`). The
//! Notifications API standard and Chrome are the reference: a click fires a trusted, cancelable `click` and does not close it.

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

/// A page of https://example.test. `grant`: the origin has the notification permission.
fn browser(grant: bool) -> Browser {
    let url = "https://example.test/dir/page.html";
    let mut b = Browser::new(tree::parse("<body></body>"), BrowserLimits::default(), EvalLimits { max_steps: 5_000_000, ..Default::default() }, HeapLimits::default()).expect("browser");
    b.use_virtual_clock();
    b.configure_fetch(FetchConfig { base_url: Some(url.into()), client: Client::new(), limits: FetchLimits::default() }).expect("fetch");
    b.set_document_url(url).expect("url");
    if grant {
        b.set_permission("https://example.test", "notifications", Some(Permission::Granted)).expect("grant");
    }
    b
}

fn js(b: &mut Browser, src: &str) -> String {
    match b.run_script(src) {
        Ok(v) => b.it.display(v),
        Err(Error::Throw(v)) if crate::eng_new::is_object(v) => format!("throw:{}:{}", b.it.get_str(v, "name"), b.it.get_str(v, "message")),
        Err(e) => format!("other:{e:?}"),
    }
}

fn pump(b: &mut Browser) {
    b.advance_time(10.0, 1000).expect("advance");
}

/// The event log as `kind:id:title` items, drained.
fn events(b: &mut Browser) -> String {
    b.take_notification_events().iter().map(|e| format!("{}:{}:{}", e.kind, e.id, e.title)).collect::<Vec<_>>().join(",")
}

#[test]
fn a_click_fires_a_trusted_cancelable_click_and_keeps_the_notification() {
    big(|| {
        let mut b = browser(true);
        js(&mut b, "var log = []; var n = new Notification('Alice', { body: 'hi' }); n.onclick = function (e) { log.push(e.type + ':' + e.isTrusted + ':' + e.cancelable + ':' + (e.target instanceof Notification)); window.focus(); }; n.onclose = function () { log.push('close'); }; n = null;");
        pump(&mut b);
        b.collect(&[]).expect("collect");
        events(&mut b);
        assert_eq!(b.click_notification(1).expect("click"), Some(false));
        assert_eq!(js(&mut b, "log.join()"), "click:true:true:true");
        assert_eq!(b.notifications().len(), 1, "Chrome does not close a clicked page notification");
        assert_eq!(events(&mut b), "clicked:1:Alice");
        assert_eq!(b.click_notification(9).expect("click"), None);
    });
}

#[test]
fn a_cancelled_click_says_so() {
    big(|| {
        let mut b = browser(true);
        js(&mut b, "var n = new Notification('a'); n.addEventListener('click', function (e) { e.preventDefault(); });");
        pump(&mut b);
        assert_eq!(b.click_notification(1).expect("click"), Some(true));
    });
}

#[test]
fn a_close_by_the_user_fires_close_at_once_and_only_once() {
    big(|| {
        let mut b = browser(true);
        js(&mut b, "var log = []; var n = new Notification('a'); n.onclose = function (e) { log.push(e.type + ':' + e.isTrusted); };");
        pump(&mut b);
        events(&mut b);
        assert!(b.close_notification(1).expect("close"));
        assert_eq!(js(&mut b, "log.join()"), "close:true");
        assert!(b.notifications().is_empty());
        assert!(!b.close_notification(1).expect("close again"));
        assert_eq!(js(&mut b, "n.close(); log.join()"), "close:true");
        pump(&mut b);
        assert_eq!(js(&mut b, "log.join()"), "close:true");
        assert_eq!(events(&mut b), "closed:1:a");
    });
}

#[test]
fn the_count_of_new_events_does_not_drain_the_log() {
    big(|| {
        let mut b = browser(true);
        js(&mut b, "new Notification('a');");
        pump(&mut b);
        assert_eq!(b.notification_event_count(), 1);
        assert_eq!(b.notification_event_count(), 1);
        events(&mut b);
        assert_eq!(b.notification_event_count(), 0);
    });
}
