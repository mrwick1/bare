//! A script that hits the step budget (or another JS execution budget) ends only its own task, as Chrome ends a runaway script:
//! the DOM stays, one console error says what happened, and later tasks and input still run. A page budget bounds the aborts.
//! Each test runs on a big stack. The fixtures are loopback pages; the probe is `localStorage`.

use crate::browser_console::ConsoleLevel;
use crate::browser_loader::*;
use crate::browser_session::*;
use crate::resources::test_server::{Reply, Server, serve};
use bare_js::eval::EvalLimits;
use std::time::Duration;

fn big<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(64 << 20).spawn(f).unwrap().join().unwrap()
}

fn page(body: &'static str) -> Server {
    serve(move |path| if path == "/" { Reply::new(200, Some("text/html; charset=utf-8"), body) } else { Reply::new(404, Some("text/plain"), "no") })
}

fn cfg(srv: &Server) -> SessionConfig {
    let mut c = SessionConfig::new(&srv.url("/"));
    c.virtual_clock = true;
    c.eval = EvalLimits { max_steps: 2_000_000, ..Default::default() };
    c.page.quiet = Duration::from_millis(100);
    c
}

fn get(s: &Session, srv: &Server, key: &str) -> Option<String> {
    s.storage().get_item(false, &srv.url(""), key)
}

/// The console of the page: `(level, text)`.
fn console(s: &mut Session) -> Vec<(ConsoleLevel, String)> {
    let b = s.browser().expect("a document");
    b.take_console_records().iter().map(|r| (r.level, b.format_console_record(r).unwrap())).collect()
}

fn errors(s: &mut Session) -> Vec<String> {
    console(s).into_iter().filter(|(l, _)| *l == ConsoleLevel::Error).map(|(_, t)| t).collect()
}

#[test]
fn a_runaway_script_ends_only_itself_and_the_page_keeps_running() {
    big(|| {
        let srv = page("<button id=go></button><p id=keep>kept</p><script>
            localStorage.setItem('a', '1');
            setTimeout(function () { localStorage.setItem('timer', '1') }, 10);
            document.getElementById('go').addEventListener('click', function () { localStorage.setItem('click', '1') });
          </script><script>
            document.body.appendChild(document.createElement('hr')); while (true) {}
          </script><script>localStorage.setItem('c', '1')</script>");
        let mut s = Session::new(cfg(&srv));
        let r = s.run();
        assert_eq!(r.stop, SessionStop::Page(Stop::Settled), "{r:?}");
        for k in ["a", "c", "timer"] {
            assert_eq!(get(&s, &srv, k).as_deref(), Some("1"), "{k}");
        }
        let e = errors(&mut s);
        assert_eq!(e.len(), 1, "{e:?}");
        assert!(e[0].contains("Steps") && e[0].contains("inline script"), "{e:?}");
        // the DOM stays as the script left it, and input still runs
        let r = s.resume(&[Input::Click("#go")]);
        assert_eq!(r.stop, SessionStop::Page(Stop::Settled), "{r:?}");
        assert_eq!(get(&s, &srv, "click").as_deref(), Some("1"));
        let b = s.browser().unwrap();
        assert!(b.query("#keep").unwrap().is_some() && b.query("hr").unwrap().is_some());
    });
}

#[test]
fn a_runaway_timer_event_handler_and_microtask_each_end_only_their_task() {
    big(|| {
        let srv = page("<button id=go></button><input id=q><script>
            var go = document.getElementById('go');
            go.addEventListener('click', function () { while (true) {} });
            go.addEventListener('click', function () { localStorage.setItem('second', '1') });
            document.getElementById('q').addEventListener('input', function () { Promise.resolve().then(function () { while (true) {} }); Promise.resolve().then(function () { localStorage.setItem('job2', '1') }) });
            setTimeout(function () { for (;;) {} }, 5);
            setTimeout(function () { localStorage.setItem('after-timer', '1') }, 20);
          </script>");
        let mut s = Session::new(cfg(&srv));
        let r = s.run();
        assert_eq!(r.stop, SessionStop::Page(Stop::Settled), "{r:?}");
        assert_eq!(get(&s, &srv, "after-timer").as_deref(), Some("1"));
        assert_eq!(errors(&mut s).len(), 1);
        // a click: the first handler runs away, the task ends. Later input still works.
        let r = s.resume(&[Input::Click("#go"), Input::Type("#q", "x")]);
        assert_eq!(r.stop, SessionStop::Page(Stop::Settled), "{r:?}");
        assert_eq!(get(&s, &srv, "job2").as_deref(), Some("1"), "the job after the runaway job still runs");
        let e = errors(&mut s);
        assert_eq!(e.len(), 2, "{e:?}");
        let r = s.resume(&[Input::Click("#go")]);
        assert_eq!(r.stop, SessionStop::Page(Stop::Settled), "{r:?}");
    });
}

#[test]
fn an_endless_chain_of_runaway_tasks_stops_the_page_with_a_reason() {
    big(|| {
        let srv = page("<input id=q><script>
            localStorage.setItem('n', '0');
            document.getElementById('q').addEventListener('input', function () { localStorage.setItem('typed', '1') });
            (function again() { setTimeout(again, 0); while (true) {} })();
          </script>");
        let mut s = Session::new(cfg(&srv));
        let r = s.run();
        let SessionStop::Page(Stop::Fatal(Fatal::Aborted(why))) = &r.stop else { panic!("{r:?}") };
        assert!(why.contains("Steps") && why.contains("aborted"), "{why}");
        // the stop is sticky and input does nothing
        let again = s.resume(&[Input::Type("#q", "x")]);
        assert_eq!(again.stop, r.stop);
        assert_eq!(get(&s, &srv, "typed"), None);
    });
}

#[test]
fn a_runaway_inside_a_host_call_leaves_that_feature_usable() {
    big(|| {
        // Each feature runs away once, then works: no flag of the host stays set by the aborted call.
        let srv = page("<button id=b></button><div id=d></div><script>
            var runaway = true, log = [];
            function maybe() { if (runaway) { while (true) {} } }
            var b = document.getElementById('b'), d = document.getElementById('d');
            b.addEventListener('click', function () { maybe(); log.push('click') });
            d.addEventListener('custom', function () { maybe(); log.push('custom') });
            new MutationObserver(function () { maybe(); log.push('mo') }).observe(d, { childList: true });
            requestAnimationFrame(function () { maybe(); log.push('raf') });
            function deep() { return deep() + 1 }
            window.go = function () {
              try { b.click() } catch (e) { log.push('click threw') }
              try { d.dispatchEvent(new Event('custom')) } catch (e) { log.push('dispatch threw') }
              d.appendChild(document.createElement('i'));
            };
            setTimeout(function () { go(); }, 5);
          </script>");
        let mut s = Session::new(cfg(&srv));
        let r = s.run();
        assert_eq!(r.stop, SessionStop::Page(Stop::Settled), "{r:?}");
        let e = errors(&mut s);
        assert_eq!(e.len(), 2, "the timer that clicks, and the frame callback: {e:?}");
        // now the same calls work
        let b = s.browser().unwrap();
        b.run_script("runaway = false; log = []; b.click(); d.dispatchEvent(new Event('custom')); d.appendChild(document.createElement('i'))").unwrap();
        let _ = b.drain();
        let v = b.run_script("log.join()").unwrap();
        let got = b.it.display(v);
        assert!(got.contains("click") && got.contains("custom"), "{got}");
        let v = b.run_script("d.children.length >= 1").unwrap();
        assert_eq!(b.it.display(v), "true");
    });
}

#[test]
fn runaway_recursion_ends_the_task_and_the_next_one_runs() {
    big(|| {
        let srv = page("<script>
            function deep() { return deep() + 1 }
            setTimeout(function () { deep() }, 1);
            setTimeout(function () { localStorage.setItem('next', '1') }, 20);
            Promise.resolve().then(function () { for (;;) {} });
            Promise.resolve().then(function () { localStorage.setItem('job', '1') });
          </script>");
        let mut s = Session::new(cfg(&srv));
        let r = s.run();
        assert_eq!(r.stop, SessionStop::Page(Stop::Settled), "{r:?}");
        assert_eq!((get(&s, &srv, "job").as_deref(), get(&s, &srv, "next").as_deref()), (Some("1"), Some("1")));
        // The job and the deep call (the engine ends a deep call with an uncatchable `CallDepth`, not a `RangeError`).
        assert_eq!(r.pages[0].aborted, 2);
    });
}

#[test]
fn a_long_selector_ends_only_its_task() {
    big(|| {
        let srv = page("<button id=go></button><script>
            document.getElementById('go').addEventListener('click', function () { localStorage.setItem('click', '1') });
            document.querySelector('a'.repeat(1025));
            localStorage.setItem('unreachable', '1');
          </script><script>localStorage.setItem('next', '1')</script>");
        let mut s = Session::new(cfg(&srv));
        let r = s.run();
        assert_eq!(r.stop, SessionStop::Page(Stop::Settled), "{r:?}");
        assert_eq!(get(&s, &srv, "unreachable"), None);
        assert_eq!(get(&s, &srv, "next").as_deref(), Some("1"));
        let e = errors(&mut s);
        assert_eq!(e.len(), 1, "{e:?}");
        assert!(e[0].contains("dom-selector"), "{e:?}");
        let r = s.resume(&[Input::Click("#go")]);
        assert_eq!(r.stop, SessionStop::Page(Stop::Settled), "{r:?}");
        assert_eq!(get(&s, &srv, "click").as_deref(), Some("1"));
    });
}
