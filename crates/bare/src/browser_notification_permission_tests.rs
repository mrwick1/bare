//! Tests of the notification permission of an origin (`browser_notification.rs`): `Notification.permission`,
//! `Notification.requestPermission` and `navigator.permissions.query`. Chrome is the reference. With no decision the state is
//! "denied" (owner decision, 2026-11-26): only a grant through `Browser::set_permission` gives "granted".

use crate::browser::*;
use crate::browser_fetch::{FetchConfig, FetchLimits};
use crate::browser_navigation_storage::Permission;
use bare_net::http::Client;
use crate::eng_new::NewFault as Error;
use crate::eng_new::Peek;
use crate::js_limits::EvalLimits;
use crate::js_limits::HeapLimits;
use bare_html::tree;

const ORIGIN: &str = "https://example.test";

fn big<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(64 << 20).spawn(f).unwrap().join().unwrap()
}

fn browser(url: &str) -> Browser {
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

fn pump(b: &mut Browser) {
    b.advance_time(10.0, 1000).expect("advance");
}

/// The state of `Notification.permission`, of `requestPermission` (callback and promise) and of `query`.
fn states(b: &mut Browser) -> String {
    js(b, "var got = []; Notification.requestPermission(function (p) { got.push('cb:' + p); }).then(function (p) { got.push('promise:' + p); }); navigator.permissions.query({ name: 'notifications' }).then(function (s) { got.push('query:' + s.state); });");
    pump(b);
    js(b, "Notification.permission + ',' + got.join()")
}

#[test]
fn with_no_decision_every_view_says_denied() {
    big(|| {
        let mut b = browser("https://example.test/a.html");
        assert_eq!(states(&mut b), "denied,cb:denied,promise:denied,query:denied");
    });
}

#[test]
fn a_grant_of_the_origin_gives_granted_and_a_reset_gives_denied() {
    big(|| {
        let mut b = browser("https://example.test/a.html");
        b.set_permission(ORIGIN, "notifications", Some(Permission::Granted)).expect("grant");
        assert_eq!(states(&mut b), "granted,cb:granted,promise:granted,query:granted");
        b.set_permission(ORIGIN, "notifications", Some(Permission::Denied)).expect("deny");
        assert_eq!(states(&mut b), "denied,cb:denied,promise:denied,query:denied");
        b.set_permission(ORIGIN, "notifications", Some(Permission::Granted)).expect("grant");
        b.set_permission(ORIGIN, "notifications", None).expect("reset");
        assert_eq!(states(&mut b), "denied,cb:denied,promise:denied,query:denied");
    });
}

#[test]
fn a_grant_of_another_origin_or_an_insecure_page_stays_denied() {
    big(|| {
        let mut b = browser("https://example.test/a.html");
        b.set_permission("https://other.test", "notifications", Some(Permission::Granted)).expect("grant");
        assert_eq!(js(&mut b, "Notification.permission"), "denied");
        let mut b = browser("http://plain.test/a.html");
        b.set_permission("http://plain.test", "notifications", Some(Permission::Granted)).expect("grant");
        assert_eq!(js(&mut b, "Notification.permission"), "denied");
        let mut b = browser("http://localhost:8080/a.html");
        b.set_permission("http://localhost:8080", "notifications", Some(Permission::Granted)).expect("grant");
        assert_eq!(js(&mut b, "Notification.permission"), "granted");
    });
}

#[test]
fn the_grant_is_kept_by_the_storage_of_the_session() {
    big(|| {
        let mut b = browser("https://example.test/a.html");
        b.set_permission(ORIGIN, "notifications", Some(Permission::Granted)).expect("grant");
        let mut c = browser("https://example.test/b.html");
        c.attach_storage(b.storage().new_session());
        assert_eq!(js(&mut c, "Notification.permission"), "granted");
        assert_eq!(b.storage().permission(ORIGIN, "notifications"), Some(Permission::Granted));
    });
}

#[test]
fn permissions_and_permission_status_have_the_chrome_shape() {
    big(|| {
        let mut b = browser("https://example.test/a.html");
        assert_eq!(js(&mut b, "var p = navigator.permissions; (p === navigator.permissions) + ',' + Object.prototype.toString.call(p) + ',' + typeof p.query + ',' + p.query.length"), "true,[object Permissions],function,1");
        js(&mut b, "var st; p.query({ name: 'notifications' }).then(function (s) { st = s; });");
        pump(&mut b);
        assert_eq!(js(&mut b, "Object.prototype.toString.call(st) + ',' + st.name + ',' + st.state + ',' + (st instanceof EventTarget) + ',' + st.onchange"), "[object PermissionStatus],notifications,denied,true,null");
    });
}

#[test]
fn other_names_say_prompt_and_a_bad_descriptor_rejects_with_a_type_error() {
    big(|| {
        let mut b = browser("https://example.test/a.html");
        js(&mut b, "var out = []; function q(d) { return navigator.permissions.query(d).then(function (s) { out.push(s.name + ':' + s.state); }, function (e) { out.push(e.name); }); } q({ name: 'geolocation' }); q({ name: 'clipboard-read' }); q({ name: 'nope' }); q({}); q(1); q();");
        pump(&mut b);
        assert_eq!(js(&mut b, "out.join()"), "geolocation:prompt,clipboard-read:prompt,TypeError,TypeError,TypeError,TypeError");
    });
}

#[test]
fn a_change_of_the_decision_fires_change_at_the_live_status() {
    big(|| {
        let mut b = browser("https://example.test/a.html");
        js(&mut b, "var log = []; navigator.permissions.query({ name: 'notifications' }).then(function (s) { var me = s; s.onchange = function (e) { log.push('on:' + e.type + ':' + this.state + ':' + (e.target === me) + ':' + e.isTrusted); }; s.addEventListener('change', function () { log.push('listener'); }); });");
        pump(&mut b);
        b.collect(&[]).expect("collect");
        b.set_permission("https://other.test", "notifications", Some(Permission::Granted)).expect("other origin");
        b.set_permission(ORIGIN, "notifications", Some(Permission::Denied)).expect("same state");
        assert_eq!(js(&mut b, "log.join()"), "");
        b.set_permission(ORIGIN, "notifications", Some(Permission::Granted)).expect("grant");
        pump(&mut b);
        assert_eq!(js(&mut b, "log.join()"), "on:change:granted:true:true,listener");
        b.set_permission(ORIGIN, "notifications", None).expect("reset");
        pump(&mut b);
        assert_eq!(js(&mut b, "log.join()"), "on:change:granted:true:true,listener,on:change:denied:true:true,listener");
    });
}

