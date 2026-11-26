//! Tests of a shown notification (`browser_notification.rs`): the options, `show`, `close`, tag replacement, the list of live
//! notifications and the event log that an agent reads. Chrome and the Notifications API standard are the reference.

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
fn a_granted_notification_fires_show_after_the_current_task_and_is_listed() {
    big(|| {
        let mut b = browser(true);
        let r = js(&mut b, "var log = []; var n = new Notification('Alice', { body: 'hi', tag: 'chat', icon: '../i.png' }); n.onshow = function (e) { log.push(e.type + ':' + e.isTrusted + ':' + (e.target === n)); }; n.onerror = function () { log.push('error'); }; log.push('sync'); log.join()");
        assert_eq!(r, "sync");
        pump(&mut b);
        assert_eq!(js(&mut b, "log.join()"), "sync,show:true:true");
        let list = b.notifications();
        assert_eq!(list.len(), 1);
        let n = &list[0];
        assert_eq!((n.title.as_str(), n.body.as_str(), n.tag.as_str(), n.icon.as_str(), n.origin.as_str()), ("Alice", "hi", "chat", "https://example.test/i.png", "https://example.test"));
        assert_eq!(events(&mut b), "shown:1:Alice");
        assert_eq!(events(&mut b), "", "the log is drained");
    });
}

#[test]
fn a_denied_notification_fires_error_and_is_not_listed() {
    big(|| {
        let mut b = browser(false);
        js(&mut b, "var log = []; var n = new Notification('x'); n.onshow = function () { log.push('show'); }; n.onerror = function () { log.push('error'); };");
        pump(&mut b);
        assert_eq!(js(&mut b, "log.join()"), "error");
        assert!(b.notifications().is_empty());
        assert_eq!(events(&mut b), "");
    });
}

#[test]
fn the_options_are_read_back_as_in_chrome() {
    big(|| {
        let mut b = browser(true);
        let r = js(&mut b, "var n = new Notification('t'); [n.dir, n.lang, n.body, n.tag, n.icon, n.badge, n.data, n.silent, n.renotify, n.requireInteraction, typeof n.timestamp].map(String).join('|')");
        assert_eq!(r, "auto||||||null|null|false|false|number");
        let r = js(&mut b, "var o = { a: [1, 2] }; var m = new Notification('t', { dir: 'rtl', lang: 'en', badge: '/b.png', data: o, silent: true, renotify: true, tag: 'x', requireInteraction: true, timestamp: 5 }); [m.dir, m.lang, m.badge, m.data !== o, m.data === m.data, JSON.stringify(m.data), m.silent, m.renotify, m.requireInteraction, m.timestamp].join('|')");
        assert_eq!(r, "rtl|en|https://example.test/b.png|true|true|{\"a\":[1,2]}|true|true|true|5");
    });
}

#[test]
fn bad_options_throw_a_type_error() {
    big(|| {
        let mut b = browser(true);
        assert_eq!(js(&mut b, "try { new Notification('t', { renotify: true }); } catch (e) { e.name; }"), "TypeError");
        assert_eq!(js(&mut b, "try { new Notification('t', { silent: true, vibrate: [100] }); } catch (e) { e.name; }"), "TypeError");
        assert_eq!(js(&mut b, "try { new Notification('t', { dir: 'up' }); } catch (e) { e.name; }"), "TypeError");
        assert_eq!(js(&mut b, "try { new Notification('t', { data: function () {} }); } catch (e) { e.name; }"), "DataCloneError");
        pump(&mut b);
        assert!(b.notifications().is_empty());
    });
}

#[test]
fn close_removes_the_notification_and_fires_close_once() {
    big(|| {
        let mut b = browser(true);
        js(&mut b, "var log = []; var n = new Notification('a'); n.onclose = function (e) { log.push(e.type + ':' + e.isTrusted); };");
        pump(&mut b);
        assert_eq!(js(&mut b, "n.close(); n.close(); log.join()"), "");
        assert!(b.notifications().is_empty());
        pump(&mut b);
        assert_eq!(js(&mut b, "log.join()"), "close:true");
        pump(&mut b);
        assert_eq!(js(&mut b, "log.join()"), "close:true");
        assert_eq!(events(&mut b), "shown:1:a,closed:1:a");
    });
}

#[test]
fn a_new_notification_with_the_same_tag_replaces_the_old_one_without_close() {
    big(|| {
        let mut b = browser(true);
        js(&mut b, "var log = []; var a = new Notification('one', { tag: 'chat' }); a.onclose = function () { log.push('close-a'); }; var c = new Notification('other');");
        pump(&mut b);
        js(&mut b, "var d = new Notification('two', { tag: 'chat' }); d.onshow = function () { log.push('show-d'); };");
        pump(&mut b);
        assert_eq!(js(&mut b, "log.join()"), "show-d");
        let titles: Vec<String> = b.notifications().into_iter().map(|n| n.title).collect();
        assert_eq!(titles, ["other", "two"]);
        assert_eq!(events(&mut b), "shown:1:one,shown:2:other,replaced:1:one,shown:3:two");
    });
}

#[test]
fn a_shown_notification_is_a_root() {
    big(|| {
        let mut b = browser(true);
        js(&mut b, "(function () { var n = new Notification('k'); n.onclick = function () {}; window.closeIt = function () { n.close(); }; })();");
        pump(&mut b);
        js(&mut b, "window.closeIt = null;");
        b.collect(&[]).expect("collect");
        assert_eq!(b.notifications().len(), 1);
        assert_eq!(b.notification_objects(), 1, "a shown notification is a root");
    });
}

#[test]
fn request_permission_with_no_decision_logs_the_request() {
    big(|| {
        let mut b = browser(false);
        js(&mut b, "Notification.requestPermission();");
        assert_eq!(b.take_notification_events().iter().map(|e| e.kind).collect::<Vec<_>>(), ["permission-requested"]);
        let mut b = browser(true);
        js(&mut b, "Notification.requestPermission();");
        assert!(b.take_notification_events().is_empty());
    });
}
