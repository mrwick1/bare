//! Tests of the service worker lifecycle (`browser_sw.rs`, `sw_hub.rs`, step 3 of `docs/internal/design-service-workers.md`): the
//! controller, `clients.claim()`, a waiting worker, `skipWaiting()`, a failed install, the idle stop and the profile. Each page loads
//! over loopback HTTP from a throwaway server of this process. Time is the virtual clock.

use crate::browser_navigation_storage::WebStorage;
use crate::browser_session::*;
use crate::js_limits::EvalLimits;
use crate::profile::{Profile, Saver};
use crate::resources::test_server::{Reply, Server, serve};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

fn big<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(64 << 20).spawn(f).unwrap().join().unwrap()
}

const LOG: &str = "function log(x){localStorage.setItem('log',(localStorage.getItem('log')||'')+x+',')}";

/// `/` and `/two` are pages with `one` and `two`. `/sw.js` gives the scripts of `sw` in turn (the last one again at the end).
fn server(one: &str, two: &str, sw: Vec<&'static str>) -> Server {
    let (one, two) = (format!("<script>{LOG}{one}</script>"), format!("<script>{LOG}{two}</script>"));
    let n = Arc::new(AtomicUsize::new(0));
    serve(move |path| match path {
        "/" => Reply::new(200, Some("text/html"), one.clone()),
        "/two" => Reply::new(200, Some("text/html"), two.clone()),
        "/sw.js" => {
            let i = n.fetch_add(1, Ordering::Relaxed).min(sw.len() - 1);
            Reply::js(sw[i])
        }
        _ => Reply::new(404, Some("text/plain"), "no"),
    })
}

fn session(srv: &Server) -> Session {
    let mut c = SessionConfig::new(&srv.url("/"));
    c.virtual_clock = true;
    c.eval = EvalLimits { max_steps: 5_000_000, ..Default::default() };
    c.page.quiet = Duration::from_millis(100);
    Session::new(c)
}

fn log(s: &Session, srv: &Server) -> Option<String> {
    s.storage().get_item(false, &srv.url(""), "log")
}

#[test]
fn claim_makes_the_worker_control_the_page() {
    big(|| {
        let srv = server(
            "var c = navigator.serviceWorker; log('ctrl:' + c.controller); c.oncontrollerchange = function(){ log('change:' + c.controller.scriptURL.slice(location.origin.length) + ':' + c.controller.state) }; c.register('/sw.js');",
            "",
            vec!["self.onactivate = function(e){ e.waitUntil(clients.claim()) };"],
        );
        let mut s = session(&srv);
        let r = s.run();
        assert_eq!(log(&s, &srv).as_deref(), Some("ctrl:null,change:/sw.js:activating,"), "{r:?}");
    });
}

#[test]
fn a_page_loaded_under_an_active_worker_is_controlled_and_ready() {
    big(|| {
        let srv = server(
            "navigator.serviceWorker.register('/sw.js');",
            "var c = navigator.serviceWorker; log('ctrl:' + (c.controller && c.controller.scriptURL.slice(location.origin.length) + ':' + c.controller.state)); c.ready.then(function(r){ log('ready:' + (r.active === c.controller)) });",
            vec![""],
        );
        let mut s = session(&srv);
        s.run();
        let r = s.navigate(Some(&srv.url("/two")));
        assert_eq!(log(&s, &srv).as_deref(), Some("ctrl:/sw.js:activated,ready:true,"), "{r:?}");
        assert_eq!(srv.hits("/sw.js"), 1, "the start of a worker loads nothing");
    });
}

#[test]
fn a_new_version_waits_while_the_old_one_controls_a_page_then_skip_waiting_activates_it() {
    big(|| {
        let srv = server(
            "var c = navigator.serviceWorker; c.register('/sw.js').then(function(r){ c.oncontrollerchange = function(){ log('change'); r.update().then(function(){ var w = r.installing; log('installing:' + !!w); w.onstatechange = function(){ log('new:' + w.state + ':' + (r.waiting === w)) }; }) }; });",
            "",
            vec!["self.onactivate = function(e){ e.waitUntil(clients.claim()) };", "self.oninstall = function(){ };"],
        );
        let mut s = session(&srv);
        let r = s.run();
        assert_eq!(log(&s, &srv).as_deref(), Some("change,installing:true,new:installed:true,"), "{r:?}");
        let srv = server(
            "var c = navigator.serviceWorker; c.register('/sw.js').then(function(r){ c.oncontrollerchange = function(){ log('change'); r.update().then(function(){ var w = r.installing; w.onstatechange = function(){ log('new:' + w.state) }; }) }; });",
            "",
            vec!["self.onactivate = function(e){ e.waitUntil(clients.claim()) };", "self.oninstall = function(){ self.skipWaiting() };"],
        );
        let mut s = session(&srv);
        let r = s.run();
        assert_eq!(log(&s, &srv).as_deref(), Some("change,new:installed,new:activating,change,new:activated,"), "{r:?}");
    });
}

#[test]
fn a_failed_install_makes_the_worker_redundant() {
    big(|| {
        let srv = server(
            "var c = navigator.serviceWorker; c.register('/sw.js').then(function(r){ var w = r.installing; w.onstatechange = function(){ log(w.state + ':' + r.installing + ':' + r.active); if (w.state === 'redundant') c.getRegistrations().then(function(l){ log('left:' + l.length) }) } });",
            "",
            vec!["self.oninstall = function(e){ e.waitUntil(Promise.reject(new Error('no'))) };"],
        );
        let mut s = session(&srv);
        let r = s.run();
        assert_eq!(log(&s, &srv).as_deref(), Some("redundant:null:null,left:0,"), "{r:?}");
    });
}

#[test]
fn an_idle_worker_stops_after_30_seconds() {
    big(|| {
        let srv = server("navigator.serviceWorker.register('/sw.js');", "", vec![""]);
        let mut s = session(&srv);
        s.run();
        assert_eq!(s.storage().sw_hub().borrow().running(), 1, "the worker runs right after its activate");
        // A timer of the page at 31 s holds it: the clock moves there, and the worker has been idle for 30 s.
        let srv = server("navigator.serviceWorker.register('/sw.js'); setTimeout(function(){ log('late') }, 31000);", "", vec![""]);
        let mut c = SessionConfig::new(&srv.url("/"));
        c.virtual_clock = true;
        c.page.quiet = Duration::from_secs(40);
        let mut s = Session::new(c);
        let r = s.run();
        assert_eq!(log(&s, &srv).as_deref(), Some("late,"), "{r:?}");
        assert_eq!(s.storage().sw_hub().borrow().running(), 0, "the idle worker stopped");
    });
}

#[test]
fn the_profile_keeps_an_active_registration_and_a_new_session_is_controlled() {
    big(|| {
        let dir = std::env::temp_dir().join(format!("bare-profile-sw-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let srv = server("navigator.serviceWorker.register('/sw.js');", "var c = navigator.serviceWorker.controller; log('ctrl:' + (c && c.state));", vec!["// v1"]);
        let mut s = session(&srv);
        let saver = Saver::new(Profile::open(&dir).unwrap(), s.storage());
        s.run();
        drop(saver);
        drop(s);
        assert!(dir.join("sw.bin").is_file());
        let st = Profile::open(&dir).unwrap().load().unwrap();
        assert_eq!(st.service_workers.len(), 1);
        assert_eq!((st.service_workers[0].scope.as_str(), st.service_workers[0].text.as_str()), (srv.url("/").as_str(), "// v1"));
        let mut c = SessionConfig::new(&srv.url("/two"));
        c.virtual_clock = true;
        c.page.quiet = Duration::from_millis(100);
        let mut s = Session::new(c);
        s.storage().import(&st).unwrap();
        let r = s.run();
        assert_eq!(log(&s, &srv).as_deref(), Some("ctrl:activated,"), "{r:?}");
        assert_eq!(srv.hits("/sw.js"), 1);
        let back = WebStorage::default();
        back.import(&st).unwrap();
        assert_eq!(back.export().service_workers, st.service_workers);
        let _ = std::fs::remove_dir_all(dir);
    });
}
