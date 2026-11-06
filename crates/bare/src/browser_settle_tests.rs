//! Tests of the settle rule (header of `browser_session.rs`, section "Settled"): which pages end at `Settled` and which
//! do not, with recurring timers and animation frame loops around. Loopback pages, the virtual clock, `quiet` of 1 s.
//! A page that must NOT settle runs on a small task budget and ends at a budget stop.

use crate::browser_loader::*;
use crate::browser_session::*;
use crate::resources::test_server::{Reply, Server, serve};
use bare_js::eval::EvalLimits;
use std::time::{Duration, Instant};

fn big<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(64 << 20).spawn(f).unwrap().join().unwrap()
}

fn server(body: &'static str) -> Server {
    serve(move |path| match path {
        "/" => Reply::new(200, Some("text/html; charset=utf-8"), body),
        "/p" => Reply::new(200, Some("text/plain"), "poll"),
        _ => Reply::new(404, Some("text/plain"), "no"),
    })
}

fn cfg(url: String) -> SessionConfig {
    let mut c = SessionConfig::new(&url);
    c.virtual_clock = true;
    c.eval = EvalLimits { max_steps: 5_000_000, ..Default::default() };
    c.page.quiet = Duration::from_secs(1);
    c
}

fn n(s: &Session, srv: &Server, key: &str) -> u32 {
    s.storage().get_item(false, &srv.url(""), key).and_then(|v| v.parse().ok()).unwrap_or(0)
}

/// Run the page to its stop. Gives the session, the server, the result and the wall time.
fn run(body: &'static str, tasks: usize) -> (Session, Server, SessionResult, Duration) {
    let srv = server(body);
    let mut c = cfg(srv.url("/"));
    c.limits.max_tasks = tasks;
    c.limits.max_rounds = tasks;
    let mut s = Session::new(c);
    let t = Instant::now();
    let r = s.run();
    let wall = t.elapsed();
    eprintln!("settle test: stop {:?}, tasks {}, wall {:?}", r.stop, r.tasks, wall);
    (s, srv, r, wall)
}

fn settled(r: &SessionResult) -> bool {
    r.stop == SessionStop::Page(Stop::Settled)
}

#[test]
fn an_interval_clock_settles_and_is_reported() {
    big(|| {
        let (s, srv, r, _) = run("<div id=c>0</div><script>var k = 0; setInterval(function () { k++; document.getElementById('c').textContent = k; localStorage.setItem('n', k) }, 1000)</script>", 5_000);
        assert!(settled(&r), "{r:?}");
        assert_eq!(r.pages[0].idle_timers, vec![1000]);
        // it learned from one run, then stopped waiting
        assert_eq!(n(&s, &srv, "n"), 1);
    });
}

#[test]
fn a_self_rearming_timeout_with_the_same_callback_settles() {
    big(|| {
        let (s, srv, r, _) = run("<script>var k = 0; function tick() { k++; localStorage.setItem('n', k); setTimeout(tick, 400) } setTimeout(tick, 400)</script>", 5_000);
        assert!(settled(&r), "{r:?}");
        assert_eq!(r.pages[0].idle_timers, vec![400]);
        assert_eq!(n(&s, &srv, "n"), 1);
    });
}

#[test]
fn a_timer_faster_than_a_frame_is_a_loop_and_never_skipped() {
    big(|| {
        let (_, _, r, _) = run("<script>setInterval(function () {}, 5)</script>", 300);
        assert!(!settled(&r), "{r:?}");
    });
}

#[test]
fn a_poll_that_fetches_each_time_does_not_settle_when_it_is_due_inside_the_window() {
    big(|| {
        let (_, srv, r, _) = run("<script>function poll() { fetch('/p').then(function (x) { return x.text() }); setTimeout(poll, 300) } poll()</script>", 300);
        assert!(!settled(&r), "{r:?}");
        assert!(srv.hits("/p") > 3);
    });
}

#[test]
fn a_poll_that_fetches_every_5_s_settles_after_its_fetch_is_done() {
    big(|| {
        let (s, srv, r, _) = run("<script>function poll() { fetch('/p').then(function (x) { return x.text() }).then(function (t) { localStorage.setItem('p', '1') }); setTimeout(poll, 5000) } poll()</script>", 5_000);
        assert!(settled(&r), "{r:?}");
        assert_eq!((srv.hits("/p"), n(&s, &srv, "p")), (1, 1), "the fetch finished and its callback ran before the settle");
        // the next poll is beyond the window: it is not skipped, it just is not near
        assert!(r.pages[0].idle_timers.is_empty());
    });
}

#[test]
fn a_timer_that_goes_quiet_after_some_work_settles_after_it_stopped() {
    big(|| {
        let (s, srv, r, _) = run("<div id=c></div><script>var k = 0; setInterval(function () { k++; if (k <= 3) { for (var i = 0; i < 20; i++) document.getElementById('c').appendChild(document.createElement('i')) } localStorage.setItem('n', k) }, 300)</script>", 5_000);
        assert!(settled(&r), "{r:?}");
        assert_eq!(n(&s, &srv, "n"), 4, "three runs with work, one quiet run");
        assert_eq!(r.pages[0].idle_timers, vec![300]);
    });
}

#[test]
fn a_one_shot_timer_inside_the_window_is_waited_for_with_a_recurring_one_around() {
    big(|| {
        let (s, srv, r, _) = run(
            "<div id=c></div><script>var k = 0; setInterval(function () { k++; localStorage.setItem('n', k) }, 200);
             setTimeout(function () { var d = document.createElement('p'); d.id = 'late'; document.getElementById('c').appendChild(d); localStorage.setItem('late', '1') }, 700)</script>",
            5_000,
        );
        assert!(settled(&r), "{r:?}");
        assert_eq!(n(&s, &srv, "late"), 1, "the one-shot ran before the settle");
        assert!(n(&s, &srv, "n") >= 3, "the interval ran on time while the page waited: {}", n(&s, &srv, "n"));
        let mut s = s;
        let b = s.browser().unwrap();
        assert!(b.query("#late").unwrap().is_some());
        assert_eq!(r.pages[0].idle_timers, vec![200]);
    });
}

#[test]
fn a_raf_loop_that_changes_nothing_settles() {
    big(|| {
        let (s, srv, r, _) = run("<script>var k = 0; function f() { k++; localStorage.setItem('n', k); requestAnimationFrame(f) } requestAnimationFrame(f)</script>", 5_000);
        assert!(settled(&r), "{r:?}");
        assert!(r.pages[0].idle_frames);
        assert_eq!(n(&s, &srv, "n"), 1, "one judged frame, then it stops waiting");
    });
}

/// Decision: a frame that makes at most 8 changes to the DOM (a spinner, a progress bar, one style) is quiet. A clock does the
/// same from a timer, and a page that waits for every spinner never ends.
#[test]
fn a_raf_loop_that_changes_one_style_per_frame_settles() {
    big(|| {
        let (_, _, r, _) = run("<div id=s></div><script>var k = 0; function f() { k++; document.getElementById('s').setAttribute('style', 'width:' + k + 'px'); requestAnimationFrame(f) } requestAnimationFrame(f)</script>", 5_000);
        assert!(settled(&r), "{r:?}");
        assert!(r.pages[0].idle_frames);
    });
}

#[test]
fn a_raf_loop_that_builds_dom_or_starts_work_does_not_settle() {
    big(|| {
        let (_, _, r, _) = run("<div id=c></div><script>function f() { for (var i = 0; i < 20; i++) document.getElementById('c').appendChild(document.createElement('i')); requestAnimationFrame(f) } requestAnimationFrame(f)</script>", 300);
        assert!(!settled(&r), "20 changes in a frame: {r:?}");
        let (_, srv, r, _) = run("<script>function f() { fetch('/p'); requestAnimationFrame(f) } requestAnimationFrame(f)</script>", 300);
        assert!(!settled(&r), "a fetch in a frame: {r:?}");
        assert!(srv.hits("/p") > 3);
        let (_, _, r, _) = run("<script>function f() { setTimeout(function () {}, 10); requestAnimationFrame(f) } requestAnimationFrame(f)</script>", 300);
        assert!(!settled(&r), "a new timer in a frame: {r:?}");
    });
}

/// Decision: the rendering step that a timer asks for is judged on its own, as a frame. It is not a task that the timer queued. A
/// clock that asks for a frame at each tick (an animation of a value) stays a clock, as one that does not. The staging `/calls` page
/// did this and ran 140 rounds for 35 s.
#[test]
fn an_interval_that_asks_for_a_quiet_frame_each_tick_settles() {
    big(|| {
        let (s, srv, r, _) = run("<script>var k = 0; setInterval(function () { k++; localStorage.setItem('n', k); requestAnimationFrame(function () { localStorage.setItem('f', k) }) }, 1000)</script>", 5_000);
        assert!(settled(&r), "{r:?}");
        assert_eq!(r.pages[0].idle_timers, vec![1000]);
        assert_eq!(n(&s, &srv, "n"), 1, "one judged tick, then it stops waiting");
        assert_eq!(n(&s, &srv, "f"), 1, "its frame ran once and was quiet");
    });
}

#[test]
fn an_interval_whose_frame_is_not_quiet_does_not_settle() {
    big(|| {
        let (_, _, r, _) = run("<div id=c></div><script>setInterval(function () { requestAnimationFrame(function () { for (var i = 0; i < 20; i++) document.getElementById('c').appendChild(document.createElement('i')) }) }, 1000)</script>", 300);
        assert!(!settled(&r), "20 changes in the frame: {r:?}");
        let (_, srv, r, _) = run("<script>setInterval(function () { requestAnimationFrame(function () { fetch('/p') }) }, 1000)</script>", 300);
        assert!(!settled(&r), "a fetch in the frame: {r:?}");
        assert!(srv.hits("/p") > 3);
    });
}

/// The same for a message: React posts one on a `MessageChannel` at each state change, and an interval that sets state does it at each tick.
#[test]
fn an_interval_that_posts_a_quiet_message_each_tick_settles() {
    big(|| {
        let (s, srv, r, _) = run("<script>var k = 0, c = new MessageChannel(); c.port1.onmessage = function () { localStorage.setItem('m', k) };
            setInterval(function () { k++; localStorage.setItem('n', k); c.port2.postMessage(k) }, 1000)</script>", 5_000);
        assert!(settled(&r), "{r:?}");
        assert_eq!(r.pages[0].idle_timers, vec![1000]);
        assert_eq!(n(&s, &srv, "n"), 1);
        assert_eq!(n(&s, &srv, "m"), 1, "its message ran once");
    });
}

#[test]
fn an_interval_whose_message_is_not_quiet_does_not_settle() {
    big(|| {
        let (_, srv, r, _) = run("<script>var c = new MessageChannel(); c.port1.onmessage = function () { fetch('/p') };
            setInterval(function () { c.port2.postMessage(1) }, 1000)</script>", 300);
        assert!(!settled(&r), "a fetch in the message: {r:?}");
        assert!(srv.hits("/p") > 3);
    });
}

#[test]
fn a_frame_asked_for_after_the_settle_is_not_skipped() {
    big(|| {
        let (mut s, srv, r, _) = run("<button id=go>x</button><script>function f() { requestAnimationFrame(f) } requestAnimationFrame(f);
            document.getElementById('go').addEventListener('click', function () { requestAnimationFrame(function () { localStorage.setItem('frame', '1') }) })</script>", 5_000);
        assert!(settled(&r), "{r:?}");
        let r = s.resume(&[Input::Click("#go")]);
        assert_eq!(r.stop, SessionStop::Page(Stop::Settled), "{r:?}");
        assert_eq!(n(&s, &srv, "frame"), 1);
    });
}

#[test]
fn resume_after_a_settle_still_fires_the_intervals_on_time() {
    big(|| {
        let (mut s, srv, r, _) = run(
            "<button id=go>x</button><script>var k = 0; setInterval(function () { k++; localStorage.setItem('n', k) }, 200);
             document.getElementById('go').addEventListener('click', function () { setTimeout(function () { localStorage.setItem('done', String(k)) }, 700) })</script>",
            5_000,
        );
        assert!(settled(&r), "{r:?}");
        let before = n(&s, &srv, "n");
        let r = s.resume(&[Input::Click("#go")]);
        assert_eq!(r.stop, SessionStop::Page(Stop::Settled), "{r:?}");
        assert_eq!(r.pages[0].idle_timers, vec![200]);
        assert_eq!(n(&s, &srv, "n") - before, 3, "ticks at +200, +400 and +600 of the 700 ms wait");
        assert_eq!(n(&s, &srv, "done"), before + 3, "the one-shot saw all three ticks");
    });
}

#[test]
fn a_cleared_interval_leaves_nothing_to_report() {
    big(|| {
        let (_, _, r, _) = run("<script>var t = setInterval(function () { clearInterval(t) }, 300)</script>", 5_000);
        assert!(settled(&r), "{r:?}");
        assert!(r.pages[0].idle_timers.is_empty() && !r.pages[0].idle_frames);
    });
}

/// The real clock: the page sleeps to the one judged run of the clock, then settles. (The virtual runs above take no time.)
#[test]
fn on_the_real_clock_a_one_second_interval_settles_after_about_one_second() {
    big(|| {
        let srv = server("<div id=c>0</div><script>var k = 0; setInterval(function () { k++; document.getElementById('c').textContent = k }, 1000)</script>");
        let mut c = cfg(srv.url("/"));
        c.virtual_clock = false;
        let mut s = Session::new(c);
        let t = Instant::now();
        let r = s.run();
        let wall = t.elapsed();
        eprintln!("settle test (real clock): stop {:?}, wall {:?}", r.stop, wall);
        assert!(settled(&r), "{r:?}");
        assert!(wall >= Duration::from_millis(900) && wall < Duration::from_secs(3), "{wall:?}");
        assert_eq!(r.pages[0].idle_timers, vec![1000]);
    });
}

#[test]
fn a_timer_that_adds_a_style_sheet_each_run_is_not_quiet_while_it_does() {
    big(|| {
        let srv = serve(|path| match path {
            "/" => Reply::new(200, Some("text/html; charset=utf-8"), "<script>var k = 0; function step() { k++; localStorage.setItem('n', k); if (k <= 3) { var l = document.createElement('link'); l.rel = 'stylesheet'; l.href = '/s' + k + '.css'; document.head.appendChild(l) } setTimeout(step, 400) } step()</script>"),
            _ => Reply::new(200, Some("text/css"), "p { color: red }"),
        });
        let mut c = cfg(srv.url("/"));
        c.limits.max_tasks = 5_000;
        c.limits.max_rounds = 5_000;
        let mut s = Session::new(c);
        let r = s.run();
        assert!(settled(&r), "{r:?}");
        // The three sheets were fetched, so the timer ran the fourth time (no sheet) before the page settled.
        assert_eq!(n(&s, &srv, "n"), 4);
    });
}
