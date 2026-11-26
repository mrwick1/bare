//! Tests of the Cache API store of a session (`cache_store.rs`, `browser_caches.rs`, `shims/caches.js`, step 6 of
//! `docs/internal/design-service-workers.md`): a page and its service worker share the caches of the origin, the query options
//! (`ignoreSearch`, `ignoreMethod`, `Vary`, `ignoreVary`, `cacheName`), the errors of `put` as Chrome gives them, and the profile.
//! Time is the virtual clock.

use crate::browser_session::*;
use crate::js_limits::EvalLimits;
use crate::profile::{Profile, Saver};
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
        "/data" => Reply::new(200, Some("text/plain"), "from the network"),
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
fn a_page_and_its_service_worker_share_the_caches() {
    big(|| {
        let srv = server(
            "caches.open('v1').then(function(c){ return c.put('/a', new Response('hi', {headers: {'x-k': '1'}})) }).then(function(){ navigator.serviceWorker.register('/sw.js'); return navigator.serviceWorker.ready }).then(function(r){ navigator.serviceWorker.onmessage = function(e){ log(e.data) }; r.active.postMessage('go') });",
            "self.onmessage = function(e){ e.waitUntil(caches.match('/a').then(function(r){ return r.text().then(function(t){ return caches.open('w').then(function(c){ return c.add('/data') }).then(function(){ return caches.match('/data') }).then(function(d){ e.source.postMessage([t, r.headers.get('x-k'), r.url, d.url === new URL('/data', location).href].join('|')) }) }) })) };",
        );
        let mut s = session(&srv);
        let r = s.run();
        assert_eq!(log(&s, &srv).as_deref(), Some("hi|1||true,"), "{r:?}");
        let mut names: Vec<String> = s.storage().caches().names(&crate::sw_hub::origin_of(&srv.url("/")));
        names.sort();
        assert_eq!(names, ["v1", "w"]);
    });
}

#[test]
fn the_query_options_and_vary() {
    big(|| {
        let srv = server(
            "var c; caches.open('v').then(function(x){ c = x; return c.put('/p?q=1', new Response('one')) })\
             .then(function(){ return c.put(new Request('/v', {headers: {'accept-language': 'en'}}), new Response('en', {headers: {vary: 'Accept-Language'}})) })\
             .then(function(){ return Promise.all([c.match('/p'), c.match('/p', {ignoreSearch: true}), c.match(new Request('/p?q=1', {method: 'POST'})), c.match(new Request('/p?q=1', {method: 'POST'}), {ignoreMethod: true}), c.match(new Request('/v', {headers: {'accept-language': 'fr'}})), c.match(new Request('/v', {headers: {'accept-language': 'fr'}}), {ignoreVary: true}), c.match(new Request('/v', {headers: {'accept-language': 'en'}}))]) })\
             .then(function(l){ log(l.map(function(r){ return r ? r.status : 'none' }).join('|')); return c.put('/p?q=1', new Response('two')) })\
             .then(function(){ return c.keys() }).then(function(k){ log(k.map(function(r){ return new URL(r.url).pathname + new URL(r.url).search }).join('|')); return caches.match('/v', {cacheName: 'nope'}) })\
             .then(function(r){ log(String(r)); return c.delete('/p', {ignoreSearch: true}) }).then(function(d){ log(d); return c.matchAll() }).then(function(l){ log(l.length) });",
            "",
        );
        let mut s = session(&srv);
        let r = s.run();
        assert_eq!(log(&s, &srv).as_deref(), Some("none|200|none|200|none|200|200,/v|/p?q=1,undefined,true,1,"), "{r:?}");
    });
}

#[test]
fn put_rejects_as_chrome_does() {
    big(|| {
        let srv = server(
            "function e(p){ return p.then(function(){ return 'ok' }, function(x){ return x.name + ': ' + x.message }) } var r = new Response('x'); r.text(); caches.open('v').then(function(c){ return Promise.all([e(c.put(new Request('/a', {method: 'POST', body: 'b'}), new Response('x'))), e(c.put('/a', new Response('x', {headers: {vary: 'a, *'}}))), e(c.put('/a', r)), e(c.put('/a', new Response('x', {status: 206})))]) }).then(function(l){ log(l.join('|')) });",
            "",
        );
        let mut s = session(&srv);
        let r = s.run();
        assert_eq!(
            log(&s, &srv).as_deref(),
            Some("TypeError: Failed to execute 'put' on 'Cache': Request method 'POST' is unsupported|TypeError: Failed to execute 'put' on 'Cache': Vary header contains *|TypeError: Failed to execute 'put' on 'Cache': Response body is already used|TypeError: Failed to execute 'put' on 'Cache': Partial response (status code 206) is unsupported,"),
            "{r:?}"
        );
    });
}

#[test]
fn the_profile_keeps_the_caches() {
    big(|| {
        let dir = std::env::temp_dir().join(format!("bare-profile-caches-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let srv = server("if (!localStorage.getItem('log')) caches.open('v').then(function(c){ return c.put('/a', new Response('kept', {status: 201, statusText: 'Made'})) }).then(function(){ log('put') });", "");
        let mut s = session(&srv);
        let saver = Saver::new(Profile::open(&dir).unwrap(), s.storage());
        s.run();
        drop(saver);
        drop(s);
        assert!(dir.join("caches.bin").is_file());
        let st = Profile::open(&dir).unwrap().load().unwrap();
        assert_eq!(st.caches.len(), 1);
        assert_eq!(&*st.caches[0].2[0].body, b"kept");
        let srv2 = server("caches.match('/a').then(function(r){ return r.text().then(function(t){ log([t, r.status, r.statusText].join('|')) }) });", "");
        let mut s = session(&srv2);
        // The profile is for the origin of the first server: put its caches under the origin of the second one.
        let o2 = crate::sw_hub::origin_of(&srv2.url("/"));
        let o1 = crate::sw_hub::origin_of(&srv.url("/"));
        let moved: Vec<_> = st.caches.iter().map(|(_, n, e)| (o2.clone(), n.clone(), e.iter().map(|e| crate::cache_store::CacheEntry { url: e.url.replace(&o1, &o2), ..e.clone() }).collect())).collect();
        s.storage().import(&crate::browser_navigation_storage::StorageState { caches: moved, ..Default::default() }).unwrap();
        let r = s.run();
        assert_eq!(log(&s, &srv2).as_deref(), Some("kept|201|Made,"), "{r:?}");
        let _ = std::fs::remove_dir_all(dir);
    });
}
