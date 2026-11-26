//! Tests of `Notification` (`browser_notification.rs`) in a page where permission is "denied". The rules are from the Notifications
//! API ("https://notifications.spec.whatwg.org/"). bare cannot show a notification, so the object is made, and a task after the
//! current one fires a plain `error` event at it.

use crate::eng_new::Peek;
use crate::browser::*;
use bare_html::tree;
use crate::eng_new::NewFault as Error;
use crate::js_limits::EvalLimits;
use crate::js_limits::HeapLimits;

fn big<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(64 << 20).spawn(f).unwrap().join().unwrap()
}

fn browser() -> Browser {
    let mut b = Browser::new(tree::parse("<body></body>"), BrowserLimits::default(), EvalLimits { max_steps: 5_000_000, ..Default::default() }, HeapLimits::default()).expect("browser");
    b.use_virtual_clock();
    b.set_document_url("https://example.test/dir/page.html").expect("url");
    b
}

fn js(b: &mut Browser, src: &str) -> String {
    match b.run_script(src) {
        Ok(v) => b.it.display(v),
        Err(Error::Throw(v)) if crate::eng_new::is_object(v) => format!("throw:{}:{}", b.it.get_str(v, "name"), b.it.get_str(v, "message")),
        Err(Error::Throw(v)) => format!("throw-value:{}", b.it.display(v)),
        Err(Error::Limit(l)) => format!("limit:{l:?}"),
        Err(Error::Unsupported(u)) => format!("unsupported:{}", u),
        Err(e) => format!("other:{e:?}"),
    }
}

fn pump(b: &mut Browser) {
    b.advance_time(10.0, 1000).expect("advance");
}

#[test]
fn notification_is_a_constructor_with_an_event_target_prototype() {
    big(|| {
        let mut b = browser();
        assert_eq!(js(&mut b, "typeof Notification + ',' + Notification.name + ',' + Notification.length + ',' + ('Notification' in window)"), "function,Notification,1,true");
        assert_eq!(js(&mut b, "(Notification.prototype instanceof EventTarget) + ',' + (Object.getPrototypeOf(Notification.prototype) === EventTarget.prototype)"), "true,true");
        assert_eq!(js(&mut b, "var n = new Notification('x'); (n instanceof Notification) + ',' + (n instanceof EventTarget)"), "true,true");
        assert_eq!(js(&mut b, "try { Notification('x'); } catch (e) { e.name; }"), "TypeError");
        assert_eq!(js(&mut b, "try { new Notification(); } catch (e) { e.name; }"), "TypeError");
    });
}

#[test]
fn permission_is_denied_and_max_actions_is_zero() {
    big(|| {
        let mut b = browser();
        assert_eq!(js(&mut b, "Notification.permission + ',' + Notification.maxActions + ',' + Notification.requestPermission.length"), "denied,2,0");
    });
}

#[test]
fn request_permission_resolves_denied_and_calls_the_callback_with_denied() {
    big(|| {
        let mut b = browser();
        js(&mut b, "var got = []; Notification.requestPermission().then(function (p) { got.push('promise:' + p); }); Notification.requestPermission(function (p) { got.push('callback:' + p); });");
        assert_eq!(js(&mut b, "got.join()"), "callback:denied");
        pump(&mut b);
        assert_eq!(js(&mut b, "got.join()"), "callback:denied,promise:denied");
    });
}

#[test]
fn the_title_body_and_tag_are_read_back_and_close_does_nothing() {
    big(|| {
        let mut b = browser();
        assert_eq!(js(&mut b, "var n = new Notification('hi', { body: 'b', tag: 't' }); n.title + ',' + n.body + ',' + n.tag"), "hi,b,t");
        assert_eq!(js(&mut b, "var m = new Notification('x'); m.body + '|' + m.tag + '|' + m.close() + '|' + n.close()"), "||undefined|undefined");
        assert_eq!(js(&mut b, "typeof n.onerror + ',' + n.onerror"), "object,null");
    });
}

#[test]
fn the_error_event_fires_once_after_the_current_task() {
    big(|| {
        let mut b = browser();
        let r = js(&mut b, "var log = []; var n = new Notification('x'); n.onerror = function (e) { log.push(e.type + ':' + (e.target === n) + ':' + e.bubbles + ':' + e.cancelable + ':' + (e instanceof Event) + ':' + (e instanceof ErrorEvent)); }; n.addEventListener('error', function () { log.push('listener'); }); log.push('sync'); log.join()");
        assert_eq!(r, "sync");
        pump(&mut b);
        assert_eq!(js(&mut b, "log.join()"), "sync,error:true:false:false:true:false,listener");
        pump(&mut b);
        assert_eq!(js(&mut b, "log.length"), "3");
    });
}
