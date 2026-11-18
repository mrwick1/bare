//! Tests of child pages (`browser_child.rs`) and of `RemoteWindow` (`browser_remote_window.rs`). Every page is loaded over loopback HTTP
//! from a throwaway server of this process. Two servers on two ports are two origins. A child writes to `localStorage` of its own
//! origin, and the test reads it through the session storage. Time is the virtual clock of the session. Each test runs on a big stack.

use crate::browser_loader::*;
use crate::browser_session::*;
use crate::resources::test_server::{Reply, Server, serve};
use bare_js::eval::EvalLimits;
use bare_js::heap::HeapLimits;
use std::time::Duration;

fn big<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(64 << 20).spawn(f).unwrap().join().unwrap()
}

type Route = (&'static str, String);

fn html(body: &str) -> String {
    body.to_string()
}

fn server(routes: impl Fn() -> Vec<Route> + Send + Sync + 'static) -> Server {
    serve(move |path| match routes().into_iter().find(|r| r.0 == path) {
        Some((_, body)) => Reply::new(200, Some("text/html; charset=utf-8"), body),
        None => Reply::new(404, Some("text/plain"), "no"),
    })
}

fn cfg(url: String) -> SessionConfig {
    let mut c = SessionConfig::new(&url);
    c.virtual_clock = true;
    c.eval = EvalLimits { max_steps: 5_000_000, ..Default::default() };
    c.page.quiet = Duration::from_millis(100);
    c
}

fn get(s: &Session, srv: &Server, key: &str) -> Option<String> {
    s.storage().get_item(false, &srv.url(""), key)
}

fn console(s: &mut Session) -> Vec<String> {
    let b = s.browser().unwrap();
    let recs = b.take_console_records();
    recs.iter().map(|r| b.format_console_record(r).unwrap()).collect()
}

fn run(url: String, tune: impl FnOnce(&mut SessionConfig)) -> (Session, SessionResult) {
    let mut c = cfg(url);
    tune(&mut c);
    let mut s = Session::new(c);
    let r = s.run();
    (s, r)
}

const LOG: &str = "function log(x){localStorage.setItem('log',(localStorage.getItem('log')||'')+x+',')}";

#[test]
fn a_cross_origin_iframe_loads_and_runs_as_its_own_page() {
    big(|| {
        let other = server(|| vec![("/child", html("<script>localStorage.setItem('ran', [location.pathname, window.parent !== window, window.top !== window, typeof window.parent.postMessage, String(window.frameElement)].join('|'))</script>"))]);
        let o = other.url("/child");
        let srv = server(move || vec![("/", html(&format!("<iframe id=f src='{o}'></iframe><p>parent</p>")))]);
        let (s, r) = run(srv.url("/"), |_| {});
        assert_eq!(r.stop, SessionStop::Page(Stop::Settled), "{r:?}");
        assert_eq!(get(&s, &other, "ran").as_deref(), Some("/child|true|true|function|null"));
        // the child has its own storage: the parent origin does not see the key
        assert_eq!(get(&s, &srv, "ran"), None);
        assert_eq!(other.hits("/child"), 1);
    });
}

#[test]
fn a_same_origin_iframe_with_a_src_is_a_child_page_with_a_null_content_document() {
    big(|| {
        let srv = server(|| {
            vec![
                ("/", html("<iframe id=f src='/child'></iframe><script>localStorage.setItem('doc', String(document.getElementById('f').contentDocument))</script>")),
                ("/child", html("<script>localStorage.setItem('child', 'ran')</script>")),
            ]
        });
        let (s, r) = run(srv.url("/"), |_| {});
        assert_eq!(r.stop, SessionStop::Page(Stop::Settled), "{r:?}");
        assert_eq!(get(&s, &srv, "child").as_deref(), Some("ran"));
        assert_eq!(get(&s, &srv, "doc").as_deref(), Some("null"));
    });
}

fn parent_removing(remove: bool) -> String {
    let js = if remove { "setTimeout(function(){ document.getElementById('f').remove() }, 30)" } else { "" };
    format!("<iframe id=f src='/child'></iframe><script>{js}</script>")
}

#[test]
fn removing_the_iframe_drops_its_child_page_and_its_timers() {
    big(|| {
        for (remove, late) in [(false, Some("1")), (true, None)] {
            let srv = server(move || vec![("/", html(&parent_removing(remove))), ("/child", html("<script>setTimeout(function(){localStorage.setItem('late','1')}, 80)</script>"))]);
            let (s, r) = run(srv.url("/"), |_| {});
            assert_eq!(r.stop, SessionStop::Page(Stop::Settled), "{r:?}");
            assert_eq!(get(&s, &srv, "late").as_deref(), late, "remove={remove}");
        }
    });
}

#[test]
fn a_navigation_of_the_parent_drops_all_its_children() {
    big(|| {
        let srv = server(|| {
            vec![
                ("/", html("<iframe src='/child'></iframe><script>setTimeout(function(){ location.assign('/b') }, 30)</script>")),
                ("/b", html("<p>b</p>")),
                ("/child", html("<script>localStorage.setItem('ran','1'); setTimeout(function(){localStorage.setItem('late','1')}, 80)</script>")),
            ]
        });
        let (s, r) = run(srv.url("/"), |_| {});
        assert_eq!(r.stop, SessionStop::Page(Stop::Settled), "{r:?}");
        assert_eq!(r.pages.len(), 2);
        assert_eq!(get(&s, &srv, "ran").as_deref(), Some("1"));
        assert_eq!(get(&s, &srv, "late"), None);
    });
}

#[test]
fn a_changed_src_navigates_the_child_and_drops_the_old_page() {
    big(|| {
        let srv = server(|| {
            vec![
                ("/", html("<iframe id=f src='/c1'></iframe><script>setTimeout(function(){ document.getElementById('f').src = '/c2' }, 30)</script>")),
                ("/c1", html("<script>localStorage.setItem('c1','1'); setTimeout(function(){localStorage.setItem('c1late','1')}, 80)</script>")),
                ("/c2", html("<script>localStorage.setItem('c2','1')</script>")),
            ]
        });
        let (s, r) = run(srv.url("/"), |_| {});
        assert_eq!(r.stop, SessionStop::Page(Stop::Settled), "{r:?}");
        assert_eq!((get(&s, &srv, "c1").as_deref(), get(&s, &srv, "c2").as_deref(), get(&s, &srv, "c1late")), (Some("1"), Some("1"), None));
    });
}

#[test]
fn the_location_setter_of_the_window_navigates_the_child() {
    big(|| {
        let srv = server(|| {
            vec![
                ("/", html("<iframe id=f src='/c1'></iframe><script>setTimeout(function(){ document.getElementById('f').contentWindow.location = '/c2' }, 30)</script>")),
                ("/c1", html("<script>localStorage.setItem('c1','1'); setTimeout(function(){localStorage.setItem('c1late','1')}, 80)</script>")),
                ("/c2", html("<script>localStorage.setItem('c2','1')</script>")),
            ]
        });
        let (s, r) = run(srv.url("/"), |_| {});
        assert_eq!(r.stop, SessionStop::Page(Stop::Settled), "{r:?}");
        assert_eq!((get(&s, &srv, "c1").as_deref(), get(&s, &srv, "c2").as_deref(), get(&s, &srv, "c1late")), (Some("1"), Some("1"), None));
    });
}

#[test]
fn the_number_of_child_pages_is_bounded_and_the_console_warns_once() {
    big(|| {
        let srv = server(|| {
            vec![
                ("/", html("<iframe src='/c1'></iframe><iframe src='/c2'></iframe><iframe src='/c3'></iframe>")),
                ("/c1", html("<script>localStorage.setItem('c1','1')</script>")),
                ("/c2", html("<script>localStorage.setItem('c2','1')</script>")),
                ("/c3", html("<script>localStorage.setItem('c3','1')</script>")),
            ]
        });
        let (mut s, r) = run(srv.url("/"), |c| c.browser.max_child_pages = 2);
        assert_eq!(r.stop, SessionStop::Page(Stop::Settled), "{r:?}");
        assert_eq!((get(&s, &srv, "c1").as_deref(), get(&s, &srv, "c2").as_deref(), get(&s, &srv, "c3")), (Some("1"), Some("1"), None));
        let warns: Vec<String> = console(&mut s).into_iter().filter(|l| l.contains("limit of child pages")).collect();
        assert_eq!(warns.len(), 1, "{warns:?}");
    });
}

#[test]
fn the_nesting_depth_of_child_pages_is_bounded() {
    big(|| {
        let srv = server(|| {
            vec![
                ("/", html("<iframe src='/d1'></iframe>")),
                ("/d1", html("<iframe src='/d2'></iframe><script>localStorage.setItem('d1','1')</script>")),
                ("/d2", html("<iframe src='/d3'></iframe><script>localStorage.setItem('d2','1')</script>")),
                ("/d3", html("<script>localStorage.setItem('d3','1')</script>")),
            ]
        });
        // a page can have its own frames (depth 2 is allowed), the third level is over the bound
        let (s, r) = run(srv.url("/"), |c| c.browser.max_child_depth = 2);
        assert_eq!(r.stop, SessionStop::Page(Stop::Settled), "{r:?}");
        assert_eq!((get(&s, &srv, "d1").as_deref(), get(&s, &srv, "d2").as_deref(), get(&s, &srv, "d3")), (Some("1"), Some("1"), None));
        let (s, _) = run(srv.url("/"), |_| {});
        assert_eq!(get(&s, &srv, "d3").as_deref(), Some("1"), "the default depth allows it");
    });
}

#[test]
fn the_iframe_load_comes_after_the_child_load_and_before_the_parent_load() {
    big(|| {
        let srv = server(|| {
            vec![
                ("/", html(&format!("<iframe id=f src='/child'></iframe><script>{LOG} document.getElementById('f').addEventListener('load',function(){{log('frame')}}); window.addEventListener('load',function(){{log('window')}})</script>"))),
                ("/child", html(&format!("<script>{LOG} window.addEventListener('load',function(){{log('child')}})</script>"))),
            ]
        });
        let (s, r) = run(srv.url("/"), |_| {});
        assert_eq!(r.stop, SessionStop::Page(Stop::Settled), "{r:?}");
        assert_eq!(get(&s, &srv, "log").as_deref(), Some("child,frame,window,"));
    });
}

#[test]
fn the_page_is_not_settled_while_a_child_has_a_timer_in_the_window() {
    big(|| {
        let srv = server(|| vec![("/", html("<iframe src='/child'></iframe>")), ("/child", html("<script>setTimeout(function(){localStorage.setItem('t','1')}, 50)</script>"))]);
        let (s, r) = run(srv.url("/"), |_| {});
        assert_eq!(r.stop, SessionStop::Page(Stop::Settled), "{r:?}");
        assert_eq!(get(&s, &srv, "t").as_deref(), Some("1"));
    });
}

#[test]
fn a_child_that_fails_to_load_fires_load_at_the_iframe_and_the_parent_settles() {
    big(|| {
        let srv = server(|| {
            vec![("/", html(&format!("<iframe id=f src='/missing'></iframe><script>{LOG} document.getElementById('f').addEventListener('load',function(){{log('frame')}}); window.addEventListener('load',function(){{log('window')}})</script>")))]
        });
        let (s, r) = run(srv.url("/"), |_| {});
        assert_eq!(r.stop, SessionStop::Page(Stop::Settled), "{r:?}");
        assert_eq!(get(&s, &srv, "log").as_deref(), Some("frame,window,"));
        assert!(r.pages[0].diagnostics.iter().any(|l| l.contains("frame") && l.contains("status 404")), "{:?}", r.pages[0].diagnostics);
    });
}

#[test]
fn the_remote_window_has_the_cross_origin_api_and_throws_security_error_for_the_rest() {
    big(|| {
        let other = server(|| {
            vec![("/child", html("<script>var n; try { parent.document } catch (e) { n = e.name } localStorage.setItem('c', [parent !== window, top !== window, parent === top, typeof parent.postMessage, n, String(window.frameElement), String(window.opener)].join('|'))</script>"))]
        });
        let o = other.url("/child");
        let srv = server(move || {
            vec![(
                "/",
                html(&format!(
                    "<iframe id=f src='{o}'></iframe><script>
                     var f = document.getElementById('f'), w = f.contentWindow, r = [];
                     r.push(typeof w.postMessage, w === f.contentWindow, f.contentDocument === null, w.window === w, w.self === w, String(w.opener), w.closed, w.length, w.top === window.top, w.parent === window);
                     try {{ w.document; r.push('no throw') }} catch (e) {{ r.push(e.name, e instanceof DOMException) }}
                     try {{ w.foo = 1; r.push('no throw') }} catch (e) {{ r.push('set:' + e.name) }}
                     try {{ 'document' in w; r.push('no throw') }} catch (e) {{ r.push('in:' + e.name) }}
                     w.blur(); w.focus(); w.postMessage('x', '*');
                     localStorage.setItem('p', r.join('|'))</script>"
                )),
            )]
        });
        let (s, r) = run(srv.url("/"), |_| {});
        assert_eq!(r.stop, SessionStop::Page(Stop::Settled), "{r:?}");
        assert_eq!(get(&s, &srv, "p").as_deref(), Some("function|true|true|true|true|null|false|0|true|true|SecurityError|true|set:SecurityError|in:SecurityError"));
        assert_eq!(get(&s, &other, "c").as_deref(), Some("true|true|true|function|SecurityError|null|null"));
    });
}

#[test]
fn a_blank_frame_still_shares_the_parent_realm() {
    big(|| {
        let srv = server(|| vec![("/", html("<iframe id=f></iframe><script>var w = document.getElementById('f').contentWindow; localStorage.setItem('b', [w.Array === Array, document.getElementById('f').contentDocument !== null].join('|'))</script>"))]);
        let (s, r) = run(srv.url("/"), |_| {});
        assert_eq!(r.stop, SessionStop::Page(Stop::Settled), "{r:?}");
        assert_eq!(get(&s, &srv, "b").as_deref(), Some("true|true"));
    });
}

#[test]
fn a_child_console_and_errors_reach_the_parent_with_the_child_url() {
    big(|| {
        let srv = server(|| vec![("/", html("<iframe src='/child'></iframe>")), ("/child", html("<script>console.log('hello from child'); console.error('boom'); throw new Error('uncaught in child')</script>"))]);
        let (mut s, r) = run(srv.url("/"), |_| {});
        assert_eq!(r.stop, SessionStop::Page(Stop::Settled), "{r:?}");
        let label = srv.url("/child");
        let lines = console(&mut s);
        assert!(lines.contains(&format!("{label}: hello from child")), "{lines:?}");
        assert!(lines.contains(&format!("{label}: boom")), "{lines:?}");
        assert!(r.pages[0].diagnostics.iter().any(|l| l.contains("frame") && l.contains("/child")), "{:?}", r.pages[0].diagnostics);
    });
}

#[test]
fn a_fatal_error_of_a_child_ends_only_that_child() {
    big(|| {
        let srv = server(|| {
            vec![
                ("/", html("<iframe src='/child'></iframe><script>setTimeout(function(){localStorage.setItem('alive','1')}, 60)</script>")),
                ("/child", html("<script>var a = []; for (var i = 0; i < 400000; i++) a.push({})</script>")),
            ]
        });
        let (s, r) = run(srv.url("/"), |c| c.heap = HeapLimits { max_objects: 120_000, ..Default::default() });
        assert_eq!(r.stop, SessionStop::Page(Stop::Settled), "{r:?}");
        assert_eq!(get(&s, &srv, "alive").as_deref(), Some("1"));
        assert!(r.pages[0].diagnostics.iter().any(|l| l.contains("frame") && l.contains("stopped")), "{:?}", r.pages[0].diagnostics);
    });
}

#[test]
fn the_order_of_child_rounds_is_fixed_under_the_virtual_clock() {
    big(|| {
        let srv = server(|| {
            vec![
                ("/", html("<iframe src='/a'></iframe><iframe src='/b'></iframe><script>setTimeout(function(){console.log('P')}, 15)</script>")),
                ("/a", html("<script>console.log('A0'); setTimeout(function(){console.log('A')}, 20)</script>")),
                ("/b", html("<script>console.log('B0'); setTimeout(function(){console.log('B')}, 10)</script>")),
            ]
        });
        let (a, b) = (srv.url("/a"), srv.url("/b"));
        let want = [format!("{a}: A0"), format!("{b}: B0"), format!("{b}: B"), "P".to_string(), format!("{a}: A")];
        for _ in 0..2 {
            let (mut s, r) = run(srv.url("/"), |_| {});
            assert_eq!(r.stop, SessionStop::Page(Stop::Settled), "{r:?}");
            assert_eq!(console(&mut s), want);
        }
    });
}
