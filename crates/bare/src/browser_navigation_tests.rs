//! Tests of the navigation module: URL, URLSearchParams, location, history, storage. Real scripts in a real realm.
//! Expectations are written by hand from the URL and HTML Standards (structural fixtures); only `URL` and
//! `URLSearchParams` have a Node oracle (`tools/js/navigation_check.sh`). Each test runs on a big stack.

use crate::browser::*;
use crate::browser_navigation::*;
use bare_html::tree;
use bare_js::eval::{Error, EvalLimits, LimitKind};
use bare_js::heap::HeapLimits;
use bare_js::value::Value;

fn big<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(64 << 20).spawn(f).unwrap().join().unwrap()
}

const PAGE: &str = "<!doctype html><html><head></head><body><div id=a></div></body></html>";
const URL0: &str = "https://staging.example.test:8443/a/b?x=1&y=2#top";

fn mk(url: Option<&str>, eval: EvalLimits) -> Browser {
    let mut b = Browser::new(tree::parse(PAGE), BrowserLimits::default(), eval, HeapLimits::default()).expect("browser");
    if let Some(u) = url {
        b.set_document_url(u).expect("url");
    }
    b
}

fn at(url: &str) -> Browser {
    mk(Some(url), EvalLimits { max_steps: 5_000_000, ..Default::default() })
}

fn show(b: &mut Browser, r: Result<Value, Error>) -> String {
    match r {
        Ok(v) => b.it.display(v),
        Err(Error::Throw(v)) if matches!(v, Value::Object(_)) => format!("throw:{}:{}", b.it.get_str(v, "name"), b.it.get_str(v, "message")),
        Err(Error::Throw(v)) => format!("throw-value:{}", b.it.display(v)),
        Err(Error::Limit(l)) => format!("limit:{l:?}"),
        Err(Error::Unsupported(u)) => format!("unsupported:{}", u.what),
        Err(e) => format!("other:{e:?}"),
    }
}

fn js(b: &mut Browser, src: &str) -> String {
    let r = b.run_script(src);
    show(b, r)
}

fn lim(name: &'static str) -> String {
    format!("limit:{:?}", LimitKind::Host(name))
}

const NOURL: &str = "unsupported:document URL is not configured (call Browser::set_document_url)";


fn drain_requests(b: &mut Browser) -> Vec<NavigationRequest> {
    std::iter::from_fn(|| b.take_navigation_request()).collect()
}

fn req(kind: NavigationKind, url: &str, from: &str) -> NavigationRequest {
    NavigationRequest { kind, url: url.into(), from: from.into(), body: None }
}

// ------------------------------------------------------------------------------------------ configuration

#[test]
fn unconfigured_refuses_and_never_invents_an_origin() {
    big(|| {
        let mut b = mk(None, EvalLimits::default());
        assert_eq!(b.document_url(), None);
        assert_eq!(b.base_uri(), None);
        assert!(b.resolve_url("/x").is_err());
        for src in [
            "location.href", "location.origin", "document.URL", "document.baseURI", "document.documentURI", "window.origin", "history.length",
            "history.state", "history.pushState(1, '', '/x')", "history.back()", "localStorage.getItem('a')", "sessionStorage.length", "location.assign('/x')",
            "location.reload()", "location.hash = 'x'",
        ] {
            assert_eq!(js(&mut b, src), NOURL, "{src}");
        }
        // the objects exist, and URL parsing that needs no document works
        assert_eq!(js(&mut b, "[typeof location, typeof history, typeof localStorage, document.location === location].join()"), "object,object,object,true");
        assert_eq!(js(&mut b, "new URL('http://a.com/x?y#z').href"), "http://a.com/x?y#z");
        assert_eq!(js(&mut b, "new URL('/x')"), "throw:TypeError:Failed to construct 'URL': Invalid URL");
        assert_eq!(js(&mut b, "new URLSearchParams('a=1').get('a')"), "1");
        assert_eq!(b.navigation_stats().history_len, 0);
    });
}

#[test]
fn set_document_url_checks_and_resets() {
    big(|| {
        let mut b = mk(None, EvalLimits::default());
        for bad in ["/relative", "ftp://a.com/", "mailto:a@b.c", "http://u:p@a.com/", "http://", "javascript:1", "http://a.com\\x"] {
            assert!(b.set_document_url(bad).is_err(), "{bad}");
            assert_eq!(b.document_url(), None, "{bad}");
        }
        b.set_document_url("HTTPS://Staging.Example.test:443/a/./b/../c?x=a b#f g").unwrap();
        assert_eq!(b.document_url().unwrap(), "https://staging.example.test/a/c?x=a%20b#f%20g");
        assert_eq!(b.base_uri(), b.document_url());
        assert_eq!(b.resolve_url("../d?e#f").unwrap(), "https://staging.example.test/d?e#f");
        assert_eq!(b.resolve_url("//other.test/x").unwrap(), "https://other.test/x");
        assert!(b.resolve_url("mailto:x").is_err());
        let g = b.url_generation();
        js(&mut b, "history.pushState({a: 1}, '', '/b'); location.hash = 'x'");
        assert_eq!(b.navigation_stats().history_len, 3);
        assert_eq!(b.navigation_stats().pending_tasks, 1);
        b.set_document_url("http://a.com/").unwrap();
        let st = b.navigation_stats();
        assert_eq!((st.history_len, st.history_index, st.pending_tasks, st.pending_requests, st.state_bytes), (1, 0, 0, 0, 0));
        assert!(b.url_generation() > g);
        assert_eq!(js(&mut b, "[history.length, history.state, location.href].join()"), "1,,http://a.com/");
    });
}

#[test]
fn location_and_document_urls() {
    big(|| {
        let mut b = at(URL0);
        assert_eq!(js(&mut b, "[location.href, location.origin, location.protocol, location.host, location.hostname, location.port].join(' ')"), "https://staging.example.test:8443/a/b?x=1&y=2#top https://staging.example.test:8443 https: staging.example.test:8443 staging.example.test 8443");
        assert_eq!(js(&mut b, "[location.pathname, location.search, location.hash, String(location), location.toString()].join(' ')"), "/a/b ?x=1&y=2 #top https://staging.example.test:8443/a/b?x=1&y=2#top https://staging.example.test:8443/a/b?x=1&y=2#top");
        assert_eq!(js(&mut b, "[document.URL, document.documentURI, document.baseURI, document.body.baseURI, window.origin].join(' ')"), format!("{URL0} {URL0} {URL0} {URL0} https://staging.example.test:8443"));
        assert_eq!(js(&mut b, "[window.location === location, document.location === location, self.location === location, location instanceof Location, history instanceof History, localStorage instanceof Storage].join()"), "true,true,true,true,true,true");
        assert_eq!(js(&mut b, "new Location()"), "throw:TypeError:Location is not a constructor");
        assert_eq!(js(&mut b, "Object.getOwnPropertyDescriptor(Location.prototype, 'href').get.call({})"), "throw:TypeError:Illegal invocation");
        let mut b = at("http://[::1]:80/");
        assert_eq!(js(&mut b, "[location.host, location.hostname, location.port, location.origin, location.pathname, location.search, location.hash].join('|')"), "[::1]|[::1]||http://[::1]|/||");
    });
}

// ------------------------------------------------------------------------------------------ location: requests

#[test]
fn location_navigation_makes_requests_and_never_changes_the_url() {
    big(|| {
        let mut b = at(URL0);
        let from = URL0;
        let g = b.url_generation();
        assert_eq!(js(&mut b, "location.assign('/login?next=%2Fa'); location.replace('c'); location.reload(); location.href = 'https://other.test/x'; location.search = '?z=1 2'; location.pathname = 'p q'; location.port = '9000'; location.protocol = 'http'; history.go(0)"), "undefined");
        assert_eq!(
            drain_requests(&mut b),
            vec![
                req(NavigationKind::Assign, "https://staging.example.test:8443/login?next=%2Fa", from),
                req(NavigationKind::Assign, "https://staging.example.test:8443/a/c", from).with(NavigationKind::Replace),
                req(NavigationKind::Reload, URL0, URL0),
                req(NavigationKind::Assign, "https://other.test/x", from),
                req(NavigationKind::Assign, "https://staging.example.test:8443/a/b?z=1%202#top", from),
                req(NavigationKind::Assign, "https://staging.example.test:8443/p%20q?x=1&y=2#top", from),
                req(NavigationKind::Assign, "https://staging.example.test:9000/a/b?x=1&y=2#top", from),
                req(NavigationKind::Assign, "http://staging.example.test:8443/a/b?x=1&y=2#top", from),
                req(NavigationKind::Reload, URL0, URL0),
            ]
        );
        assert_eq!(b.document_url().unwrap(), URL0);
        assert_eq!(b.url_generation(), g);
        let st = b.navigation_stats();
        assert_eq!((st.history_len, st.pending_tasks, st.pending_requests), (1, 0, 0));
        // the same URL is a replace
        js(&mut b, "location.assign(location.href.split('#')[0] + '#top')");
        assert_eq!(b.navigation_stats().history_len, 1, "same fragment URL replaces");
        js(&mut b, "location.assign('/a/b?x=1&y=2')");
        assert_eq!(drain_requests(&mut b), vec![req(NavigationKind::Assign, "https://staging.example.test:8443/a/b?x=1&y=2", from)]);
    });
}

impl NavigationRequest {
    fn with(mut self, k: NavigationKind) -> Self {
        self.kind = k;
        self
    }
}

#[test]
fn replace_is_a_replace_request_and_errors_are_precise() {
    big(|| {
        let mut b = at(URL0);
        js(&mut b, "location.replace('/r')");
        assert_eq!(drain_requests(&mut b)[0].kind, NavigationKind::Replace);
        assert_eq!(js(&mut b, "try { location.assign('http://') } catch (e) { e.name + ':' + e.code }"), "SyntaxError:12");
        assert_eq!(js(&mut b, "try { location.href = 'http://a b/' } catch (e) { e.name }"), "SyntaxError");
        assert_eq!(js(&mut b, "location.assign('javascript:void(0)')"), "unsupported:URL scheme other than http and https");
        assert_eq!(js(&mut b, "location.assign('mailto:a@b.c')"), "unsupported:URL scheme other than http and https");
        assert_eq!(js(&mut b, "location.assign('http://u:p@a.com/')"), "unsupported:user info in a URL");
        assert_eq!(js(&mut b, "location.protocol = 'ftp'"), "unsupported:URL scheme other than http and https");
        assert_eq!(js(&mut b, "location.protocol = 'mailto'; 1"), "1");
        assert_eq!(js(&mut b, "location.port = 'x'; location.hostname = ''; location.host = 'a b'; 1"), "1");
        assert!(drain_requests(&mut b).is_empty(), "failed or ignored sets queue nothing");
        assert_eq!(js(&mut b, "location.assign()"), "throw:TypeError:Failed to execute 'assign' on 'Location': 1 argument required, but only 0 present.");
        assert_eq!(js(&mut b, "window.location = '/via-window'"), "/via-window");
        assert_eq!(drain_requests(&mut b)[0].url, "https://staging.example.test:8443/via-window");
        assert_eq!(js(&mut b, "document.location = '/via-doc'"), "/via-doc");
        assert_eq!(drain_requests(&mut b)[0].url, "https://staging.example.test:8443/via-doc");
    });
}

#[test]
fn requests_are_bounded() {
    big(|| {
        let mut b = at(URL0);
        b.set_navigation_limits(NavLimits { max_requests: 3, ..Default::default() });
        assert_eq!(js(&mut b, "location.assign('/1'); location.assign('/2'); location.reload(); location.assign('/4')"), lim("nav-requests"));
        assert_eq!(drain_requests(&mut b).len(), 3);
    });
}

// ------------------------------------------------------------------------------------------ fragments and traversal

#[test]
fn fragment_navigation_updates_at_once_and_queues_events() {
    big(|| {
        let mut b = at("https://a.test/p?q=1");
        js(&mut b, "var log = []; window.addEventListener('popstate', function (e) { log.push('pop:' + e.state + ':' + location.hash); }); window.addEventListener('hashchange', function (e) { log.push('hash:' + e.oldURL + '>' + e.newURL); });");
        let g = b.url_generation();
        assert_eq!(js(&mut b, "location.hash = 'sec 1'; [location.href, location.hash, history.length].join(' ')"), "https://a.test/p?q=1#sec%201 #sec%201 2");
        assert!(b.url_generation() > g);
        assert_eq!(js(&mut b, "log.length"), "0", "events wait for the driver");
        assert!(drain_requests(&mut b).is_empty(), "a fragment navigation loads nothing");
        assert_eq!(b.run_navigation_tasks().unwrap(), 1);
        assert_eq!(js(&mut b, "log.join()"), "pop:null:#sec%201,hash:https://a.test/p?q=1>https://a.test/p?q=1#sec%201");
        assert_eq!(b.run_navigation_tasks().unwrap(), 0);
        // the same fragment again does nothing; href and assign with a fragment make entries; the same URL replaces
        js(&mut b, "log.length = 0; location.hash = '#sec%201'; location.href = '#two'; location.assign('#two'); location.replace('#three')");
        assert_eq!(js(&mut b, "[location.hash, history.length].join()"), "#three,3");
        assert_eq!(b.run_navigation_tasks().unwrap(), 3);
        assert_eq!(js(&mut b, "var h = 0; for (var i = 0; i < log.length; i++) if (log[i].indexOf('hash:') == 0) h++; h"), "3");
        assert!(drain_requests(&mut b).is_empty());
        // `location.hash = ''` leaves an empty fragment ("#"), so it is a fragment navigation too
        js(&mut b, "location.hash = ''");
        assert!(drain_requests(&mut b).is_empty());
        assert_eq!(js(&mut b, "location.href + '|' + location.hash"), "https://a.test/p?q=1#|");
    });
}

#[test]
fn traversal_is_queued_and_runs_in_the_driver() {
    big(|| {
        let mut b = at("https://a.test/start");
        js(&mut b, "var log = []; window.addEventListener('popstate', function (e) { log.push(location.pathname + ':' + JSON.stringify(e.state) + ':' + history.length); });
            history.pushState({n: 1}, '', '/one'); history.pushState({n: 2}, '', '/two'); history.pushState({n: 3}, '', '/three');");
        assert_eq!(js(&mut b, "[history.length, location.pathname, JSON.stringify(history.state)].join()"), r#"4,/three,{"n":3}"#);
        assert_eq!(js(&mut b, "history.back(); history.back(); history.go(-1)"), "undefined");
        assert_eq!(js(&mut b, "location.pathname"), "/three", "nothing moved yet");
        assert_eq!(b.navigation_stats().pending_tasks, 3);
        assert_eq!(b.run_navigation_tasks().unwrap(), 3);
        assert_eq!(js(&mut b, "log.join(' ')"), r#"/two:{"n":2}:4 /one:{"n":1}:4 /start:null:4"#);
        assert_eq!(js(&mut b, "history.back(); history.go(-5); history.go(1); history.forward(); history.go(Infinity); history.go(NaN)"), "undefined");
        // go(0) and non-finite are a reload; the rest: back is out of range (no-op), go(-5) no-op, forward twice
        let reloads = drain_requests(&mut b);
        assert_eq!(reloads.len(), 2);
        assert!(reloads.iter().all(|r| r.kind == NavigationKind::Reload && r.url == "https://a.test/start"));
        js(&mut b, "log.length = 0");
        assert_eq!(b.run_navigation_tasks().unwrap(), 4);
        assert_eq!(js(&mut b, "log.join(' ')"), r#"/one:{"n":1}:4 /two:{"n":2}:4"#);
        assert_eq!(js(&mut b, "history.state === history.state"), "true");
        assert_eq!(b.document_url().unwrap(), "https://a.test/two");
        assert!(drain_requests(&mut b).is_empty(), "traversal never loads");
    });
}

#[test]
fn traversal_between_fragments_fires_hashchange_after_popstate() {
    big(|| {
        let mut b = at("https://a.test/p");
        js(&mut b, "history.pushState(null, '', '/p#a'); history.pushState(null, '', '/q#a'); var log = [];
            window.addEventListener('popstate', function () { log.push('pop'); }); window.addEventListener('hashchange', function (e) { log.push('hash:' + e.newURL); });");
        js(&mut b, "history.back(); history.back(); history.forward()");
        b.run_navigation_tasks().unwrap();
        // /q#a -> /p#a (path differs: no hashchange) -> /p (fragment differs) -> /p#a
        assert_eq!(js(&mut b, "log.join()"), "pop,pop,hash:https://a.test/p,pop,hash:https://a.test/p#a");
    });
}

#[test]
fn a_task_that_queues_more_tasks_does_not_loop_the_driver() {
    big(|| {
        let mut b = at("https://a.test/");
        js(&mut b, "history.pushState(1, '', '/1'); history.pushState(2, '', '/2'); var n = 0; window.addEventListener('popstate', function () { n++; history.go(n % 2 ? 1 : -1); });");
        js(&mut b, "history.back()");
        assert_eq!(b.run_navigation_tasks().unwrap(), 1);
        assert_eq!(js(&mut b, "n"), "1");
        assert_eq!(b.navigation_stats().pending_tasks, 1, "the new task waits for the next call");
        assert_eq!(b.run_navigation_tasks().unwrap(), 1);
        assert_eq!(js(&mut b, "n"), "2");
    });
}

#[test]
fn tasks_are_bounded() {
    big(|| {
        let mut b = at("https://a.test/");
        b.set_navigation_limits(NavLimits { max_tasks: 2, ..Default::default() });
        assert_eq!(js(&mut b, "history.back(); history.back(); history.back()"), lim("nav-tasks"));
        assert_eq!(js(&mut b, "location.hash = 'a'; location.hash = 'b'"), lim("nav-tasks"));
        assert_eq!(b.navigation_stats().pending_tasks, 2);
    });
}

#[test]
fn a_limit_in_a_listener_stops_the_driver_and_no_more_js_runs() {
    big(|| {
        let mut b = mk(Some("https://a.test/"), EvalLimits { max_steps: 20_000, ..Default::default() });
        js(&mut b, "history.pushState(1, '', '/1'); history.pushState(2, '', '/2'); var n = 0, other = 0;
            window.addEventListener('popstate', function () { if (n++ == 0) for (;;) {} });
            window.addEventListener('hashchange', function () { other++; });");
        js(&mut b, "history.back(); history.back()");
        let e = b.run_navigation_tasks().unwrap_err();
        assert!(matches!(e, Error::Limit(LimitKind::Steps)), "{e:?}");
        assert_eq!(js(&mut b, "n"), "1");
        assert_eq!(b.navigation_stats().pending_tasks, 1, "the spent task is gone, the next one waits");
        assert!(matches!(b.run_navigation_tasks(), Err(Error::Limit(LimitKind::Steps))));
        assert_eq!(js(&mut b, "n + ',' + other"), "1,0");
    });
}

#[test]
fn a_throwing_listener_is_reported_and_the_task_goes_on() {
    big(|| {
        let mut b = at("https://a.test/");
        js(&mut b, "history.pushState(1, '', '/1'); var ok = 0; window.addEventListener('popstate', function () { throw new Error('boom'); }); window.addEventListener('popstate', function () { ok++; });
            history.back();");
        assert_eq!(b.run_navigation_tasks().unwrap(), 1);
        assert_eq!(js(&mut b, "ok"), "1");
        let r = b.take_reports();
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].event_type, "popstate");
        assert!(r[0].message.contains("boom"));
    });
}

// ------------------------------------------------------------------------------------------ history

#[test]
fn push_and_replace_state() {
    big(|| {
        let mut b = at(URL0);
        assert_eq!(js(&mut b, "[history.length, history.state, history.scrollRestoration].join()"), "1,,auto");
        let g = b.url_generation();
        assert_eq!(js(&mut b, "history.pushState({id: 1, tags: ['a', 'b']}, 'ignored title', '/journeys/1?tab=x#h'); [history.length, location.href, JSON.stringify(history.state)].join(' ')"), r#"2 https://staging.example.test:8443/journeys/1?tab=x#h {"id":1,"tags":["a","b"]}"#);
        assert_eq!(b.url_generation(), g + 1);
        assert_eq!(js(&mut b, "history.replaceState({id: 2}, '', 'rel?z=1'); [history.length, location.pathname + location.search, location.hash, JSON.stringify(history.state)].join(' ')"), r#"2 /journeys/rel?z=1  {"id":2}"#);
        assert_eq!(js(&mut b, "history.replaceState(history.state, ''); [history.length, location.pathname, JSON.stringify(history.state)].join(' ')"), r#"2 /journeys/rel {"id":2}"#);
        // url omitted or null: the URL stays
        let g = b.url_generation();
        js(&mut b, "history.pushState(5, ''); history.pushState(6, '', null); history.pushState(7, '', undefined)");
        assert_eq!(b.url_generation(), g);
        assert_eq!(js(&mut b, "[history.length, history.state].join()"), "5,7");
        // pushing after going back drops the forward entries
        js(&mut b, "history.back(); history.back()");
        b.run_navigation_tasks().unwrap();
        assert_eq!(js(&mut b, "history.state"), "5");
        js(&mut b, "history.pushState('new', '', '/n')");
        assert_eq!(js(&mut b, "[history.length, history.state].join()"), "4,new");
        assert_eq!(js(&mut b, "history.forward(); 1"), "1");
        b.run_navigation_tasks().unwrap();
        assert_eq!(js(&mut b, "history.state"), "new", "no forward entry");
        assert!(drain_requests(&mut b).is_empty());
        // scrollRestoration is a stored value with an enum check
        assert_eq!(js(&mut b, "history.scrollRestoration = 'manual'; var a = history.scrollRestoration; history.scrollRestoration = 'bogus'; a + history.scrollRestoration"), "manualmanual");
    });
}

#[test]
fn state_is_cloned_data_not_the_callers_object() {
    big(|| {
        let mut b = at(URL0);
        js(&mut b, "var src = {a: 1, nested: {list: [1, 2, {deep: true}]}, u: undefined, n: null, neg: -0, nan: NaN, s: 'x\\ud83d\\ude00y\\ud800'}; history.pushState(src, '');");
        assert_eq!(js(&mut b, "src.a = 99; src.nested.list.push(4); src.nested.list[2].deep = false; JSON.stringify(history.state.nested) + history.state.a"), r#"{"list":[1,2,{"deep":true}]}1"#);
        assert_eq!(js(&mut b, "var s = history.state; [s === src, s.nested === src.nested, s === history.state, Object.getPrototypeOf(s) === Object.prototype, Array.isArray(s.nested.list)].join()"), "false,false,true,true,true");
        assert_eq!(js(&mut b, "['u' in s, s.u === undefined, 1 / s.neg, s.nan !== s.nan, s.n === null, s.s.length, s.s.charCodeAt(1), s.s.charCodeAt(4)].join()"), "true,true,-Infinity,true,true,5,55357,55296");
        // changing the result does not change the entry: it is cached once, and the entry is data
        assert_eq!(js(&mut b, "s.a = 'mutated'; history.state.a"), "mutated");
        // a new entry gets its own graph; traversal gives back the cached object
        js(&mut b, "history.pushState(null, ''); history.back()");
        b.run_navigation_tasks().unwrap();
        assert_eq!(js(&mut b, "history.state === s"), "true");
    });
}

#[test]
fn state_keeps_cycles_shared_references_sparse_arrays_and_null_prototypes() {
    big(|| {
        let mut b = at(URL0);
        js(&mut b, "var a = {name: 'a'}; a.self = a; var shared = {v: 1}; a.l = [shared, shared, a]; var sp = [1, , 3]; sp.extra = 'e'; a.sp = sp; var np = Object.create(null); np.k = 1; a.np = np; history.pushState(a, '');");
        assert_eq!(js(&mut b, "var s = history.state; [s !== a, s.self === s, s.l[0] === s.l[1], s.l[0] !== shared, s.l[2] === s, s.sp.length, 1 in s.sp, s.sp.extra, s.sp[2], Object.getPrototypeOf(s.np) === Object.prototype, s.np.k].join()"), "true,true,true,true,true,3,false,e,3,true,1");
        // an array that holds itself
        js(&mut b, "var arr = [1]; arr.push(arr); history.replaceState(arr, '');");
        assert_eq!(js(&mut b, "history.state[1] === history.state && history.state[0] === 1 && history.state !== arr"), "true");
        // primitives and the 3 empties
        for (src, want) in [("undefined", "undefined"), ("null", "null"), ("0", "0"), ("'str'", "str"), ("true", "true"), ("NaN", "NaN")] {
            assert_eq!(js(&mut b, &format!("history.replaceState({src}, ''); String(history.state)")), want, "{src}");
        }
    });
}

#[test]
fn state_gates_and_clone_errors() {
    big(|| {
        let mut b = at(URL0);
        let g = b.navigation_stats();
        assert_eq!(js(&mut b, "try { history.pushState(function () {}, '') } catch (e) { e.name + ':' + e.code }"), "DataCloneError:25");
        assert_eq!(js(&mut b, "try { history.pushState({deep: {f: function () {}}}, '') } catch (e) { e.name }"), "DataCloneError");
        assert_eq!(js(&mut b, "try { history.pushState({n: document.body}, '') } catch (e) { e.name }"), "DataCloneError");
        assert_eq!(js(&mut b, "try { history.pushState(new URL('http://a.com'), '') } catch (e) { e.name }"), "DataCloneError");
        assert_eq!(js(&mut b, "try { history.pushState(new URLSearchParams(), '') } catch (e) { e.name }"), "DataCloneError");
        assert_eq!(js(&mut b, "try { history.pushState(location, '') } catch (e) { e.name }"), "DataCloneError");
        assert_eq!(js(&mut b, "try { history.pushState(localStorage, '') } catch (e) { e.name }"), "DataCloneError");
        assert_eq!(js(&mut b, "try { history.pushState(new Event('x'), '') } catch (e) { e.name }"), "DataCloneError");
        // other object kinds are cloned by their kind (`structuredClone`), or refused
        assert_eq!(js(&mut b, "try { history.pushState({p: Promise.resolve(1)}, '') } catch (e) { e.name }"), "DataCloneError");
        let a = b.navigation_stats();
        assert_eq!((a.history_len, a.state_bytes, a.url_generation), (g.history_len, g.state_bytes, g.url_generation), "nothing committed");
        assert_eq!(js(&mut b, "history.length"), "1");
        for (ctor, want) in [("new Map([[1, 2]])", "Map:1"), ("new Uint8Array(2)", "Uint8Array:2"), ("/re/g", "RegExp:/re/g"), ("new Error('x')", "Error:x"), ("Object.create({inherited: 1})", "Object:undefined"), ("new (function Foo() { this.a = 1; })()", "Object:1")] {
            let src = format!("history.pushState({{x: {ctor}}}, ''); var v = history.state.x; v.constructor.name + ':' + (v instanceof Map ? v.size : v instanceof Uint8Array ? v.length : v instanceof RegExp ? String(v) : v instanceof Error ? v.message : v.a || v.inherited)");
            assert_eq!(js(&mut b, &src), want, "{ctor}");
        }
    });
}

#[test]
fn symbol_state_is_a_clone_error_when_symbols_exist() {
    big(|| {
        let mut b = at(URL0);
        if js(&mut b, "typeof Symbol") == "function" {
            assert_eq!(js(&mut b, "try { history.pushState({s: Symbol('x')}, '') } catch (e) { e.name }"), "DataCloneError");
            // symbol keys are ignored by structured clone
            assert_eq!(js(&mut b, "var o = {a: 1}; o[Symbol('k')] = 2; history.pushState(o, ''); Object.keys(history.state).join()"), "a");
        }
    });
}

#[test]
fn cross_origin_history_updates_fail_before_anything_commits() {
    big(|| {
        let mut b = at(URL0);
        js(&mut b, "history.pushState({keep: 1}, '', '/keep')");
        let before = b.navigation_stats();
        let url = b.document_url();
        for (u, what) in [
            ("https://evil.test/x", "other host"), ("https://staging.example.test/x", "other port"), ("http://staging.example.test:8443/x", "other scheme"),
            ("//evil.test/", "scheme relative other host"), ("mailto:a@b.c", "other scheme"), ("javascript:void(0)", "javascript"), ("http://", "unparseable"),
        ] {
            let r = js(&mut b, &format!("try {{ history.pushState({{leak: 1}}, '', '{u}') }} catch (e) {{ e.name + ':' + e.code }}"));
            assert_eq!(r, "SecurityError:18", "{what}: {u}");
            let r = js(&mut b, &format!("try {{ history.replaceState({{leak: 1}}, '', '{u}') }} catch (e) {{ e.name }}"));
            assert_eq!(r, "SecurityError", "{what}: {u}");
        }
        let after = b.navigation_stats();
        assert_eq!((after.history_len, after.state_bytes, after.url_generation, after.pending_requests, after.pending_tasks), (before.history_len, before.state_bytes, before.url_generation, 0, 0));
        assert_eq!(b.document_url(), url);
        assert_eq!(js(&mut b, "JSON.stringify(history.state)"), r#"{"keep":1}"#);
        assert!(drain_requests(&mut b).is_empty(), "no navigation request, no network side effect");
        // a gated form that a browser accepts is Unsupported, never a quiet other URL
        assert_eq!(js(&mut b, "history.pushState(1, '', 'http://u:p@staging.example.test:8443/x')"), "unsupported:user info in a URL");
        assert_eq!(js(&mut b, "history.pushState(1, '', 'http:\\\\\\\\x')"), "unsupported:backslash in a URL");
        assert_eq!(b.navigation_stats().history_len, before.history_len);
    });
}

#[test]
fn history_argument_order_and_reentrancy() {
    big(|| {
        let mut b = at(URL0);
        // title, then url, then the state getters; a DataCloneError wins over a bad URL
        assert_eq!(
            js(&mut b, "var log = []; var st = {get a() { log.push('get-a'); return 1; }, get b() { log.push('get-b'); return 2; }};
                history.pushState(st, {toString: function () { log.push('title'); return ''; }}, {toString: function () { log.push('url'); return '/o'; }}); log.join()"),
            "title,url,get-a,get-b"
        );
        assert_eq!(js(&mut b, "log.length = 0; try { history.pushState(function () {}, '', 'https://evil.test/') } catch (e) { e.name }"), "DataCloneError");
        assert_eq!(js(&mut b, "try { history.pushState(1) } catch (e) { e.name + ':' + e.message }"), "TypeError:Failed to execute 'pushState' on 'History': 2 arguments required, but only 1 present.");
        assert_eq!(js(&mut b, "try { history.replaceState() } catch (e) { e.name }"), "TypeError");
        // a getter that changes the history while the state is cloned: no borrow is held, the result is consistent
        let n = b.navigation_stats().history_len;
        assert_eq!(js(&mut b, "history.pushState({get x() { history.pushState('inner', '', '/inner'); return 1; }}, '', '/outer'); [history.length, location.pathname, JSON.stringify(history.state)].join()"), format!("{},/outer,{{\"x\":1}}", n + 2));
        // a title whose toString throws: nothing happens
        let n = b.navigation_stats();
        assert_eq!(js(&mut b, "try { history.pushState(1, {toString: function () { throw new RangeError('t'); }}, '/z') } catch (e) { e.name }"), "RangeError");
        assert_eq!(b.navigation_stats().history_len, n.history_len);
        // a state getter that throws: nothing happens, the error is the script's
        assert_eq!(js(&mut b, "try { history.pushState({get x() { throw new TypeError('g'); }}, '', '/z') } catch (e) { e.message }"), "g");
        assert_eq!(b.navigation_stats().state_bytes, n.state_bytes);
        // location setters coerce before they look at the URL
        assert_eq!(js(&mut b, "location.assign({toString: function () { location.assign('/first'); return '/second'; }}); 1"), "1");
        let r: Vec<String> = drain_requests(&mut b).into_iter().map(|r| r.url).collect();
        assert_eq!(r, ["https://staging.example.test:8443/first", "https://staging.example.test:8443/second"]);
    });
}

#[test]
fn saved_history_methods_and_reassignment() {
    big(|| {
        let mut b = at(URL0);
        assert_eq!(js(&mut b, "var push = history.pushState, rep = history.replaceState; push.call(history, 1, '', '/a'); rep.call(history, 2, '', '/b'); location.pathname + history.state"), "/b2");
        assert_eq!(js(&mut b, "try { push(1, '') } catch (e) { e.name + ':' + e.message }"), "TypeError:Illegal invocation");
        assert_eq!(js(&mut b, "try { push.call({}, 1, '') } catch (e) { e.name }"), "TypeError");
        // reassigning on the instance (Next does) works and does not touch the prototype
        assert_eq!(js(&mut b, "var seen = []; history.pushState = function (s, t, u) { seen.push(u); return push.call(history, s, t, u); }; history.pushState({}, '', '/w'); [seen.join(), location.pathname, History.prototype.pushState === push, Object.prototype.hasOwnProperty.call(history, 'pushState')].join()"), "/w,/w,true,true");
        let g = js(&mut b, "var d = Object.getOwnPropertyDescriptor(History.prototype, 'length'); d.get.call(history)");
        assert!(g.parse::<usize>().unwrap() >= 3, "{g}");
        assert_eq!(js(&mut b, "d.get.call({})"), "throw:TypeError:Illegal invocation");
        assert_eq!(js(&mut b, "typeof History + typeof Location + typeof Storage + typeof URL + typeof URLSearchParams"), "functionfunctionfunctionfunctionfunction");
    });
}

#[test]
fn history_is_bounded() {
    big(|| {
        let mut b = at("https://a.test/");
        b.set_navigation_limits(NavLimits { max_history_entries: 4, ..Default::default() });
        js(&mut b, "for (var i = 1; i <= 10; i++) history.pushState({i: i}, '', '/' + i);");
        let st = b.navigation_stats();
        assert_eq!((st.history_len, st.history_index), (4, 3));
        assert_eq!(js(&mut b, "history.length + ',' + history.state.i"), "4,10");
        js(&mut b, "history.go(-3)");
        b.run_navigation_tasks().unwrap();
        assert_eq!(js(&mut b, "location.pathname + history.state.i"), "/77", "the oldest entries dropped");
        js(&mut b, "history.go(-1)");
        b.run_navigation_tasks().unwrap();
        assert_eq!(js(&mut b, "location.pathname"), "/7");
        // state size caps
        let mut b = at("https://a.test/");
        b.set_navigation_limits(NavLimits { max_state_bytes: 1000, max_total_state_bytes: 2400, ..Default::default() });
        js(&mut b, "function rep(s, n) { var o = ''; while (n-- > 0) o += s; return o; }");
        assert_eq!(js(&mut b, "history.pushState(rep('x', 600), '')"), lim("history-state-bytes"));
        let mut c = at("https://a.test/");
        c.set_navigation_limits(NavLimits { max_state_depth: 5, max_state_nodes: 20, ..Default::default() });
        assert_eq!(js(&mut c, "var d = {}, c = d; for (var i = 0; i < 6; i++) { c.n = {}; c = c.n; } history.pushState(d, '')"), lim("history-state-depth"));
        assert_eq!(js(&mut c, "var big = []; for (var i = 0; i < 30; i++) big.push({}); history.pushState(big, '')"), lim("history-state-nodes"));
        assert_eq!(c.navigation_stats().history_len, 1);
        assert_eq!(b.navigation_stats().history_len, 1);
        js(&mut b, "history.pushState(rep('y', 400), '')");
        js(&mut b, "history.pushState(rep('y', 400), '')");
        let n = b.navigation_stats();
        assert_eq!(js(&mut b, "history.pushState(rep('y', 400), '')"), lim("history-state-total"), "{n:?}");
        // a replace frees the bytes of the entry it replaces
        assert_eq!(js(&mut b, "history.replaceState(rep('z', 400), ''); 1"), "1");
        assert_eq!(b.navigation_stats().history_len, 3);
    });
}

#[test]
fn history_state_and_pending_work_are_collected() {
    big(|| {
        let mut b = at("https://a.test/");
        js(&mut b, "var o = {}; o.self = o; o.kids = []; for (var i = 0; i < 100; i++) o.kids.push({i: i, up: o}); history.pushState(o, ''); o = null;");
        assert_eq!(js(&mut b, "history.state.kids.length"), "100");
        let c = b.collect(&[]).unwrap();
        assert_eq!(js(&mut b, "history.state.kids[99].up === history.state"), "true", "the rooted cache survives {c:?}");
        // while the entry lives its graph lives; replace it and the whole cycle goes
        let before = b.collect(&[]).unwrap().objects;
        assert_eq!(before, 0);
        js(&mut b, "history.replaceState(null, '')");
        let freed = b.collect(&[]).unwrap().objects;
        assert!(freed >= 101, "freed {freed}");
        // an unreferenced entry past the forward end drops with the push
        js(&mut b, "history.pushState({a: {b: {}}}, ''); history.state; history.back();");
        b.run_navigation_tasks().unwrap();
        js(&mut b, "history.pushState(1, '')");
        assert!(b.collect(&[]).unwrap().objects >= 3);
        assert_eq!(js(&mut b, "history.length + ',' + history.state"), "3,1");
    });
}

#[test]
fn history_state_survives_collect_between_calls() {
    big(|| {
        let mut b = at("https://a.test/");
        js(&mut b, "history.pushState({k: [1, 2, {z: 'q'}]}, '');");
        for _ in 0..3 {
            b.collect(&[]).unwrap();
        }
        assert_eq!(js(&mut b, "history.state.k[2].z"), "q");
        js(&mut b, "history.pushState(2, ''); history.back();");
        b.collect(&[]).unwrap();
        b.run_navigation_tasks().unwrap();
        b.collect(&[]).unwrap();
        assert_eq!(js(&mut b, "history.state.k[2].z"), "q");
    });
}

// ------------------------------------------------------------------------------------------ storage

#[test]
fn storage_basics_and_coercion() {
    big(|| {
        let mut b = at("https://a.test/");
        assert_eq!(js(&mut b, "[localStorage.length, localStorage.getItem('token'), localStorage.key(0), sessionStorage.length].join()"), "0,,,0");
        assert_eq!(js(&mut b, "localStorage.setItem('token', 'abc.def'); localStorage.setItem('theme', 'dark'); sessionStorage.setItem('token', 'sess');
            [localStorage.getItem('token'), sessionStorage.getItem('token'), localStorage.length, localStorage.key(0), localStorage.key(1), localStorage.key(2), localStorage.key(-1), localStorage.key(NaN), localStorage.key(1.9), localStorage.key('1')].join()"),
            "abc.def,sess,2,token,theme,,,token,theme,theme");
        // overwrite keeps the position; remove then add goes last
        js(&mut b, "localStorage.setItem('token', 'v2'); localStorage.removeItem('theme'); localStorage.setItem('theme', 'light'); localStorage.removeItem('absent')");
        assert_eq!(js(&mut b, "[localStorage.key(0), localStorage.getItem('token'), localStorage.key(1), localStorage.length].join()"), "token,v2,theme,2");
        // ToString on keys and values
        assert_eq!(js(&mut b, "localStorage.clear(); localStorage.setItem(1, 2); localStorage.setItem(null, undefined); localStorage.setItem({toString: function () { return 'obj'; }}, [1, 2]);
            [localStorage.getItem('1'), localStorage.getItem('null'), localStorage.getItem('obj'), localStorage.getItem(1), localStorage.length].join()"), "2,undefined,1,2,2,3");
        assert_eq!(js(&mut b, "sessionStorage.length + ',' + (localStorage.clear(), localStorage.length)"), "1,0");
        // argument counts and `this`
        assert_eq!(js(&mut b, "try { localStorage.getItem() } catch (e) { e.name + ':' + e.message }"), "TypeError:Failed to execute 'getItem' on 'Storage': 1 argument required, but only 0 present.");
        assert_eq!(js(&mut b, "try { localStorage.setItem('a') } catch (e) { e.name }"), "TypeError");
        assert_eq!(js(&mut b, "var get = localStorage.getItem; try { get('a') } catch (e) { e.name + ':' + e.message }"), "TypeError:Illegal invocation");
        assert_eq!(js(&mut b, "get.call(localStorage, 'nope')"), "null");
        assert_eq!(js(&mut b, "Object.getOwnPropertyDescriptor(Storage.prototype, 'length').get.call({})"), "throw:TypeError:Illegal invocation");
        // coercion that throws leaves the store alone
        assert_eq!(js(&mut b, "try { localStorage.setItem('k', {toString: function () { throw new RangeError('v'); }}) } catch (e) { e.name + localStorage.length }"), "RangeError0");
    });
}

#[test]
fn storage_keeps_utf16_and_is_not_a_property_bag() {
    big(|| {
        let mut b = at("https://a.test/");
        assert_eq!(js(&mut b, "localStorage.setItem('k\\ud83d\\ude00', 'v\\ud800'); localStorage.setItem('\\udc00', '\\u0000x'); var k = localStorage.key(0), v = localStorage.getItem(k);
            [k.length, k.charCodeAt(1), v.length, v.charCodeAt(1), localStorage.getItem('\\udc00').length, localStorage.getItem('\\udc00').charCodeAt(0), localStorage.getItem('k\\ud83d')].join()"),
            "3,55357,2,55296,2,0,");
        assert_eq!(b.storage().get_item(false, "https://a.test", "k\u{1F600}").as_deref(), Some("v\u{FFFD}"));
        // `localStorage.x = 1` is NOT storage access: it is dropped, and the store never sees it
        assert_eq!(js(&mut b, "localStorage.length = 99; localStorage.token = 'p'; var r = [localStorage.token, localStorage.length, localStorage.getItem('token'), Object.keys(localStorage).length].join(); r"), ",2,,0");
        assert_eq!(js(&mut b, "'use strict'; try { localStorage.token = 'p'; 'no error' } catch (e) { e.name }"), "TypeError");
    });
}

#[test]
fn storage_isolates_origins_and_sessions_and_survives_a_new_browser() {
    big(|| {
        let store = WebStorage::default();
        let mut a = at("https://a.test:8443/x");
        a.attach_storage(store.clone());
        js(&mut a, "localStorage.setItem('token', 'A'); sessionStorage.setItem('s', 'A')");
        // another origin (port, scheme, host) sees nothing; the same origin (other path, after pushState) sees it
        for other in ["https://a.test:8444/", "http://a.test:8443/", "https://b.test:8443/", "https://sub.a.test:8443/"] {
            let mut b = at(other);
            b.attach_storage(store.clone());
            assert_eq!(js(&mut b, "[localStorage.length, localStorage.getItem('token'), sessionStorage.getItem('s')].join()"), "0,,", "{other}");
            js(&mut b, "localStorage.setItem('token', 'other')");
        }
        assert_eq!(js(&mut a, "localStorage.getItem('token')"), "A");
        js(&mut a, "history.pushState(null, '', '/other/path?x=1#y')");
        assert_eq!(js(&mut a, "localStorage.getItem('token') + sessionStorage.getItem('s')"), "AA");
        assert_eq!(store.origin_count(false), 5);
        // a recreated browser with the same store is the same session: both areas stay
        drop(a);
        let mut again = at("https://a.test:8443/after-reload");
        again.attach_storage(store.clone());
        assert_eq!(js(&mut again, "localStorage.getItem('token') + sessionStorage.getItem('s')"), "AA");
        // a new tab shares localStorage, not sessionStorage
        let mut tab = at("https://a.test:8443/");
        tab.attach_storage(store.new_session());
        assert_eq!(js(&mut tab, "localStorage.getItem('token') + ',' + sessionStorage.getItem('s')"), "A,null");
        js(&mut tab, "localStorage.setItem('shared', 'yes'); sessionStorage.setItem('s', 'tab')");
        assert_eq!(js(&mut again, "localStorage.getItem('shared') + ',' + sessionStorage.getItem('s')"), "yes,A");
        // a browser without attach has its own store
        let mut lone = at("https://a.test:8443/");
        assert_eq!(js(&mut lone, "localStorage.length"), "0");
        // the driver view of the store
        assert_eq!(store.get_item(false, "https://a.test:8443", "token").as_deref(), Some("A"));
        assert_eq!(store.item_count(true, "https://a.test:8443"), 1);
    });
}

#[test]
fn storage_origin_follows_the_document_url_when_it_is_reconfigured() {
    big(|| {
        let mut b = at("https://a.test/");
        js(&mut b, "localStorage.setItem('k', 'a')");
        b.set_document_url("https://b.test/").unwrap();
        assert_eq!(js(&mut b, "localStorage.getItem('k')"), "null");
        b.set_document_url("https://a.test/again").unwrap();
        assert_eq!(js(&mut b, "localStorage.getItem('k')"), "a");
    });
}

#[test]
fn storage_quota_and_bounds() {
    big(|| {
        let store = WebStorage::new(StorageLimits { max_origins: 2, max_entries: 3, max_origin_units: 50, max_total_units: 70 });
        let mut b = at("https://a.test/");
        b.attach_storage(store.clone());
        js(&mut b, "function rep(s, n) { var o = ''; while (n-- > 0) o += s; return o; }");
        // over quota: a catchable QuotaExceededError, and the write did not happen
        assert_eq!(js(&mut b, "localStorage.setItem('k', rep('x', 40)); 1"), "1");
        assert_eq!(js(&mut b, "try { localStorage.setItem('big', rep('y', 20)) } catch (e) { e.name + ':' + e.code }"), "QuotaExceededError:22");
        assert_eq!(js(&mut b, "[localStorage.length, localStorage.getItem('big'), localStorage.getItem('k').length].join()"), "1,,40");
        // replacing a value counts only the difference; a smaller one frees room
        assert_eq!(js(&mut b, "localStorage.setItem('k', rep('x', 49)); localStorage.setItem('k', rep('x', 5)); localStorage.setItem('a', rep('a', 20)); localStorage.length"), "2");
        assert_eq!(js(&mut b, "try { localStorage.setItem('c', rep('c', 30)) } catch (e) { e.name }"), "QuotaExceededError");
        // the same value again is a no-op even at the edge
        assert_eq!(js(&mut b, "localStorage.setItem('a', rep('a', 20)); localStorage.length"), "2");
        // entries: a sandbox Limit
        assert_eq!(js(&mut b, "localStorage.setItem('c', '1'); localStorage.setItem('d', '2')"), lim("storage-entries"));
        assert_eq!(js(&mut b, "localStorage.length"), "3");
        // origins: a second origin is fine, a third is a Limit
        let mut c = at("https://b.test/");
        c.attach_storage(store.clone());
        assert_eq!(js(&mut c, "sessionStorage.setItem('s', '1'); localStorage.setItem('l', '1'); 1"), "1");
        let mut d = at("https://c.test/");
        d.attach_storage(store.clone());
        assert_eq!(js(&mut d, "localStorage.setItem('l', '1')"), lim("storage-origins"));
        assert_eq!(store.origin_count(false), 2);
        // total across origins
        js(&mut c, "function rep(s, n) { var o = ''; while (n-- > 0) o += s; return o; }");
        let before = store.total_units(false);
        assert!(before <= 70);
        assert_eq!(js(&mut c, "localStorage.setItem('big', rep('z', 45))"), lim("storage-total"));
        assert_eq!(store.total_units(false), before, "nothing was written");
        // removing the last item of an origin frees the origin slot
        js(&mut c, "localStorage.clear()");
        assert_eq!(store.origin_count(false), 1);
        assert_eq!(js(&mut d, "localStorage.setItem('l', '1'); localStorage.length"), "1");
        // one item cap
        let mut e = at("https://a.test/");
        e.set_navigation_limits(NavLimits { max_item_units: 10, ..Default::default() });
        assert_eq!(js(&mut e, "function rep(s, n) { var o = ''; while (n-- > 0) o += s; return o; } localStorage.setItem('k', rep('x', 11))"), lim("storage-item"));
    });
}

#[test]
fn storage_survives_collect_and_saved_references() {
    big(|| {
        let mut b = at("https://a.test/");
        js(&mut b, "var ls = localStorage, set = ls.setItem, get = ls.getItem; set.call(ls, 'k', 'v');");
        b.collect(&[]).unwrap();
        assert_eq!(js(&mut b, "get.call(localStorage, 'k') + ls.getItem('k') + (ls === localStorage)"), "vvtrue");
        assert_eq!(js(&mut b, "delete window.localStorage; typeof localStorage + ',' + ls.getItem('k')"), "undefined,v");
    });
}

#[test]
fn login_shaped_flow_with_synthetic_strings() {
    big(|| {
        let mut b = at("https://staging.example.test/login");
        // theme script of the page: reads storage in a try
        assert_eq!(js(&mut b, "var theme; try { theme = localStorage.getItem('theme') } catch (e) { theme = 'blocked' } String(theme)"), "null");
        // login: the handler stores a token, then the router pushes the dashboard route
        js(&mut b, "localStorage.setItem('token', 'synthetic-token-1'); history.pushState({__NA: true, tree: ['', {children: ['dashboard', {}]}]}, '', '/dashboard?tab=jobs#top');");
        assert_eq!(js(&mut b, "[location.pathname, location.search, location.hash, history.length, history.state.tree[1].children[0], localStorage.getItem('token')].join()"), "/dashboard,?tab=jobs,#top,2,dashboard,synthetic-token-1");
        // the search changes through the router: replaceState keeps the entry count
        js(&mut b, "history.replaceState(history.state, '', '/dashboard?tab=agents#top')");
        assert_eq!(js(&mut b, "[location.search, history.length].join()"), "?tab=agents,2");
        // reload keeps the token and asks the loader for the same URL
        js(&mut b, "location.reload()");
        let r = drain_requests(&mut b);
        assert_eq!(r, vec![req(NavigationKind::Reload, "https://staging.example.test/dashboard?tab=agents#top", "https://staging.example.test/dashboard?tab=agents#top")]);
        // the loader loads it: a new Browser over the same session sees the token and the stored URL
        let store = b.storage();
        let mut reloaded = at(&r[0].url);
        reloaded.attach_storage(store.clone());
        assert_eq!(js(&mut reloaded, "[localStorage.getItem('token'), history.length, history.state, location.pathname].join()"), "synthetic-token-1,1,,/dashboard");
        // sign out: remove the token, go to /login (a document load), and a 401 redirect does the same
        js(&mut reloaded, "localStorage.removeItem('token'); sessionStorage.clear(); window.location.href = '/login'");
        assert_eq!(drain_requests(&mut reloaded), vec![req(NavigationKind::Assign, "https://staging.example.test/login", "https://staging.example.test/dashboard?tab=agents#top")]);
        assert_eq!(js(&mut reloaded, "localStorage.getItem('token')"), "null");
        assert_eq!(store.item_count(false, "https://staging.example.test"), 0);
        // a cross-origin pushState during login is refused: no state, no request
        let n = reloaded.navigation_stats();
        assert_eq!(js(&mut reloaded, "try { history.pushState({t: 'x'}, '', 'https://login.other.test/cb?token=synthetic') } catch (e) { e.name }"), "SecurityError");
        assert_eq!(reloaded.navigation_stats().history_len, n.history_len);
        assert!(drain_requests(&mut reloaded).is_empty());
    });
}

// ------------------------------------------------------------------------------------------ URL and URLSearchParams

#[test]
fn url_components_and_live_search_params() {
    big(|| {
        let mut b = at(URL0);
        assert_eq!(js(&mut b, "var u = new URL('/p q?a=1&b=x+y&a=3#frag', 'https://h.test:8080/base/'); [u.href, u.origin, u.searchParams.get('b'), u.searchParams.getAll('a').join('/')].join(' ')"), "https://h.test:8080/p%20q?a=1&b=x+y&a=3#frag https://h.test:8080 x y 1/3");
        assert_eq!(js(&mut b, "u.searchParams.append('c', 'd e&f'); u.searchParams.delete('a'); [u.search, u.href].join(' ')"), "?b=x+y&c=d+e%26f https://h.test:8080/p%20q?b=x+y&c=d+e%26f#frag");
        assert_eq!(js(&mut b, "u.search = '?z=1&z=2'; u.searchParams.get('z') + u.searchParams.getAll('z').length + u.searchParams.size"), "122");
        assert_eq!(js(&mut b, "u.searchParams.delete('z'); [u.search === '', u.href.indexOf('?'), u.href].join()"), "true,-1,https://h.test:8080/p%20q#frag");
        assert_eq!(js(&mut b, "var sp = u.searchParams; u.href = 'http://other.test/?k=v'; [sp === u.searchParams, sp.get('k'), u.protocol].join()"), "true,v,http:");
        assert_eq!(js(&mut b, "u.hash = 'h h'; u.pathname = '/n m'; u.port = '81'; u.hostname = 'NEW.test'; u.protocol = 'https'; u.href"), "https://new.test:81/n%20m?k=v#h%20h");
        // relative reference against the document URL
        assert_eq!(js(&mut b, "new URL('../c?d', location.href).href + ' ' + new URL(location.href).hash"), "https://staging.example.test:8443/c?d #top");
        assert_eq!(js(&mut b, "new URL('x', 'nonsense')"), "throw:TypeError:Failed to construct 'URL': Invalid URL");
        assert_eq!(js(&mut b, "new URL('mailto:a@b.c')"), "unsupported:URL scheme other than http and https");
        assert_eq!(js(&mut b, "new URL('http://u:p@a.com')"), "unsupported:user info in a URL");
        assert_eq!(js(&mut b, "new URL('http://bücher.de')"), "unsupported:non-ASCII, escaped or punycode host (IDNA is gated)");
        assert_eq!(js(&mut b, "u.username = 'x'"), "unsupported:user info in a URL");
        assert_eq!(js(&mut b, "u.username = ''; u.password = ''; u.username + u.password + 'ok'"), "ok");
        assert_eq!(js(&mut b, "u.href = 'bad'"), "throw:TypeError:Failed to set the 'href' property on 'URL': Invalid URL");
        assert_eq!(js(&mut b, "u.href"), "https://new.test:81/n%20m?k=v#h%20h", "a failed set changes nothing");
        assert_eq!(js(&mut b, "typeof URL.createObjectURL + typeof URL.parse"), "undefinedundefined");
    });
}

#[test]
fn search_params_semantics() {
    big(|| {
        let mut b = at(URL0);
        assert_eq!(js(&mut b, "var p = new URLSearchParams('?a=1&b=2&a=3'); p.set('a', '9'); p.append('c', 'x y'); p.delete('b'); p.toString()"), "a=9&c=x+y");
        assert_eq!(js(&mut b, "p = new URLSearchParams([['x', '1'], ['y', '2']]); p.sort(); p.append('a', '0'); p.sort(); p + ''"), "a=0&x=1&y=2");
        assert_eq!(js(&mut b, "p = new URLSearchParams({q: 'a&b', r: 'é'}); p.toString()"), "q=a%26b&r=%C3%A9");
        assert_eq!(js(&mut b, "p = new URLSearchParams(new URLSearchParams('m=1')); p.append('n', '2'); p.toString()"), "m=1&n=2");
        // the entries are live iterator objects with next()
        assert_eq!(js(&mut b, "var it = p.entries(), r = it.next(), s = r.value.join('=') + r.done; r = it.next(); s += r.value.join('=') + r.done; r = it.next(); s + r.done + r.value"), "m=1falsen=2falsetrueundefined");
        assert_eq!(js(&mut b, "typeof p.keys().next + typeof p.values().next + typeof Object.getPrototypeOf(p.keys()).next"), "functionfunctionfunction");
        // an object without Symbol.iterator is a record (own enumerable keys); a pair must be iterable
        assert_eq!(js(&mut b, "new URLSearchParams(new (function F() { this.k = 1; })()) + ''"), "k=1");
        assert_eq!(js(&mut b, "new URLSearchParams([{0: 'a', 1: 'b', length: 2}])"), "throw:TypeError:Failed to construct 'URLSearchParams': The provided value cannot be converted to a sequence.");
        assert_eq!(js(&mut b, "new URLSearchParams('a=1').constructor === URLSearchParams"), "true");
        assert_eq!(js(&mut b, "new URLSearchParams(null).toString()"), "null=", "WebIDL: null is the string 'null' (Node returns '')");
        // forEach: a callback that mutates and throws; the list stays consistent
        assert_eq!(js(&mut b, "p = new URLSearchParams('a=1&b=2'); var seen = []; try { p.forEach(function (v, k, o) { seen.push(k); if (k == 'a') o.append('c', '3'); if (k == 'c') throw new RangeError('stop'); }) } catch (e) { seen.push(e.name) } seen.join() + '|' + p"), "a,b,c,RangeError|a=1&b=2&c=3");
        // coercion that mutates the list: no borrow is held
        assert_eq!(js(&mut b, "p = new URLSearchParams('a=1&b=2'); p.append({toString: function () { p.delete('a'); return 'k'; }}, {toString: function () { p.append('inner', '1'); return 'v'; }}); p + ''"), "b=2&inner=1&k=v");
        assert_eq!(js(&mut b, "p = new URLSearchParams('a=1'); var o = {get x() { p.append('got', '1'); return 'y'; }}; var q = new URLSearchParams(o); p + ',' + q"), "a=1&got=1,x=y");
        // lone surrogates become U+FFFD (USVString)
        assert_eq!(js(&mut b, "p = new URLSearchParams(); p.append('k\\ud800', 'v'); p.toString()"), "k%EF%BF%BD=v");
        // saved method references and `this`
        assert_eq!(js(&mut b, "p = new URLSearchParams('a=1'); var get = p.get, app = URLSearchParams.prototype.append; app.call(p, 'b', '2'); [get.call(p, 'b'), (function () { try { get('a') } catch (e) { return e.name } })()].join()"), "2,TypeError");
        assert_eq!(js(&mut b, "try { URLSearchParams.prototype.keys.call({}) } catch (e) { e.name }"), "TypeError");
    });
}

#[test]
fn url_objects_are_collected_as_cycles_and_slots_do_not_alias() {
    big(|| {
        let mut b = at(URL0);
        js(&mut b, "function make(i) { var u = new URL('http://h.test/p?i=' + i); u.searchParams.append('n', '' + i); var it = u.searchParams.entries(); it.next(); return u; }
            var keep = make(0); var held = keep.searchParams;");
        assert_eq!(b.navigation_stats().urls, 1);
        for round in 0..20 {
            js(&mut b, &format!("for (var i = 1; i <= 50; i++) make({round} * 100 + i); 1"));
            let c = b.collect(&[]).unwrap();
            let st = b.navigation_stats();
            assert_eq!((st.urls, st.params, st.iterators), (1, 1, 0), "round {round}: {c:?}");
        }
        assert_eq!(b.navigation_stats().param_bytes, 2 * (32 + 1 + 1), "param bytes follow the live lists");
        // after many reused slots the survivor is intact and new objects do not alias old ones
        assert_eq!(js(&mut b, "keep.href + ' ' + (held === keep.searchParams) + ' ' + make(7).href"), "http://h.test/p?i=0&n=0 true http://h.test/p?i=7&n=7");
        // a held searchParams keeps its URL alive (the cycle is one unit), and releasing both frees both
        js(&mut b, "var only = make(5).searchParams;");
        b.collect(&[]).unwrap();
        assert_eq!(js(&mut b, "only.get('i') + ',' + only.toString()"), "5,i=5&n=5");
        assert_eq!(b.navigation_stats().urls, 2);
        js(&mut b, "only = null; keep = null; held = null;");
        b.collect(&[]).unwrap();
        let st = b.navigation_stats();
        assert_eq!((st.urls, st.params, st.iterators, st.param_bytes), (0, 0, 0, 0));
    });
}

#[test]
fn colliding_slots_between_scripts_dom_nodes_and_navigation_objects() {
    big(|| {
        let mut b = at(URL0);
        // script 1 makes URL objects and drops them; script 2 makes DOM wrappers that may reuse their slots
        js(&mut b, "for (var i = 0; i < 40; i++) { var u = new URL('http://h.test/' + i); u.searchParams.append('k', 'v'); }");
        js(&mut b, "u = null");
        b.collect(&[]).unwrap();
        js(&mut b, "var nodes = []; for (var i = 0; i < 80; i++) nodes.push(document.createElement('div'));");
        js(&mut b, "var ug = Object.getOwnPropertyDescriptor(URL.prototype, 'href').get, ng = Object.getOwnPropertyDescriptor(Node.prototype, 'nodeType').get;");
        assert_eq!(js(&mut b, "var bad = 0; for (var i = 0; i < nodes.length; i++) { try { ug.call(nodes[i]); bad++ } catch (e) {} } bad"), "0");
        assert_eq!(js(&mut b, "var u2 = new URL('http://z.test/'), bad = 0; try { ng.call(u2); bad++ } catch (e) {} try { ng.call(u2.searchParams); bad++ } catch (e) {} try { ug.call(u2.searchParams); bad++ } catch (e) {} bad"), "0");
        assert_eq!(js(&mut b, "try { history.pushState({n: nodes[3]}, '') } catch (e) { e.name }"), "DataCloneError");
        assert_eq!(js(&mut b, "try { history.pushState({u: u2}, '') } catch (e) { e.name }"), "DataCloneError");
        // state graphs and DOM wrappers share the heap: collect between calls keeps both consistent
        js(&mut b, "history.pushState({a: [1, 2, 3]}, ''); nodes.length = 0; nodes = null;");
        b.collect(&[]).unwrap();
        js(&mut b, "var more = []; for (var i = 0; i < 80; i++) more.push({i: i}); history.replaceState(history.state, '');");
        b.collect(&[]).unwrap();
        assert_eq!(js(&mut b, "history.state.a.join() + more.length + u2.href"), "1,2,380http://z.test/");
    });
}

#[test]
fn url_limits() {
    big(|| {
        let mut b = at(URL0);
        b.set_navigation_limits(NavLimits { max_objects: 6, max_params: 3, max_param_bytes: 400, max_url_bytes: 64, ..Default::default() });
        assert_eq!(js(&mut b, "var us = []; for (var i = 0; i < 10; i++) us.push(new URL('http://h.test/' + i)); 1"), lim("url-objects"));
        assert_eq!(b.navigation_stats().urls, 6);
        let mut b = at(URL0);
        b.set_navigation_limits(NavLimits { max_params: 3, max_param_bytes: 250, max_url_bytes: 64, ..Default::default() });
        assert_eq!(js(&mut b, "var p = new URLSearchParams('a=1&b=2&c=3'); p.append('d', '4')"), lim("url-params"));
        assert_eq!(js(&mut b, "p.toString()"), "a=1&b=2&c=3", "a refused append changes nothing");
        assert_eq!(js(&mut b, "new URLSearchParams('a=1&b=2&c=3&d=4')"), lim("url-params"));
        let mut c = at(URL0);
        c.set_navigation_limits(NavLimits { max_param_bytes: 250, ..Default::default() });
        assert_eq!(js(&mut c, "var q = new URLSearchParams('x=1'); q.append('k', new Array(200).join('z')); 1"), lim("url-params"), "bytes cap");
        assert_eq!(js(&mut c, "q.toString()"), "x=1");
        assert_eq!(js(&mut b, "new URL('http://h.test/' + new Array(80).join('x'))"), lim("url-string"));
        assert_eq!(js(&mut b, "var u = new URL('http://h.test/'); u.search = '?' + new Array(70).join('q')"), lim("url-string"));
        assert_eq!(js(&mut b, "u.href"), "http://h.test/");
        // the URL a params list writes into is bounded too
        assert_eq!(js(&mut b, "u.searchParams.append('k', new Array(60).join('w'))"), lim("url-string"));
        assert_eq!(js(&mut b, "u.search + '|' + u.searchParams"), "|", "the failed write left the URL and its params as they were");
    });
}

#[test]
fn many_wrappers_cost_steps_and_a_step_limit_stops_the_script() {
    big(|| {
        let mut b = mk(Some(URL0), EvalLimits { max_steps: 3000, ..Default::default() });
        let r = js(&mut b, "var p = new URLSearchParams('a=1'); for (var i = 0; i < 100000; i++) p.append('k' + i, 'v'); 1");
        assert_eq!(r, "limit:Steps");
        let n = b.navigation_stats().param_bytes;
        // the script is over, and later scripts run with the state as it was left
        assert_eq!(js(&mut b, "var q = new URLSearchParams('x=1'); q.get('x')"), "1");
        assert!(n > 0);
    });
}


#[test]
fn navigation_pump_checks_failed_empty_checkpoint() {
    big(|| {
        let mut b = at(URL0);
        js(&mut b, "var called = 0; window.addEventListener('popstate', function(){ called++; }); history.pushState(null, '', '/new'); history.back(); Promise.resolve().then(function(){ while(true) {} });");
        assert!(matches!(b.drain(), Err(Error::Limit(_))));
        assert!(matches!(b.run_navigation_tasks(), Err(Error::Limit(_))));
        assert_eq!(js(&mut b, "called"), "0");
        assert_eq!(b.navigation_stats().pending_tasks, 1);
    });
}

#[test]
fn navigation_pump_runs_microtasks_between_tasks() {
    big(|| {
        let mut b = at(URL0);
        js(&mut b, "var order = ''; window.addEventListener('popstate', function(){ order += 'p'; Promise.resolve().then(function(){ order += 'm'; }); }); history.pushState(null, '', '/one'); history.pushState(null, '', '/two'); history.back(); history.back();");
        assert_eq!(b.run_navigation_tasks().unwrap(), 2);
        assert_eq!(js(&mut b, "order"), "pmpm");
    });
}

#[test]
fn history_go_uses_webidl_signed_long_conversion() {
    big(|| {
        let mut b = at(URL0);
        js(&mut b, "history.pushState(null, '', '/one'); history.back();");
        b.run_navigation_tasks().unwrap();
        js(&mut b, "history.go(4294967297);");
        b.run_navigation_tasks().unwrap();
        assert_eq!(js(&mut b, "location.pathname"), "/one");
    });
}

#[test]
fn history_clone_depth_has_a_hard_ceiling() {
    big(|| {
        let mut b = at(URL0);
        b.set_navigation_limits(NavLimits { max_state_depth: usize::MAX, ..Default::default() });
        assert_eq!(js(&mut b, "var head = {}, tail = head; for(var i=0;i<200;i++){ tail.next = {}; tail = tail.next; } history.pushState(head, '');"), lim("history-state-depth"));
    });
}

#[test]
fn fetch_resolves_against_immediate_history_updates() {
    big(|| {
        let mut b = at(URL0);
        b.configure_fetch(crate::browser_fetch::FetchConfig { base_url: Some(URL0.into()), client: bare_net::http::Client::new(), limits: Default::default() }).unwrap();
        assert_eq!(js(&mut b, "history.pushState(null, '', '/changed/page'); new Request('child').url"), "https://staging.example.test:8443/changed/child");
        assert_eq!(b.pending_fetches(), 0);
    });
}

#[test]
fn params_set_checks_total_bytes_before_replacing() {
    big(|| {
        let mut b = at(URL0);
        b.set_navigation_limits(NavLimits { max_param_bytes: 40, ..Default::default() });
        assert_eq!(js(&mut b, "var params = new URLSearchParams('x=1'); params.set('x', '0123456789');"), lim("url-params"));
        assert_eq!(js(&mut b, "params.get('x')"), "1");
        assert_eq!(b.navigation_stats().param_bytes, 34);
    });
}

#[test]
fn fetch_navigation_storage_and_timer_share_one_realm() {
    big(|| {
        use crate::browser_fetch_tests::{serve, ok};
        let srv = serve(|_| ok("{}"));
        let base = format!("http://127.0.0.1:{}/old/page", srv.port);
        let mut b = at(&base);
        b.use_virtual_clock_at(1000.0).unwrap();
        b.configure_fetch(crate::browser_fetch::FetchConfig { base_url: Some(base), client: bare_net::http::Client::new(), limits: Default::default() }).unwrap();
        js(&mut b, "sessionStorage.setItem('key', 'test-only'); var result = ''; history.pushState({n:1}, '', '/new/page'); fetch('child').then(function(r){ return r.json(); }).then(function(){ setTimeout(function(){ result = sessionStorage.getItem('key') + history.state.n; }, 0); });");
        b.collect(&[]).unwrap();
        assert_eq!(b.run_network_tasks(2).unwrap(), 1);
        b.advance_time(1.0, 10).unwrap();
        assert_eq!(js(&mut b, "result"), "test-only1");
        assert_eq!(srv.recs()[0].path, "/new/child");
    });
}

#[test]
fn async_class_login_shaped_flow_crosses_fetch_history_and_collection() {
    big(|| {
        use crate::browser_fetch_tests::{serve, ok};
        let srv = serve(|r| {
            assert_eq!(r.method, "POST");
            assert_eq!(r.path, "/api/session");
            assert_eq!(r.body, br#"{"label":"synthetic"}"#);
            ok("{\"ok\":true}")
        });
        let base = format!("http://127.0.0.1:{}/login", srv.port);
        let mut b = at(&base);
        b.use_virtual_clock_at(1000.0).unwrap();
        b.configure_fetch(crate::browser_fetch::FetchConfig { base_url: Some(base), client: bare_net::http::Client::new(), limits: Default::default() }).unwrap();
        assert_eq!(js(&mut b, "class SessionFlow { #label = 'synthetic'; async enter(){ var reply = await fetch('/api/session', {method:'POST', headers:{'content-type':'application/json'}, body:JSON.stringify({label:this.#label})}); var data = await reply.json(); if(data.ok){ sessionStorage.setItem('state', this.#label); history.pushState({label:this.#label}, '', '/dashboard'); } return new TextDecoder().decode(new TextEncoder().encode(this.#label)); } }"), "undefined");
        assert_eq!(js(&mut b, "var flow = new SessionFlow(); var result = ''; flow.enter().then(function(value){ setTimeout(function(){ result = value + ':' + history.state.label; }, 0); });"), "object");
        b.collect(&[]).unwrap();
        assert_eq!(b.run_network_tasks(4).unwrap(), 1);
        b.advance_time(1.0, 10).unwrap();
        b.collect(&[]).unwrap();
        assert_eq!(js(&mut b, "[result, location.pathname, sessionStorage.getItem('state')].join()"), "synthetic:synthetic,/dashboard,synthetic");
        assert_eq!(b.pending_fetches(), 0);
    });
}

/// A byte view whose prototype is `Object.prototype` (a script cannot make one yet: there is no `setPrototypeOf`).
fn plain_view(b: &mut Browser, len: usize) {
    let buf = b.it.heap.new_buffer(None, len).unwrap();
    let op = b.it.object_prototype();
    let v = b.it.heap.new_view(Some(op), buf, 0, len).unwrap();
    let k = b.it.heap.key_from_str("pv").unwrap();
    let g = b.it.global_object();
    b.it.define_data(g, k, Value::Object(v), true, true, true).unwrap();
}

#[test]
fn search_params_iterate_through_the_shared_engine() {
    big(|| {
        let mut b = at(URL0);
        // `[Symbol.iterator]` is `entries`, with the realm's own symbol: it survives a script that drops the global
        assert_eq!(js(&mut b, "var S = Symbol.iterator, P = URLSearchParams.prototype, d = Object.getOwnPropertyDescriptor(P, S); [d.value === P.entries, d.writable, d.enumerable, d.configurable, P.entries.name, P[S].length].join()"), "true,true,false,true,entries,0");
        assert_eq!(js(&mut b, "Symbol = undefined; var p = new URLSearchParams('a=1&b=2'); [[...p].join('|'), new URLSearchParams(new Map([['x', 'y']])) + '', p[S] === P.entries].join(' ')"), "a,1|b,2 x=y true");
        b.collect(&[]).unwrap();
        assert_eq!(js(&mut b, "var it = p[S](); [it[S]() === it, Object.getPrototypeOf(Object.getPrototypeOf(it)) === Object.getPrototypeOf(Object.getPrototypeOf([][S]())), [...it].length].join()"), "true,true,2");
        // generic iterables, pairs with coercion, live mutation (the same cases run against Node: tools/js/navigation_check.sh)
        assert_eq!(js(&mut b, "for (var [k, v] of p) { if (k === 'a') { p.delete('a'); p.append('c', '3'); } } p + ''"), "b=2&c=3");
        assert_eq!(js(&mut b, "new URLSearchParams([new Set(['a', {toString() { return 'b'; }}])]) + ''"), "a=b");
        assert_eq!(js(&mut b, "new URLSearchParams([['a', 'b', 'c']])"), "throw:TypeError:Failed to construct 'URLSearchParams': Each pair must have exactly 2 elements.");
        assert_eq!(js(&mut b, "new URLSearchParams(['ab'])"), "throw:TypeError:Failed to construct 'URLSearchParams': The provided value cannot be converted to a sequence.");
        assert_eq!(js(&mut b, "new URLSearchParams({[S]: 1})"), "throw:TypeError:Symbol.iterator is not a function");
        assert_eq!(js(&mut b, "new URLSearchParams({[Symbol]: 1}) + ''"), "undefined=1", "a plain string key");
        // an iterator that ends the sequence early or throws: `return` runs for ordinary errors only
        assert_eq!(js(&mut b, "var closed = 0, inner = 0; var endless = {[S]() { return {next() { return {done: false, value: new Set(['a', 'b', 'c'])}; }, return() { closed++; return {}; }}; }}; try { new URLSearchParams(endless) } catch (e) { e.name + closed }"), "TypeError1");
        assert_eq!(js(&mut b, "var pair = {[S]() { var n = 0; return {next() { return {done: false, value: 'v' + n++}; }, return() { inner++; return {}; }}; }}; closed = 0; try { new URLSearchParams({[S]() { return {next() { return {done: false, value: pair}; }, return() { closed++; return {}; }}; }}) } catch (e) { e.name + closed + inner }"), "TypeError11", "a third value is a TypeError, never an endless read");
        // a resource limit is final: no `return`, no more JS
        b.set_navigation_limits(NavLimits { max_params: 3, ..Default::default() });
        assert_eq!(js(&mut b, "closed = 0; var n = 0; new URLSearchParams({[S]() { return {next() { n++; return {done: false, value: ['a', 'b']}; }, return() { closed++; return {}; }}; }})"), lim("url-params"));
        assert_eq!(js(&mut b, "closed + ',' + n"), "0,4", "the cap is checked before the pair that would pass it is read");
        assert_eq!(js(&mut b, "new URLSearchParams('a=1&b=2&c=3&d=4')"), lim("url-params"));
        b.set_navigation_limits(NavLimits { max_param_bytes: 200, ..Default::default() });
        assert_eq!(js(&mut b, "var big = []; for (var i = 0; i < 5; i++) big.push(['k' + i, 'v'.repeat ? 'v'.repeat(30) : new Array(31).join('v')]); new URLSearchParams(big)"), lim("url-params"));
    });
}

#[test]
fn a_step_limit_ends_an_endless_iterable_without_more_js() {
    big(|| {
        let mut b = mk(Some(URL0), EvalLimits { max_steps: 20_000, ..Default::default() });
        let r = js(&mut b, "var closed = 0; new URLSearchParams({[Symbol.iterator]() { return {next() { return {done: false, value: ['a', 'b']}; }, return() { closed++; return {}; }}; }})");
        assert!(r == lim("url-params") || r == "limit:Steps", "{r}");
        assert_eq!(js(&mut b, "closed"), "0");
    });
}

#[test]
fn iterators_survive_collection_and_cycles_are_freed() {
    big(|| {
        let mut b = at(URL0);
        // an iterator that only the script holds keeps its params alive (the params are not otherwise reachable)
        js(&mut b, "var it = new URLSearchParams('a=1&b=2').entries(); it.next();");
        b.collect(&[]).unwrap();
        let st = b.navigation_stats();
        assert_eq!((st.params, st.iterators), (1, 1));
        assert_eq!(js(&mut b, "JSON.stringify([it.next(), it.next()])"), r#"[{"value":["b","2"],"done":false},{"done":true}]"#);
        // iterators in a loop are freed after the loop; so are the params they held
        js(&mut b, "it = null; for (var i = 0; i < 100; i++) { var q = new URLSearchParams([['k' + i, 'v']]); var i2 = q[Symbol.iterator](); i2.next(); } q = i2 = null;");
        b.collect(&[]).unwrap();
        let st = b.navigation_stats();
        assert_eq!((st.params, st.iterators, st.param_bytes), (0, 0, 0), "{st:?}");
        // a self cycle: the iterable holds the params, the params' iterator holds them, the object holds itself
        js(&mut b, "var o = {}; o.me = o; o.p = new URLSearchParams('a=1'); o[Symbol.iterator] = function () { return o.p.entries(); }; var made = new URLSearchParams(o) + '';");
        assert_eq!(js(&mut b, "made"), "a=1");
        js(&mut b, "o = null; made = null;");
        b.collect(&[]).unwrap();
        let st = b.navigation_stats();
        assert_eq!((st.params, st.iterators), (0, 0), "{st:?}");
        // a script-side iterator that is collected in the middle of a read: its state lives in the realm, the host holds nothing
        js(&mut b, "var steps = 0; var src = {[Symbol.iterator]() { var i = 0; return {next() { steps++; return i++ < 3 ? {done: false, value: ['k' + i, 'v']} : {done: true}; }}; }};");
        b.collect(&[]).unwrap();
        assert_eq!(js(&mut b, "new URLSearchParams(src) + '|' + steps"), "k1=v&k2=v&k3=v|4");
    });
}

#[test]
fn callbacks_from_other_scripts_with_the_same_ids_are_not_mixed_up() {
    big(|| {
        let mut b = at(URL0);
        js(&mut b, "var A = {[Symbol.iterator]() { var i = 0; return {next() { return i++ < 2 ? {done: false, value: ['a' + i, '1']} : {done: true}; }}; }};");
        js(&mut b, "var B = {[Symbol.iterator]() { var i = 0; return {next() { return i++ < 2 ? {done: false, value: ['b' + i, '2']} : {done: true}; }}; }};");
        b.collect(&[]).unwrap();
        js(&mut b, "var C = {[Symbol.iterator]() { var i = 0; return {next() { return i++ < 1 ? {done: false, value: ['c', '3']} : {done: true}; }}; }};");
        assert_eq!(js(&mut b, "new URLSearchParams(B) + '|' + new URLSearchParams(A) + '|' + new URLSearchParams(C) + '|' + new URLSearchParams(B)"), "b1=2&b2=2|a1=1&a2=1|c=3|b1=2&b2=2");
        // a method from one script used as the iterator of an object made by another
        js(&mut b, "var m = A[Symbol.iterator]; var D = {}; D[Symbol.iterator] = m;");
        assert_eq!(js(&mut b, "new URLSearchParams(D) + ''"), "a1=1&a2=1");
    });
}

#[test]
fn byte_views_are_refused_before_their_keys_are_listed() {
    big(|| {
        let mut b = at(URL0);
        b.set_navigation_limits(NavLimits { max_params: 100, ..Default::default() });
        // `u[Symbol.iterator] = undefined` sends a view down the record path, which lists one key for each byte
        assert_eq!(js(&mut b, "var u = new Uint8Array(1 << 26); u[Symbol.iterator] = undefined; new URLSearchParams(u)"), lim("url-params"));
        plain_view(&mut b, 1 << 26);
        assert_eq!(js(&mut b, "new URLSearchParams(pv)"), lim("url-params"));
        assert_eq!(js(&mut b, "history.pushState(pv, '')"), lim("history-state-bytes"));
        assert_eq!(js(&mut b, "history.pushState({deep: pv}, '')"), lim("history-state-bytes"));
        // a small view is still data, and an enumerable symbol key is a TypeError, not a resource limit
        assert_eq!(js(&mut b, "var s = new Uint8Array(2); s[Symbol.iterator] = undefined; new URLSearchParams(s)"), "throw:TypeError:Cannot convert a Symbol value to a string");
    });
}
