//! Tests of the notifications of a service worker registration (`browser_sw.rs`, `browser_notification.rs`, step 5 of
//! `docs/internal/design-service-workers.md`): `showNotification`, `getNotifications`, `notificationclick` with an action,
//! `notificationclose`, `WindowClient.focus()` in a click, and the agent view (`Browser::notifications`, the event log). Chrome is the
//! reference. Time is the virtual clock.

use crate::browser_navigation_storage::Permission;
use crate::browser_session::*;
use crate::js_limits::EvalLimits;
use crate::resources::test_server::{Reply, Server, serve};
use std::time::Duration;

fn big<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(64 << 20).spawn(f).unwrap().join().unwrap()
}

const LOG: &str = "function log(x){localStorage.setItem('log',(localStorage.getItem('log')||'')+x+',')}";

fn server(page: &str, sw: &'static str) -> Server {
    let page = format!("<script>{LOG}{page}</script>");
    serve(move |path| match path {
        "/" => Reply::new(200, Some("text/html"), page.clone()),
        "/sw.js" => Reply::js(sw),
        _ => Reply::new(404, Some("text/plain"), "no"),
    })
}

/// A session on `srv`, with the notification grant when `grant`, after its first run.
fn session(srv: &Server, grant: bool, quiet: Duration) -> (Session, SessionResult) {
    let mut c = SessionConfig::new(&srv.url("/"));
    c.virtual_clock = true;
    c.eval = EvalLimits { max_steps: 5_000_000, ..Default::default() };
    c.page.quiet = quiet;
    let mut s = Session::new(c);
    if grant {
        s.storage().set_permission(&crate::sw_hub::origin_of(&srv.url("/")), "notifications", Some(Permission::Granted));
    }
    let r = s.run();
    (s, r)
}

fn log(s: &Session, srv: &Server) -> Option<String> {
    s.storage().get_item(false, &srv.url(""), "log")
}

/// The event log as `kind:id:title` items, drained.
fn events(s: &mut Session) -> String {
    s.browser().unwrap().take_notification_events().iter().map(|e| format!("{}:{}:{}", e.kind, e.id, e.title)).collect::<Vec<_>>().join(",")
}

/// The page registers `/sw.js` and runs `then` with `r`, the registration, when it is ready.
fn when_ready(then: &str) -> String {
    format!("navigator.serviceWorker.register('/sw.js'); navigator.serviceWorker.ready.then(function(r){{ {then} }});")
}

const FAIL: &str = ".then(function(){ log('ok') }, function(e){ log(e.name + ': ' + e.message) })";

#[test]
fn show_notification_needs_the_grant_and_an_active_worker() {
    big(|| {
        let page = format!("navigator.serviceWorker.register('/sw.js').then(function(r){{ return r.showNotification('a') }}){FAIL}; navigator.serviceWorker.ready.then(function(r){{ return r.showNotification('b') }}){FAIL};");
        let srv = server(&page, "");
        let (s, r) = session(&srv, false, Duration::from_millis(100));
        assert_eq!(
            log(&s, &srv).as_deref(),
            Some("TypeError: Failed to execute 'showNotification' on 'ServiceWorkerRegistration': No active registration available on the ServiceWorkerRegistration.,TypeError: Failed to execute 'showNotification' on 'ServiceWorkerRegistration': No notification permission has been granted for this origin.,"),
            "{r:?}"
        );
    });
}

#[test]
fn a_shown_notification_is_read_back_and_the_agent_sees_it() {
    big(|| {
        let srv = server(
            &when_ready("r.showNotification('Alice', {body: 'hi', tag: 't', data: {n: 1}, actions: [{action: 'a', title: 'A'}, {action: 'b', title: 'B'}, {action: 'c', title: 'C'}]}).then(function(v){ return r.getNotifications().then(function(l){ var n = l[0]; log([v, l.length, n instanceof Notification, n.title, n.body, n.tag, n.data.n, n.actions.length, n.actions[1].action, n.actions[1].title, Object.isFrozen(n.actions)].join('|')) }) })"),
            "",
        );
        let (mut s, r) = session(&srv, true, Duration::from_millis(100));
        assert_eq!(log(&s, &srv).as_deref(), Some("|1|true|Alice|hi|t|1|2|b|B|true,"), "{r:?}");
        let list = s.browser().unwrap().notifications();
        assert_eq!(list.len(), 1);
        assert_eq!((list[0].title.as_str(), list[0].actions.len(), list[0].actions[0].action.as_str()), ("Alice", 2, "a"));
        assert_eq!(events(&mut s), format!("shown:{}:Alice", list[0].id));
    });
}

#[test]
fn a_tag_replaces_and_get_notifications_filters_and_close_takes_it_out() {
    big(|| {
        let srv = server(
            &when_ready("r.showNotification('a', {tag: 'x'}).then(function(){ return r.showNotification('b', {tag: 'x'}) }).then(function(){ return r.showNotification('c', {tag: 'y'}) }).then(function(){ return r.getNotifications({tag: 'x'}) }).then(function(l){ log(l.map(function(n){ return n.title }).join('+')); l[0].close(); return r.getNotifications() }).then(function(l){ log(l.map(function(n){ return n.title }).join('+')) })"),
            "",
        );
        let (mut s, r) = session(&srv, true, Duration::from_millis(100));
        assert_eq!(log(&s, &srv).as_deref(), Some("b,c,"), "{r:?}");
        assert_eq!(events(&mut s), "shown:1:a,replaced:1:a,shown:2:b,shown:3:c,closed:2:b");
        assert_eq!(s.browser().unwrap().notifications().iter().map(|n| n.title.as_str()).collect::<Vec<_>>(), ["c"]);
    });
}

#[test]
fn a_click_on_an_action_starts_the_worker_and_it_focuses_the_page() {
    big(|| {
        // The worker stops after 30 s with no event, so the click starts it again.
        let srv = server(
            &when_ready("navigator.serviceWorker.onmessage = function(e){ log(e.data + '|' + document.visibilityState) }; r.showNotification('Alice', {data: {n: 7}, actions: [{action: 'reply', title: 'Reply'}]});"),
            "self.onnotificationclick = function(e){ var m = [e.action, e.notification.title, e.notification.data.n, e instanceof NotificationEvent, e.isTrusted].join('|'); e.waitUntil(clients.matchAll({includeUncontrolled: true}).then(function(l){ return l[0].focus() }).then(function(c){ c.postMessage(m + '|' + c.focused) })) };",
        );
        let (mut s, r) = session(&srv, true, Duration::from_secs(40));
        assert_eq!(log(&s, &srv), None, "{r:?}");
        let b = s.browser().unwrap();
        b.set_background(true).unwrap();
        let id = b.notifications()[0].id;
        assert_eq!(b.click_notification_action(id, "nope").unwrap(), None);
        assert_eq!(b.click_notification_action(id, "reply").unwrap(), Some(false));
        let r = s.resume(&[]);
        assert_eq!(log(&s, &srv).as_deref(), Some("reply|Alice|7|true|true|true|visible,"), "{r:?}");
        assert_eq!(events(&mut s), format!("shown:{id}:Alice,clicked:{id}:Alice"));
        assert_eq!(s.browser().unwrap().notifications().len(), 1, "a click does not close it");
    });
}

#[test]
fn focus_outside_a_click_is_refused_and_a_user_close_fires_notificationclose() {
    big(|| {
        let srv = server(
            &when_ready("navigator.serviceWorker.onmessage = function(e){ log(e.data) }; r.showNotification('Bob', {tag: 'z'}); r.active.postMessage('try');"),
            "self.onmessage = function(e){ e.waitUntil(e.source.focus().catch(function(x){ e.source.postMessage(x.name) })) }; self.onnotificationclose = function(e){ e.waitUntil(clients.matchAll({includeUncontrolled: true}).then(function(l){ l[0].postMessage('closed:' + e.notification.title + ':' + e.notification.tag) })) };",
        );
        let (mut s, r) = session(&srv, true, Duration::from_millis(100));
        assert_eq!(log(&s, &srv).as_deref(), Some("InvalidAccessError,"), "{r:?}");
        let b = s.browser().unwrap();
        let id = b.notifications()[0].id;
        assert!(b.close_notification(id).unwrap());
        assert!(!b.close_notification(id).unwrap());
        let r = s.resume(&[]);
        assert_eq!(log(&s, &srv).as_deref(), Some("InvalidAccessError,closed:Bob:z,"), "{r:?}");
        assert!(s.browser().unwrap().notifications().is_empty());
    });
}

#[test]
fn a_service_worker_cannot_construct_a_notification_and_shows_one_itself() {
    big(|| {
        let srv = server(
            &when_ready("navigator.serviceWorker.onmessage = function(e){ log(e.data) }; r.active.postMessage('go');"),
            "self.onmessage = function(e){ var m; try { new Notification('x') } catch (x) { m = x.name + ': ' + x.message } e.waitUntil(registration.showNotification('from sw', {body: 'b'}).then(function(){ return registration.getNotifications() }).then(function(l){ e.source.postMessage(m + '|' + l[0].title) })) };",
        );
        let (mut s, r) = session(&srv, true, Duration::from_millis(100));
        assert_eq!(log(&s, &srv).as_deref(), Some("TypeError: Failed to construct 'Notification': Illegal constructor. Use ServiceWorkerRegistration.showNotification() instead.|from sw,"), "{r:?}");
        assert_eq!(s.browser().unwrap().notifications()[0].title, "from sw");
    });
}

#[test]
fn page_and_service_worker_notifications_share_the_ids() {
    big(|| {
        let srv = server(&when_ready("new Notification('page'); setTimeout(function(){ r.showNotification('sw') }, 10);"), "");
        let (mut s, r) = session(&srv, true, Duration::from_millis(100));
        let list: Vec<(u64, String)> = s.browser().unwrap().notifications().into_iter().map(|n| (n.id, n.title)).collect();
        assert_eq!(list, [(1, "page".to_string()), (2, "sw".to_string())], "{r:?}");
    });
}
