//! Tests of `Session::resume`: input into a settled document through the existing driver primitives, then the same pumps.
//! Loopback fixtures only; every effect is seen in storage or in server hits. Each test runs on a big stack.

use crate::browser_loader::*;
use crate::browser_session::*;
use crate::resources::test_server::{Reply, Server, serve};
use bare_js::eval::EvalLimits;
use std::time::Duration;

fn big<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(64 << 20).spawn(f).unwrap().join().unwrap()
}

fn html(body: &str) -> Reply {
    Reply::new(200, Some("text/html; charset=utf-8"), body)
}

fn text(body: &str) -> Reply {
    Reply::new(200, Some("text/plain"), body)
}

fn server(routes: impl Fn() -> Vec<(&'static str, Reply)> + Send + Sync + 'static) -> Server {
    serve(move |path| routes().into_iter().find(|r| r.0 == path).map(|r| r.1).unwrap_or_else(|| Reply::new(404, Some("text/plain"), "no")))
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

const FORM: &str = "<form id=f><input id=q type=text><button id=go type=button>go</button></form>";

#[test]
fn input_and_submit_queue_real_fetches_promises_and_timers_in_the_same_realm() {
    big(|| {
        let srv = server(|| vec![
            ("/", html(&format!("{FORM}<script>window.marker = 'same'; var seen = [];
                document.getElementById('q').addEventListener('input', function (e) {{
                  seen.push(e.target.value);
                  fetch('/echo-hi').then(function (r) {{ return r.text() }}).then(function (t) {{ localStorage.setItem('typed', t + ':' + seen.join()) }});
                }});
                document.getElementById('f').addEventListener('submit', function (e) {{
                  e.preventDefault();
                  Promise.resolve().then(function () {{ localStorage.setItem('micro', window.marker) }});
                  fetch('/sub').then(function (r) {{ return r.text() }}).then(function (t) {{ setTimeout(function () {{ localStorage.setItem('timer', t) }}, 40) }});
                }});</script>"))),
            ("/echo-hi", text("echo")),
            ("/sub", text("sent")),
        ]);
        let mut s = Session::new(cfg(srv.url("/")));
        assert_eq!(s.run().stop, SessionStop::Page(Stop::Settled));
        assert_eq!(get(&s, &srv, "typed"), None);
        let r = s.resume(&[Input::Type("#q", "hi"), Input::Submit("#f")]);
        assert_eq!(r.stop, SessionStop::Page(Stop::Settled), "{r:?}");
        assert_eq!((r.pages.len(), r.urls.len()), (1, 1));
        assert_eq!(get(&s, &srv, "typed").as_deref(), Some("echo:hi"));
        assert_eq!(get(&s, &srv, "micro").as_deref(), Some("same"));
        assert_eq!(get(&s, &srv, "timer").as_deref(), Some("sent"));
        assert_eq!((srv.hits("/"), srv.hits("/echo-hi"), srv.hits("/sub")), (1, 1, 1), "no reload, one fetch each");
        let b = s.browser().unwrap();
        let q = b.query("#q").unwrap().unwrap();
        assert_eq!(b.input_value(q).as_deref(), Some("hi"), "the value stayed: the same DOM");
    });
}

#[test]
fn navigation_after_input_makes_a_fresh_realm_and_old_callbacks_never_run() {
    big(|| {
        let srv = server(|| vec![
            ("/a", html(&format!("{FORM}<script>window.marker = 'old'; localStorage.setItem('k', 'v1');
                document.getElementById('go').addEventListener('click', function () {{
                  setTimeout(function () {{ localStorage.setItem('old-timer', '1') }}, 0);
                  fetch('/slow').then(function () {{ localStorage.setItem('old-fetch', '1') }});
                  Promise.resolve().then(function () {{ localStorage.setItem('same-task-micro', '1') }});
                  location.assign('/b?token=secretq');
                }});</script>"))),
            ("/b?token=secretq", html("<script>localStorage.setItem('b', [typeof window.marker, localStorage.getItem('k')].join('|'));
                document.addEventListener('click', function () { localStorage.setItem('b-click', '1') })</script><button id=go>x</button>")),
            ("/slow", text("x")),
        ]);
        let mut s = Session::new(cfg(srv.url("/a")));
        s.run();
        let r = s.resume(&[Input::Click("#go")]);
        assert_eq!(r.stop, SessionStop::Page(Stop::Settled), "{r:?}");
        assert_eq!(r.pages.len(), 2, "the resumed page, then the new document");
        assert!(matches!(r.pages[0].stop, Stop::Navigate(_)));
        assert_eq!(get(&s, &srv, "b").as_deref(), Some("undefined|v1"));
        assert_eq!(get(&s, &srv, "same-task-micro").as_deref(), Some("1"));
        assert_eq!(get(&s, &srv, "old-timer"), None);
        assert_eq!(get(&s, &srv, "old-fetch"), None);
        assert_eq!(srv.hits("/slow"), 0);
        assert!(!format!("{r:?}").contains("secretq"));
        // the new document takes input too
        let r = s.resume(&[Input::Click("#go")]);
        assert_eq!(r.stop, SessionStop::Page(Stop::Settled), "{r:?}");
        assert_eq!(get(&s, &srv, "b-click").as_deref(), Some("1"));
    });
}

#[test]
fn a_navigation_ends_the_inputs_before_the_next_one_runs() {
    big(|| {
        let srv = server(|| vec![
            ("/a", html(&format!("{FORM}<script>document.getElementById('go').addEventListener('click', function () {{ location.assign('/b') }});
                document.getElementById('q').addEventListener('input', function () {{ localStorage.setItem('late', '1') }})</script>"))),
            ("/b", html("<p>b</p>")),
        ]);
        let mut s = Session::new(cfg(srv.url("/a")));
        s.run();
        let r = s.resume(&[Input::Click("#go"), Input::Type("#q", "x")]);
        assert!(matches!(r.pages[0].stop, Stop::Navigate(_)), "{r:?}");
        assert_eq!(get(&s, &srv, "late"), None, "no old script runs after the navigation request");
    });
}

#[test]
fn idle_resumes_keep_the_realm_and_spend_the_round_budget() {
    big(|| {
        let srv = server(|| vec![("/", html("<script>window.marker = 1; setTimeout(function () { localStorage.setItem('t', '1') }, 10)</script>"))]);
        let mut c = cfg(srv.url("/"));
        c.limits.max_rounds = 12;
        let mut s = Session::new(c);
        let first = s.run();
        assert_eq!(first.stop, SessionStop::Page(Stop::Settled));
        let mut spent = first.rounds;
        let mut ended = false;
        for _ in 0..40 {
            let r = s.resume(&[]);
            spent += r.rounds;
            assert!(spent <= 12, "rounds are session-wide: {spent}");
            if r.stop == SessionStop::RoundBudget {
                ended = true;
                break;
            }
            assert_eq!(r.stop, SessionStop::Page(Stop::Settled), "{r:?}");
            assert_eq!(r.tasks, 0, "idle: nothing runs");
        }
        assert!(ended, "idle resumes cannot reset the round budget");
        assert_eq!(srv.hits("/"), 1);
        let again = s.resume(&[Input::Click("#none")]);
        assert_eq!((again.stop, again.rounds, again.tasks), (SessionStop::RoundBudget, 0, 0), "sticky");
    });
}

#[test]
fn task_and_time_budgets_survive_resumes_and_end_the_session() {
    big(|| {
        let srv = server(|| vec![("/", html("<button id=go></button><script>var n = 0;
            document.getElementById('go').addEventListener('click', function () { setInterval(function () { n++; localStorage.setItem('n', String(n)) }, 0) })</script>"))]);
        let mut c = cfg(srv.url("/"));
        c.limits.max_tasks = 300;
        let mut s = Session::new(c);
        s.run();
        let r = s.resume(&[Input::Click("#go")]);
        assert!(matches!(r.stop, SessionStop::Page(Stop::TaskBudget) | SessionStop::TaskBudget), "{r:?}");
        let n = get(&s, &srv, "n");
        assert!(r.tasks <= 300 + 8, "{}", r.tasks);
        for _ in 0..3 {
            let again = s.resume(&[Input::Click("#go")]);
            assert_eq!(again.stop, r.stop);
            assert_eq!((again.tasks, again.rounds, again.pages.len()), (0, 0, 0));
        }
        assert_eq!(get(&s, &srv, "n"), n, "no JS after the budget stop");

        let mut c = cfg(srv.url("/"));
        c.limits.max_time = Duration::from_millis(150);
        let mut s = Session::new(c);
        s.run();
        let r = s.resume(&[Input::Click("#go")]);
        assert!(matches!(r.stop, SessionStop::Page(Stop::TimeBudget) | SessionStop::TimeBudget), "{r:?}");
        let n = get(&s, &srv, "n");
        assert_eq!(s.resume(&[]).stop, r.stop);
        assert_eq!(get(&s, &srv, "n"), n);
    });
}

#[test]
fn the_document_budget_counts_navigations_that_resumes_cause() {
    big(|| {
        let srv = server(|| vec![("/", html("<button id=go></button><script>document.getElementById('go').addEventListener('click', function () { location.reload() })</script>"))]);
        let mut c = cfg(srv.url("/"));
        c.limits.max_documents = 3;
        let mut s = Session::new(c);
        s.run();
        for _ in 0..2 {
            assert_eq!(s.resume(&[Input::Click("#go")]).stop, SessionStop::Page(Stop::Settled));
        }
        assert_eq!(srv.hits("/"), 3);
        assert_eq!(s.resume(&[Input::Click("#go")]).stop, SessionStop::DocumentLimit);
        assert_eq!(s.resume(&[Input::Click("#go")]).stop, SessionStop::DocumentLimit);
        assert_eq!(srv.hits("/"), 3, "no load past the limit");
    });
}

#[test]
fn a_fatal_stop_is_sticky_and_a_queued_navigation_never_loads() {
    big(|| {
        let srv = server(|| vec![
            ("/", html("<button id=go></button><input id=q><script>document.getElementById('go').addEventListener('click', function () { location.assign('/b'); while (true) {} });
                document.getElementById('q').addEventListener('input', function () { localStorage.setItem('after', '1') })</script>")),
            ("/b", html("<p>b</p>")),
        ]);
        let mut s = Session::new(cfg(srv.url("/")));
        s.run();
        let r = s.resume(&[Input::Click("#go")]);
        assert!(matches!(r.stop, SessionStop::Page(Stop::Fatal(Fatal::Limit(_)))), "{r:?}");
        let again = s.resume(&[Input::Type("#q", "x")]);
        assert_eq!(again.stop, r.stop);
        assert_eq!((again.tasks, again.pages.len()), (0, 0));
        assert_eq!(get(&s, &srv, "after"), None);
        assert_eq!(srv.hits("/b"), 0);
    });
}

#[test]
fn a_missing_target_is_not_fatal_and_a_wrong_element_is() {
    big(|| {
        let srv = server(|| vec![("/", html("<p id=p></p><input id=q><script>document.getElementById('q').addEventListener('input', function () { localStorage.setItem('v', document.getElementById('q').value) })</script>"))]);
        let mut s = Session::new(cfg(srv.url("/")));
        s.run();
        let r = s.resume(&[Input::Type("#q", "a"), Input::Click("#missing"), Input::Type("#q", "b")]);
        assert_eq!(r.stop, SessionStop::Page(Stop::NoTarget(1)), "{r:?}");
        assert_eq!(get(&s, &srv, "v").as_deref(), Some("a"));
        assert_eq!(s.resume(&[Input::Click("[")]).stop, SessionStop::Page(Stop::NoTarget(0)));
        assert_eq!(s.resume(&[Input::Type("#q", "c")]).stop, SessionStop::Page(Stop::Settled));
        assert_eq!(get(&s, &srv, "v").as_deref(), Some("c"));
        let r = s.resume(&[Input::Type("#p", "x")]);
        assert!(matches!(r.stop, SessionStop::Page(Stop::Fatal(Fatal::Unsupported(_)))), "{r:?}");
        assert_eq!(s.resume(&[Input::Type("#q", "d")]).stop, r.stop);
        assert_eq!(get(&s, &srv, "v").as_deref(), Some("c"));
    });
}

#[test]
fn resume_before_run_and_after_a_failed_load_does_nothing() {
    big(|| {
        let srv = server(|| vec![]);
        let mut s = Session::new(cfg(srv.url("/")));
        assert!(matches!(s.resume(&[]).stop, SessionStop::LoadFailed(_)));
        assert_eq!(srv.total(), 0);
        let r = s.run();
        assert!(matches!(r.stop, SessionStop::LoadFailed(_)));
        assert_eq!(s.resume(&[]).stop, r.stop);
    });
}

#[test]
fn garbage_of_resumed_listeners_is_collected() {
    big(|| {
        let srv = server(|| vec![("/", html("<button id=go></button><script>document.getElementById('go').addEventListener('click', function () {
            var a = []; for (var i = 0; i < 3000; i++) a.push({ i: i }); localStorage.setItem('last', String(a.length)) })</script>"))]);
        let mut c = cfg(srv.url("/"));
        c.heap.max_objects = 20_000;
        let mut s = Session::new(c);
        s.run();
        for i in 0..20 {
            let r = s.resume(&[Input::Click("#go")]);
            assert_eq!(r.stop, SessionStop::Page(Stop::Settled), "resume {i}: {r:?}");
        }
        assert_eq!(get(&s, &srv, "last").as_deref(), Some("3000"));
    });
}

/// Load driver, run by hand in a fresh capped process: `BARE_RESUME_LOAD=<n> cargo test --release -- --ignored resume_load`.
/// `n` rounds of type + submit on one settled page. Every 10th round also does a real loopback fetch. Prints a checksum.
#[test]
#[ignore]
fn resume_load() {
    let n: usize = std::env::var("BARE_RESUME_LOAD").ok().and_then(|v| v.parse().ok()).unwrap_or(100);
    let stack = if cfg!(debug_assertions) { 64 << 20 } else { 8 << 20 };
    std::thread::Builder::new().stack_size(stack).spawn(move || {
        let srv = server(|| vec![("/", html("<input id=q><form id=f></form><script>var sum = 0, last = '', fx = 0;
            document.getElementById('q').addEventListener('input', function (e) { last = e.target.value });
            document.getElementById('f').addEventListener('submit', function () {
              sum += last.length * 3 + 1;
              Promise.resolve(sum).then(function (v) { localStorage.setItem('sum', String(v)) });
              if (last.length % 10 == 0) fetch('/p').then(function (r) { return r.text() }).then(function (t) { fx += t.length; localStorage.setItem('fx', String(fx)) });
            })</script>")), ("/p", text("ab"))]);
        let mut c = cfg(srv.url("/"));
        c.limits.max_tasks = 10_000_000;
        c.limits.max_rounds = 10_000_000;
        c.limits.max_time = Duration::from_secs(40);
        c.fetch.max_started = 1_000_000;
        c.page.resources.max_fetches = 1_000_000;
        let mut s = Session::new(c);
        s.run();
        let (mut want, mut fetches) = (0usize, 0usize);
        let text_in: Vec<String> = (0..n).map(|i| "x".repeat(i % 20)).collect();
        for t in &text_in {
            let r = s.resume(&[Input::Type("#q", t), Input::Submit("#f")]);
            assert_eq!(r.stop, SessionStop::Page(Stop::Settled), "{r:?}");
            want += t.len() * 3 + 1;
            if t.len() % 10 == 0 { fetches += 1; }
        }
        let (sum, fx) = (get(&s, &srv, "sum"), get(&s, &srv, "fx"));
        assert_eq!(sum.as_deref(), Some(want.to_string().as_str()));
        assert_eq!(fx.as_deref(), Some((fetches * 2).to_string().as_str()));
        assert_eq!((srv.hits("/"), srv.hits("/p")), (1, fetches));
        println!("LOAD n={n} sum={want} fx={} hits={fetches} checksum_ok", fetches * 2);
    }).unwrap().join().unwrap();
}
