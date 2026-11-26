//! Tests of `Notification.maxActions` and the `actions` option (`browser_notification.rs`). Chrome is the reference: `maxActions`
//! is 2, and a page notification (`new Notification`) with actions throws a `TypeError`, because only a service worker
//! notification can have actions.

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
    let url = "https://example.test/page.html";
    let mut b = Browser::new(tree::parse("<body></body>"), BrowserLimits::default(), EvalLimits { max_steps: 5_000_000, ..Default::default() }, HeapLimits::default()).expect("browser");
    b.use_virtual_clock();
    b.configure_fetch(FetchConfig { base_url: Some(url.into()), client: Client::new(), limits: FetchLimits::default() }).expect("fetch");
    b.set_document_url(url).expect("url");
    b.set_permission("https://example.test", "notifications", Some(Permission::Granted)).expect("grant");
    b
}

fn js(b: &mut Browser, src: &str) -> String {
    match b.run_script(src) {
        Ok(v) => b.it.display(v),
        Err(Error::Throw(v)) if crate::eng_new::is_object(v) => format!("throw:{}:{}", b.it.get_str(v, "name"), b.it.get_str(v, "message")),
        Err(e) => format!("other:{e:?}"),
    }
}

#[test]
fn max_actions_is_two_as_in_chrome() {
    big(|| {
        let mut b = browser();
        assert_eq!(js(&mut b, "Notification.maxActions"), "2");
    });
}

#[test]
fn a_page_notification_with_actions_throws_a_type_error() {
    big(|| {
        let mut b = browser();
        assert_eq!(
            js(&mut b, "new Notification('t', { actions: [{ action: 'a', title: 'A' }] })"),
            "throw:TypeError:Failed to construct 'Notification': Actions are only supported for persistent notifications shown using ServiceWorkerRegistration.showNotification()."
        );
        // An empty list is allowed, and the getter gives a frozen empty array.
        assert_eq!(js(&mut b, "var n = new Notification('t', { actions: [] }); [Array.isArray(n.actions), n.actions.length, Object.isFrozen(n.actions)].join()"), "true,0,true");
        assert_eq!(js(&mut b, "typeof Object.getOwnPropertyDescriptor(Notification.prototype, 'actions').get"), "function");
    });
}

#[test]
fn bad_actions_throw_before_the_persistent_check() {
    big(|| {
        let mut b = browser();
        assert_eq!(
            js(&mut b, "new Notification('t', { actions: [{ title: 'A' }] })"),
            "throw:TypeError:Failed to construct 'Notification': Failed to read the 'actions' property from 'NotificationOptions': Failed to read the 'action' property from 'NotificationAction': Required member is undefined."
        );
        assert_eq!(
            js(&mut b, "new Notification('t', { actions: [{ action: 'a' }] })"),
            "throw:TypeError:Failed to construct 'Notification': Failed to read the 'actions' property from 'NotificationOptions': Failed to read the 'title' property from 'NotificationAction': Required member is undefined."
        );
        assert_eq!(
            js(&mut b, "new Notification('t', { actions: 5 })"),
            "throw:TypeError:Failed to construct 'Notification': Failed to read the 'actions' property from 'NotificationOptions': The provided value cannot be converted to a sequence."
        );
        // `actions` is read first: a bad `dir` after it does not hide its error.
        assert_eq!(js(&mut b, "var order = []; try { new Notification('t', { get actions() { order.push('actions'); return [] }, get badge() { order.push('badge'); return '' } }) } catch (e) {} order.join()"), "actions,badge");
    });
}
