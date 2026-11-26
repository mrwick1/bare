//! Tests of `navigator.serviceWorker.register` (`browser_sw.rs`, `sw_hub.rs`, steps 2 and 3 of `docs/internal/design-service-workers.md`).
//! Each page loads over loopback HTTP from a throwaway server of this process. The page writes what it sees to `localStorage`, and
//! the test reads it through the session storage. Time is the virtual clock. The messages are those of Chrome.

use crate::browser_loader::*;
use crate::browser_session::*;
use crate::js_limits::EvalLimits;
use crate::resources::test_server::{Reply, Server, serve};
use std::time::Duration;

fn big<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(64 << 20).spawn(f).unwrap().join().unwrap()
}

/// A route: the path, the `Content-Type`, the body and one more header.
type Route = (&'static str, &'static str, String, Option<(&'static str, &'static str)>);

fn server(routes: Vec<Route>) -> Server {
    serve(move |path| match routes.iter().find(|r| r.0 == path) {
        Some((_, mime, body, h)) => {
            let r = Reply::new(200, Some(mime), body.clone());
            match h {
                Some((n, v)) => r.header(n, v),
                None => r,
            }
        }
        None => Reply::new(404, Some("text/plain"), "no"),
    })
}

const LOG: &str = "function log(x){localStorage.setItem('log',(localStorage.getItem('log')||'')+x+',')}";

fn page(path: &'static str, script: &str) -> Route {
    (path, "text/html; charset=utf-8", format!("<script>{LOG}{script}</script>"), None)
}

fn js(path: &'static str, body: &str) -> Route {
    (path, "text/javascript", body.to_string(), None)
}

fn config(url: &str) -> SessionConfig {
    let mut c = SessionConfig::new(url);
    c.virtual_clock = true;
    c.eval = EvalLimits { max_steps: 5_000_000, ..Default::default() };
    c.page.quiet = Duration::from_millis(100);
    c
}

/// Run the page `/` of `srv` and give the log in its `localStorage`.
fn run(srv: &Server) -> (Option<String>, SessionResult, Session) {
    let mut s = Session::new(config(&srv.url("/")));
    let r = s.run();
    (s.storage().get_item(false, &srv.url(""), "log"), r, s)
}

#[test]
fn register_resolves_then_the_worker_installs_and_activates() {
    big(|| {
        let srv = server(vec![
            page("/", "navigator.serviceWorker.register('/sw.js').then(function(r){ log('scope:' + r.scope.slice(location.origin.length)); var w = r.installing; log(w.state + ':' + w.scriptURL.slice(location.origin.length)); w.onstatechange = function(){ log(w.state) }; }); navigator.serviceWorker.ready.then(function(r){ localStorage.setItem('ready', r.active.scriptURL.slice(location.origin.length) + ':' + r.scope.slice(location.origin.length)) });"),
            js("/sw.js", "self.addEventListener('install', function(e){ e.waitUntil(Promise.resolve()) });"),
        ]);
        let (log, r, s) = run(&srv);
        assert_eq!(r.stop, SessionStop::Page(Stop::Settled), "{r:?}");
        assert_eq!(log.as_deref(), Some("scope:/,installing:/sw.js,installed,activating,activated,"), "{r:?}");
        assert_eq!(s.storage().get_item(false, &srv.url(""), "ready").as_deref(), Some("/sw.js:/"));
        assert_eq!(srv.hits("/sw.js"), 1);
        let heads = srv.heads.lock().unwrap();
        let head = heads.iter().find(|h| h.starts_with("get /sw.js")).expect("script request");
        assert!(head.contains("service-worker: script"), "{head}");
    });
}

#[test]
fn the_registration_is_listed_and_unregister_removes_it() {
    big(|| {
        let srv = server(vec![
            page("/", "var c = navigator.serviceWorker; c.register('/a/sw.js').then(function(r){ return c.getRegistrations().then(function(l){ log(l.length + ':' + (l[0] === r)); return c.getRegistration('/a/x'); }).then(function(g){ log('get:' + (g === r)); return c.getRegistration('/b'); }).then(function(g){ log('other:' + g); return r.unregister(); }).then(function(ok){ log('unregister:' + ok); return c.getRegistrations(); }).then(function(l){ log('left:' + l.length); return r.unregister(); }).then(function(ok){ log('again:' + ok) }); });"),
            js("/a/sw.js", ""),
        ]);
        let (log, r, _) = run(&srv);
        assert_eq!(log.as_deref(), Some("1:true,get:true,other:undefined,unregister:true,left:0,again:false,"), "{r:?}");
    });
}

#[test]
fn register_rejects_as_chrome_does() {
    big(|| {
        let srv = server(vec![
            page("/", "function t(p){ return p.then(function(){ return 'ok' }, function(e){ return e.name + ': ' + e.message.split(location.origin).join('O') }) } var c = navigator.serviceWorker; Promise.all([t(c.register('/missing.js')), t(c.register('/page.html')), t(c.register('/a/sw.js', {scope: '/'})), t(c.register('https://other.test/sw.js')), t(c.register('/throws.js')), t(c.register('/a/sw.js', {updateViaCache: 'x'}))]).then(function(l){ localStorage.setItem('log', l.join('\\n')) });"),
            page("/page.html", ""),
            js("/a/sw.js", ""),
            js("/throws.js", "throw new Error('no')"),
        ]);
        let (log, r, _) = run(&srv);
        let want = [
            "TypeError: Failed to register a ServiceWorker for scope ('O/') with script ('O/missing.js'): A bad HTTP response code (404) was received when fetching the script.",
            "SecurityError: Failed to register a ServiceWorker for scope ('O/') with script ('O/page.html'): The script has an unsupported MIME type ('text/html').",
            "SecurityError: Failed to register a ServiceWorker for scope ('O/') with script ('O/a/sw.js'): The path of the provided scope ('/') is not under the max scope allowed ('/a/'). Adjust the scope, move the Service Worker script, or use the Service-Worker-Allowed HTTP header to allow the scope.",
            "SecurityError: Failed to register a ServiceWorker: The origin of the provided scriptURL ('https://other.test') does not match the current origin ('O').",
            "TypeError: Failed to register a ServiceWorker for scope ('O/') with script ('O/throws.js'): ServiceWorker script evaluation failed",
            "TypeError: Failed to execute 'register' on 'ServiceWorkerContainer': Failed to read the 'updateViaCache' property from 'RegistrationOptions': The provided value 'x' is not a valid enum value of type ServiceWorkerUpdateViaCache.",
        ];
        assert_eq!(log.as_deref(), Some(want.join("\n").as_str()), "{r:?}");
    });
}

#[test]
fn service_worker_allowed_widens_the_max_scope() {
    big(|| {
        let srv = server(vec![
            page("/", "navigator.serviceWorker.register('/a/sw.js', {scope: '/'}).then(function(r){ log('ok:' + r.scope.slice(location.origin.length)) }, function(e){ log(e.name) });"),
            ("/a/sw.js", "text/javascript", String::new(), Some(("Service-Worker-Allowed", "/"))),
        ]);
        let (log, r, _) = run(&srv);
        assert_eq!(log.as_deref(), Some("ok:/,"), "{r:?}");
    });
}

#[test]
fn a_page_that_never_reads_service_worker_names_is_no_client() {
    big(|| {
        let srv = server(vec![page("/", "log(1)")]);
        let (_, _, s) = run(&srv);
        assert!(!s.storage().sw_hub().borrow().has_regs());
        assert_eq!(s.storage().export().service_workers, vec![]);
    });
}
