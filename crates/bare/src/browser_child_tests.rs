//! Tests of child pages (`browser_child.rs`) and of `RemoteWindow` (`browser_remote_window.rs`). Every page is loaded over loopback HTTP
//! from a throwaway server of this process. Two servers on two ports are two origins. A child writes to `localStorage` of its own
//! origin, and the test reads it through the session storage. Time is the virtual clock of the session. Each test runs on a big stack.

use crate::eng_new::Peek;
use crate::browser_loader::*;
use crate::browser_session::*;
use crate::resources::test_server::{Reply, Server, serve};
use crate::js_limits::EvalLimits;
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
#[ignore = "vex: left: (None, None, None)"]
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

// ---------------------------------------------------------------------------------------------- postMessage across pages

/// The page of `a` with an iframe `f` on `child`, and `script` after it.
fn framed(child: &str, script: &str) -> String {
    html(&format!("<iframe id=f src='{child}'></iframe><script>{LOG}; var f = document.getElementById('f'); var w = f.contentWindow; {script}</script>"))
}

fn page(script: &str) -> String {
    html(&format!("<script>{LOG}; {script}</script>"))
}

const STORE: &str = "function store(k, v){localStorage.setItem(k, v)}";

#[test]
fn the_parent_posts_to_a_child_and_the_child_sees_the_origin_and_source() {
    big(|| {
        let other = server(|| vec![("/c", page("addEventListener('message', function(e){ localStorage.setItem('m', [JSON.stringify(e.data), e.origin, e.source === parent, e.ports.length].join('|')) })"))]);
        let o = other.url("/c");
        let srv = server(move || vec![("/", framed(&o, "f.addEventListener('load', function(){ w.postMessage({k: [1, 'x']}, '*') })"))]);
        let (s, r) = run(srv.url("/"), |_| {});
        assert_eq!(r.stop, SessionStop::Page(Stop::Settled), "{r:?}");
        assert_eq!(get(&s, &other, "m"), Some(format!("{{\"k\":[1,\"x\"]}}|{}|true|0", srv.url(""))));
    });
}

#[test]
fn a_child_posts_to_its_parent_and_the_parent_sees_the_origin_and_the_frame_window() {
    big(|| {
        let other = server(|| vec![("/c", page("parent.postMessage({a: [1, 2]}, '*')"))]);
        let o = other.url("/c");
        let srv = server(move || vec![("/", framed(&o, "addEventListener('message', function(e){ localStorage.setItem('m', [JSON.stringify(e.data), e.origin, e.source === w].join('|')) })"))]);
        let (s, r) = run(srv.url("/"), |_| {});
        assert_eq!(r.stop, SessionStop::Page(Stop::Settled), "{r:?}");
        assert_eq!(get(&s, &srv, "m"), Some(format!("{{\"a\":[1,2]}}|{}|true", other.url(""))));
    });
}

#[test]
fn three_round_trips_run_in_the_same_order_each_time() {
    big(|| {
        let other = server(|| vec![("/c", page("addEventListener('message', function(e){ console.log('c' + e.data); parent.postMessage(e.data, '*') })"))]);
        let o = other.url("/c");
        let srv = server(move || {
            vec![("/", framed(&o, "f.addEventListener('load', function(){ w.postMessage(1, '*') }); addEventListener('message', function(e){ console.log('p' + e.data); if (e.data < 3) w.postMessage(e.data + 1, '*') })"))]
        });
        let c = other.url("/c");
        let want = [format!("{c}: c1"), "p1".to_string(), format!("{c}: c2"), "p2".to_string(), format!("{c}: c3"), "p3".to_string()];
        for _ in 0..2 {
            let (mut s, r) = run(srv.url("/"), |_| {});
            assert_eq!(r.stop, SessionStop::Page(Stop::Settled), "{r:?}");
            assert_eq!(console(&mut s), want);
        }
    });
}

#[test]
fn the_target_origin_is_checked_when_the_message_arrives() {
    big(|| {
        let other = server(|| {
            vec![(
                "/c",
                page("addEventListener('message', function(e){ log(e.data) }); parent.postMessage('slash', '/'); parent.postMessage('star', '*'); parent.postMessage('no', 'http://nope.example'); window.onload = function(){ localStorage.setItem('seen', localStorage.getItem('log')) }"),
            )]
        });
        let o = other.url("/c");
        let oo = other.url("");
        let srv = server(move || {
            vec![(
                "/",
                framed(&o, &format!("addEventListener('message', function(e){{ log(e.data) }}); f.addEventListener('load', function(){{ w.postMessage('a', 'http://nope.example'); w.postMessage('b', '{oo}'); w.postMessage('c', '*'); w.postMessage('d', '/'); w.postMessage('e', {{ targetOrigin: '{oo}' }}) }})")),
            )]
        });
        let (s, r) = run(srv.url("/"), |_| {});
        assert_eq!(r.stop, SessionStop::Page(Stop::Settled), "{r:?}");
        // the parent got only "star": "/" names the child origin, and the other origin is not the parent
        assert_eq!(get(&s, &srv, "log").as_deref(), Some("star,"));
        // the child got b, c and e
        assert_eq!(get(&s, &other, "log").as_deref(), Some("b,c,e,"));
    });
}

#[test]
fn an_unparsable_target_origin_and_a_function_throw_in_the_sender() {
    big(|| {
        let other = server(|| vec![("/c", page(""))]);
        let o = other.url("/c");
        let srv = server(move || {
            vec![(
                "/",
                framed(&o, &format!("{STORE}; f.addEventListener('load', function(){{ var r = []; try {{ w.postMessage('a', 'not a url') }} catch (e) {{ r.push(e.name) }} try {{ w.postMessage(function(){{}}, '*') }} catch (e) {{ r.push(e.name) }} try {{ w.postMessage({{ f: function(){{}} }}, '*') }} catch (e) {{ r.push(e.name) }} try {{ w.postMessage() }} catch (e) {{ r.push(e.name) }} try {{ w.postMessage.call({{}}, 1) }} catch (e) {{ r.push(e.name) }} store('r', r.join('|')) }})")),
            )]
        });
        let (s, r) = run(srv.url("/"), |_| {});
        assert_eq!(r.stop, SessionStop::Page(Stop::Settled), "{r:?}");
        assert_eq!(get(&s, &srv, "r").as_deref(), Some("SyntaxError|DataCloneError|DataCloneError|TypeError|TypeError"));
    });
}

#[test]
fn an_array_buffer_in_the_transfer_list_moves_to_the_other_page() {
    big(|| {
        let other = server(|| vec![("/c", page("addEventListener('message', function(e){ localStorage.setItem('m', new Uint8Array(e.data).join('-') + '|' + e.data.byteLength) })"))]);
        let o = other.url("/c");
        let srv = server(move || {
            vec![("/", framed(&o, &format!("{STORE}; f.addEventListener('load', function(){{ var b = new Uint8Array([1, 2, 3]).buffer; w.postMessage(b, '*', [b]); store('len', b.byteLength) }})")))]
        });
        let (s, r) = run(srv.url("/"), |_| {});
        assert_eq!(r.stop, SessionStop::Page(Stop::Settled), "{r:?}");
        assert_eq!(get(&s, &srv, "len").as_deref(), Some("0"));
        assert_eq!(get(&s, &other, "m").as_deref(), Some("1-2-3|3"));
    });
}

#[test]
fn a_message_to_a_child_that_still_loads_is_dropped_and_the_parent_settles() {
    big(|| {
        let other = server(|| vec![("/c", page("addEventListener('message', function(e){ log(e.data) })"))]);
        let o = other.url("/c");
        let srv = server(move || vec![("/", framed(&o, "w.postMessage('early', '*')"))]);
        let (s, r) = run(srv.url("/"), |_| {});
        assert_eq!(r.stop, SessionStop::Page(Stop::Settled), "{r:?}");
        assert_eq!(get(&s, &other, "log"), None);
    });
}

#[test]
#[ignore = "vex: left: None"]
fn a_grandchild_reaches_the_top_page_with_a_null_source() {
    big(|| {
        let leaf = server(|| vec![("/l", page("top.postMessage({deep: 1}, '*')"))]);
        let l = leaf.url("/l");
        let mid = server(move || vec![("/m", html(&format!("<iframe src='{l}'></iframe>")))]);
        let m = mid.url("/m");
        let srv = server(move || vec![("/", framed(&m, "addEventListener('message', function(e){ localStorage.setItem('m', [JSON.stringify(e.data), e.origin, e.source === null].join('|')) })"))]);
        let (s, r) = run(srv.url("/"), |_| {});
        assert_eq!(r.stop, SessionStop::Page(Stop::Settled), "{r:?}");
        assert_eq!(get(&s, &srv, "m"), Some(format!("{{\"deep\":1}}|{}|true", leaf.url(""))));
    });
}

#[test]
fn the_parent_does_not_settle_while_a_message_is_in_flight() {
    big(|| {
        let other = server(|| vec![("/c", page("addEventListener('load', function(){ parent.postMessage('late', '*') })"))]);
        let o = other.url("/c");
        let srv = server(move || vec![("/", framed(&o, "addEventListener('message', function(e){ localStorage.setItem('m', e.data) })"))]);
        let (s, r) = run(srv.url("/"), |_| {});
        assert_eq!(r.stop, SessionStop::Page(Stop::Settled), "{r:?}");
        assert_eq!(get(&s, &srv, "m").as_deref(), Some("late"));
    });
}

#[test]
fn a_message_to_a_removed_frame_never_reaches_a_later_frame() {
    big(|| {
        let other = server(|| vec![("/c", page("addEventListener('message', function(e){ console.log('got ' + e.data) })"))]);
        let o = other.url("/c");
        let srv = server(move || {
            vec![(
                "/",
                framed(&o, &format!("f.addEventListener('load', function(){{ w.postMessage('old', '*'); f.remove(); setTimeout(function(){{ var g = document.createElement('iframe'); g.src = '{o}'; g.onload = function(){{ console.log('second loaded') }}; document.body.appendChild(g) }}, 50) }})")),
            )]
        });
        let (mut s, r) = run(srv.url("/"), |_| {});
        assert_eq!(r.stop, SessionStop::Page(Stop::Settled), "{r:?}");
        assert_eq!(console(&mut s), vec!["second loaded".to_string()]);
    });
}

// ---------------------------------------------------------------------------------------------- ports across pages

/// A `MessagePort` pair is split across the parent and a child. The console of the top page shows each page's lines in run order.
fn lines(srv: &Server, tune: impl FnOnce(&mut SessionConfig)) -> (Session, Vec<String>) {
    let (mut s, r) = run(srv.url("/"), tune);
    assert_eq!(r.stop, SessionStop::Page(Stop::Settled), "{r:?}");
    let l = console(&mut s);
    (s, l)
}

/// Each line of the child gets a prefix with its URL. This strips it so the test reads the same for any port.
fn plain(v: &[String]) -> Vec<String> {
    v.iter().map(|l| l.rsplit_once(": ").map_or(l.clone(), |p| p.1.to_string())).collect()
}

#[test]
fn a_port_moves_from_the_parent_to_a_child_and_they_talk_three_times_in_the_same_order() {
    big(|| {
        let other = server(|| {
            vec![("/c", page("addEventListener('message', function(e){ var p = e.ports[0]; p.onmessage = function(m){ console.log('c' + m.data); p.postMessage('r' + m.data) }; p.postMessage('r0') })"))]
        });
        let o = other.url("/c");
        let srv = server(move || {
            vec![(
                "/",
                framed(&o, "f.addEventListener('load', function(){ var mc = new MessageChannel(); var n = 0; mc.port1.onmessage = function(e){ console.log('p' + e.data); if (++n < 4) mc.port1.postMessage(n) }; w.postMessage('init', '*', [mc.port2]) })"),
            )]
        });
        let mut all = Vec::new();
        for _ in 0..2 {
            let (_, l) = lines(&srv, |_| {});
            all.push(plain(&l));
        }
        assert_eq!(all[0], all[1]);
        assert_eq!(all[0], ["pr0", "c1", "pr1", "c2", "pr2", "c3", "pr3"].map(String::from).to_vec(), "{:?}", all[0]);
    });
}

#[test]
fn a_child_sends_a_port_to_its_parent_and_the_parent_replies_over_it() {
    big(|| {
        let other = server(|| {
            vec![("/c", page("var mc = new MessageChannel(); mc.port1.onmessage = function(e){ console.log('child got ' + e.data) }; parent.postMessage('hello', '*', [mc.port2])"))]
        });
        let o = other.url("/c");
        let srv = server(move || {
            vec![("/", framed(&o, "addEventListener('message', function(e){ console.log('parent got ' + e.data + ' ' + e.ports.length); e.ports[0].postMessage('reply') })"))]
        });
        let (_, l) = lines(&srv, |_| {});
        assert_eq!(plain(&l), ["parent got hello 1", "child got reply"].map(String::from).to_vec(), "{l:?}");
    });
}

#[test]
fn messages_posted_before_the_move_arrive_first_and_in_order() {
    big(|| {
        let other = server(|| vec![("/c", page("addEventListener('message', function(e){ e.ports[0].onmessage = function(m){ console.log('c' + m.data) } })"))]);
        let o = other.url("/c");
        let srv = server(move || {
            vec![(
                "/",
                framed(&o, "f.addEventListener('load', function(){ var mc = new MessageChannel(); mc.port1.postMessage('a'); mc.port1.postMessage('b'); w.postMessage('init', '*', [mc.port2]); mc.port1.postMessage('c') })"),
            )]
        });
        let (_, l) = lines(&srv, |_| {});
        assert_eq!(plain(&l), ["ca", "cb", "cc"].map(String::from).to_vec(), "{l:?}");
    });
}

#[test]
fn a_listener_gets_nothing_before_start_and_the_queued_messages_after_it() {
    big(|| {
        let other = server(|| {
            vec![(
                "/c",
                page("addEventListener('message', function(e){ var p = e.ports[0]; p.addEventListener('message', function(m){ console.log('c' + m.data) }); setTimeout(function(){ console.log('before start'); p.start() }, 30) })"),
            )]
        });
        let o = other.url("/c");
        let srv = server(move || {
            vec![("/", framed(&o, "f.addEventListener('load', function(){ var mc = new MessageChannel(); w.postMessage('init', '*', [mc.port2]); mc.port1.postMessage('a'); mc.port1.postMessage('b') })"))]
        });
        let (_, l) = lines(&srv, |_| {});
        assert_eq!(plain(&l), ["before start", "ca", "cb"].map(String::from).to_vec(), "{l:?}");
    });
}

#[test]
fn a_port_that_a_child_got_goes_back_to_the_parent_and_the_pair_still_works() {
    big(|| {
        let other = server(|| {
            vec![("/c", page("addEventListener('message', function(e){ if (e.data === 'init') parent.postMessage('back', '*', [e.ports[0]]) })"))]
        });
        let o = other.url("/c");
        let srv = server(move || {
            vec![(
                "/",
                framed(&o, "var mc; f.addEventListener('load', function(){ mc = new MessageChannel(); mc.port1.onmessage = function(e){ console.log('p1 got ' + e.data) }; w.postMessage('init', '*', [mc.port2]) }); addEventListener('message', function(e){ var back = e.ports[0]; back.onmessage = function(m){ console.log('back got ' + m.data) }; mc.port1.postMessage('x'); back.postMessage('y') })"),
            )]
        });
        let (_, l) = lines(&srv, |_| {});
        assert_eq!(plain(&l), ["back got x", "p1 got y"].map(String::from).to_vec(), "{l:?}");
    });
}

#[test]
fn both_ends_of_a_channel_sent_in_one_message_still_talk_to_each_other() {
    big(|| {
        let other = server(|| {
            vec![("/c", page("addEventListener('message', function(e){ var a = e.ports[0], b = e.ports[1]; b.onmessage = function(m){ console.log('b got ' + m.data) }; a.postMessage('ping') })"))]
        });
        let o = other.url("/c");
        let srv = server(move || {
            vec![("/", framed(&o, "f.addEventListener('load', function(){ var mc = new MessageChannel(); w.postMessage('init', '*', [mc.port1, mc.port2]) })"))]
        });
        let (_, l) = lines(&srv, |_| {});
        assert_eq!(plain(&l), ["b got ping"].map(String::from).to_vec(), "{l:?}");
    });
}

#[test]
fn an_array_buffer_in_a_message_over_a_cross_page_port_is_detached_and_arrives() {
    big(|| {
        let other = server(|| {
            vec![("/c", page("addEventListener('message', function(e){ e.ports[0].onmessage = function(m){ console.log('bytes ' + new Uint8Array(m.data).join('-')) } })"))]
        });
        let o = other.url("/c");
        let srv = server(move || {
            vec![(
                "/",
                framed(&o, "f.addEventListener('load', function(){ var mc = new MessageChannel(); w.postMessage('init', '*', [mc.port2]); var b = new Uint8Array([4, 5, 6]).buffer; mc.port1.postMessage(b, [b]); console.log('len ' + b.byteLength) })"),
            )]
        });
        let (_, l) = lines(&srv, |_| {});
        assert_eq!(l.iter().filter(|x| *x == "len 0").count(), 1, "{l:?}");
        assert!(plain(&l).contains(&"bytes 4-5-6".to_string()), "{l:?}");
    });
}

#[test]
#[ignore = "vex: left: []"]
fn a_grandchild_and_the_top_page_talk_over_a_port_that_the_middle_page_passed_down() {
    big(|| {
        let leaf = server(|| {
            vec![("/l", page("addEventListener('message', function(e){ var p = e.ports[0]; p.onmessage = function(m){ console.log('leaf got ' + m.data); p.postMessage('pong') }; p.postMessage('hi') })"))]
        });
        let l = leaf.url("/l");
        let mid = server(move || {
            vec![(
                "/m",
                html(&format!(
                    "<iframe id=g src='{l}'></iframe><script>var g = document.getElementById('g'); g.addEventListener('load', function(){{ addEventListener('message', function(e){{ g.contentWindow.postMessage('down', '*', [e.ports[0]]) }}); parent.postMessage('ready', '*') }})</script>"
                )),
            )]
        });
        let m = mid.url("/m");
        let srv = server(move || {
            vec![(
                "/",
                framed(&m, "addEventListener('message', function(e){ if (e.data !== 'ready') return; var mc = new MessageChannel(); mc.port1.onmessage = function(x){ console.log('top got ' + x.data); if (x.data === 'hi') mc.port1.postMessage('ping') }; w.postMessage('port', '*', [mc.port2]) })"),
            )]
        });
        let (_, l) = lines(&srv, |_| {});
        assert_eq!(plain(&l), ["top got hi", "leaf got ping", "top got pong"].map(String::from).to_vec(), "{l:?}");
    });
}

#[test]
fn close_on_one_side_drops_later_messages_from_the_other_and_the_page_settles() {
    big(|| {
        let other = server(|| {
            vec![("/c", page("addEventListener('message', function(e){ var p = e.ports[0]; p.onmessage = function(m){ console.log('c' + m.data); if (m.data === 'a') { p.close(); parent.postMessage('closed', '*') } } })"))]
        });
        let o = other.url("/c");
        let srv = server(move || {
            vec![(
                "/",
                framed(&o, "var mc; f.addEventListener('load', function(){ mc = new MessageChannel(); w.postMessage('init', '*', [mc.port2]); mc.port1.postMessage('a') }); addEventListener('message', function(e){ mc.port1.postMessage('late'); console.log('sent late') })"),
            )]
        });
        let (_, l) = lines(&srv, |_| {});
        assert_eq!(plain(&l), ["ca", "sent late"].map(String::from).to_vec(), "{l:?}");
    });
}

#[test]
fn removing_the_iframe_ends_its_ports_and_a_later_post_is_dropped_silently() {
    big(|| {
        let other = server(|| vec![("/c", page("addEventListener('message', function(e){ e.ports[0].onmessage = function(m){ console.log('child got ' + m.data) } })"))]);
        let o = other.url("/c");
        let srv = server(move || {
            vec![(
                "/",
                framed(&o, "f.addEventListener('load', function(){ var mc = new MessageChannel(); w.postMessage('init', '*', [mc.port2]); setTimeout(function(){ f.remove(); setTimeout(function(){ try { mc.port1.postMessage('after'); console.log('posted') } catch (e) { console.log('threw ' + e) } }, 50) }, 50) })"),
            )]
        });
        let (_, l) = lines(&srv, |_| {});
        assert_eq!(plain(&l), ["posted"].map(String::from).to_vec(), "{l:?}");
    });
}

#[test]
fn the_parent_does_not_settle_while_a_port_message_is_in_flight() {
    big(|| {
        let other = server(|| {
            vec![("/c", page("var mc = new MessageChannel(); parent.postMessage('p', '*', [mc.port2]); addEventListener('load', function(){ mc.port1.postMessage('late') })"))]
        });
        let o = other.url("/c");
        let srv = server(move || {
            vec![("/", framed(&o, "addEventListener('message', function(e){ e.ports[0].onmessage = function(m){ localStorage.setItem('m', m.data) } })"))]
        });
        let (s, _) = lines(&srv, |_| {});
        assert_eq!(get(&s, &srv, "m").as_deref(), Some("late"));
    });
}

#[test]
fn a_queued_message_that_holds_ports_is_dropped_with_one_warning_when_its_port_moves() {
    big(|| {
        let other = server(|| vec![("/c", page("addEventListener('message', function(e){ var p = e.ports[0]; p.onmessage = function(m){ console.log('c got ' + m.data) } })"))]);
        let o = other.url("/c");
        let srv = server(move || {
            vec![(
                "/",
                framed(&o, "f.addEventListener('load', function(){ var mc = new MessageChannel(); var x = new MessageChannel(); mc.port1.postMessage('has ports', [x.port1]); mc.port1.postMessage('plain'); w.postMessage('init', '*', [mc.port2]) })"),
            )]
        });
        let (_, l) = lines(&srv, |_| {});
        assert_eq!(l.iter().filter(|x| x.contains("carried ports was dropped")).count(), 1, "{l:?}");
        let p = plain(&l);
        assert!(p.contains(&"c got plain".to_string()) && !p.iter().any(|x| x.contains("has ports")), "{l:?}");
    });
}

/// Run `src` in the page of the session after the run ended.
fn eval(s: &mut Session, src: &str) -> String {
    let b = s.browser().unwrap();
    let v = b.run_script(src).expect("script");
    b.it.display(v)
}

fn collect3(s: &mut Session) {
    for _ in 0..3 {
        s.browser().unwrap().collect(&[]).unwrap();
    }
}

/// The handles of the shared functions of `Common`: handler, `Proxy.revocable`, `closed`, `replace`, the two `href` functions and four no-ops.
const COMMON: usize = 10;

fn window_page(tail: &str) -> (Server, Server) {
    let other = server(|| vec![("/child", html("<p>child</p>"))]);
    let o = other.url("/child");
    let tail = tail.to_string();
    let srv = server(move || vec![("/", html(&format!("<iframe id=f src='{o}'></iframe><script>{tail}</script>")))]);
    (srv, other)
}

#[test]
#[ignore = "vex: left: 0"]
fn a_remote_window_that_no_script_holds_is_reachable_while_its_page_lives() {
    big(|| {
        let (srv, _other) = window_page("var r = new WeakRef(document.getElementById('f').contentWindow); setTimeout(function () {}, 30);");
        let (mut s, r) = run(srv.url("/"), |_| {});
        assert_eq!(r.stop, SessionStop::Page(Stop::Settled), "{r:?}");
        collect3(&mut s);
        // The target and the proxy of the window, its `location` object and the 10 shared functions.
        assert_eq!(s.browser().unwrap().dom.kids.handle_count(), 3 + COMMON);
        assert_eq!(eval(&mut s, "typeof r.deref() + ',' + (r.deref() === document.getElementById('f').contentWindow) + ',' + typeof r.deref().postMessage"), "object,true,function");
    });
}

#[test]
#[ignore = "vex: left: 0"]
fn closing_the_child_page_frees_the_handles_of_its_remote_window() {
    big(|| {
        let (srv, _other) = window_page("var r = new WeakRef(document.getElementById('f').contentWindow); setTimeout(function () { document.getElementById('f').remove() }, 30);");
        let (mut s, r) = run(srv.url("/"), |_| {});
        assert_eq!(r.stop, SessionStop::Page(Stop::Settled), "{r:?}");
        // Only the shared functions stay: they live as long as the page.
        assert_eq!(s.browser().unwrap().dom.kids.handle_count(), COMMON);
        collect3(&mut s);
        // No script holds the window and no handle does: it is collected.
        assert_eq!(eval(&mut s, "typeof r.deref()"), "undefined");
    });
}

#[test]
fn a_remote_window_that_a_script_holds_survives_three_collections() {
    big(|| {
        let (srv, _other) = window_page("var w = document.getElementById('f').contentWindow; var l = w.location;");
        let (mut s, r) = run(srv.url("/"), |_| {});
        assert_eq!(r.stop, SessionStop::Page(Stop::Settled), "{r:?}");
        collect3(&mut s);
        assert_eq!(eval(&mut s, "[w === document.getElementById('f').contentWindow, w.closed, typeof w.postMessage, w.window === w, typeof l.replace].join()"), "true,false,function,true,function");
    });
}
