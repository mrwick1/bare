//! `Date` in the browser host: the wall clock against the scheduler clock, and Date objects across scripts, timers, promises and
//! collections. The Date builtin itself is tested in `crates/js/src/eval_date_tests.rs`.

use crate::browser::*;
use bare_html::tree;
use bare_js::eval::{Error, EvalLimits, LimitKind, WallClock};
use bare_js::heap::HeapLimits;
use bare_js::value::Value;

fn big<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(64 << 20).spawn(f).unwrap().join().unwrap()
}

/// Scheduler clock at 1000 ms, wall clock at 2023-11-14T22:13:20.000Z.
const WALL0: f64 = 1_700_000_000_000.0;

fn browser() -> Browser {
    let mut b = Browser::new(tree::parse("<body><p id=p>x</p></body>"), BrowserLimits::default(), EvalLimits { max_steps: 2_000_000, ..Default::default() }, HeapLimits::default()).expect("browser");
    b.use_virtual_clock_at(1000.0).unwrap();
    b.use_virtual_wall_clock_at(WALL0).unwrap();
    b
}

fn js(b: &mut Browser, src: &str) -> String {
    let r = b.run_script(src);
    match r {
        Ok(v) => b.it.display(v),
        Err(Error::Throw(v)) if matches!(v, Value::Object(_)) => format!("throw:{}:{}", b.it.get_str(v, "name"), b.it.get_str(v, "message")),
        Err(Error::Throw(v)) => format!("throw-value:{}", b.it.display(v)),
        Err(Error::Limit(l)) => format!("limit:{l:?}"),
        Err(Error::Unsupported(u)) => format!("unsupported:{}", u.what),
        Err(e) => format!("other:{e:?}"),
    }
}

#[test]
fn the_wall_clock_and_the_monotonic_clock_are_different_things() {
    big(|| {
        let mut b = browser();
        assert_eq!(b.wall_clock(), WallClock::Virtual(WALL0));
        assert_eq!(js(&mut b, "[Date.now(), performance.now()].join()"), "1700000000000,1000");
        // the monotonic clock moves, the wall clock does not follow it
        b.advance_time(5000.0, 100).unwrap();
        assert_eq!(js(&mut b, "[Date.now(), performance.now()].join()"), "1700000000000,6000");
        // the driver steps the wall clock back (a clock correction): performance.now never goes back
        b.use_virtual_wall_clock_at(WALL0 - 60_000.0).unwrap();
        assert_eq!(js(&mut b, "[Date.now(), performance.now()].join()"), "1699999940000,6000");
        b.advance_time(1.5, 100).unwrap();
        assert_eq!(js(&mut b, "[Date.now(), performance.now()].join()"), "1699999940000,6001.5");
        // a Date made earlier is a fixed value
        assert_eq!(js(&mut b, "var early = new Date(); Date.now() - early.getTime()"), "0");
        b.use_virtual_wall_clock_at(WALL0).unwrap();
        assert_eq!(js(&mut b, "Date.now() - early.getTime()"), "60000");
    });
}

#[test]
fn scheduler_elapsed_time_is_never_the_unix_epoch() {
    big(|| {
        // a browser that never set a wall clock: the default is the system clock, and performance.now is a small elapsed time
        let mut b = Browser::new(tree::parse("<body></body>"), BrowserLimits::default(), EvalLimits::default(), HeapLimits::default()).unwrap();
        assert_eq!(b.wall_clock(), WallClock::System);
        let r = js(&mut b, "[Date.now() > 1.7e12, performance.now() < 1e6, Date.now() - performance.now() > 1.7e12].join()");
        assert_eq!(r, "true,true,true");
        // a virtual scheduler clock alone leaves the wall clock on the system clock
        b.use_virtual_clock();
        assert_eq!(b.wall_clock(), WallClock::System);
        assert_eq!(js(&mut b, "var frozenPerf = performance.now(); frozenPerf >= 0"), "true");
        b.advance_time(5.0, 100).unwrap();
        assert_eq!(js(&mut b, "performance.now() - frozenPerf"), "5");
        assert_eq!(js(&mut b, "Date.now() > 1.7e12"), "true");
        b.use_virtual_wall_clock_at(9.0).unwrap();
        assert_eq!(js(&mut b, "Date.now()"), "9");
        b.use_system_wall_clock();
        assert_eq!(js(&mut b, "Date.now() > 1.7e12"), "true");
        // a bad value is refused and the clock stays
        b.use_virtual_wall_clock_at(1.0).unwrap();
        assert!(matches!(b.use_virtual_wall_clock_at(f64::NAN), Err(Error::Unsupported(_))));
        assert_eq!(b.wall_clock(), WallClock::Virtual(1.0));
    });
}

#[test]
fn timers_see_the_wall_clock_the_driver_set_and_dates_cross_scripts_and_collections() {
    big(|| {
        let mut b = browser();
        js(&mut b, "var seen = []; var start = new Date(); var held = new Date(777); setTimeout(function (d) { seen.push('t1:' + (Date.now() - start.getTime()) + ':' + d.getTime()); Promise.resolve(new Date(Date.now())).then(function (x) { seen.push('p1:' + x.getTime()); }); }, 100, new Date(555));");
        b.collect(&[]).unwrap();
        // second script and a second timer, set after the first collection
        js(&mut b, "setTimeout(function () { seen.push('t2:' + (Date.now() - start.getTime())); }, 200);");
        b.collect(&[]).unwrap();
        b.advance_time(100.0, 100).unwrap();
        b.drain().unwrap();
        // the wall clock moved by the driver between the two timers
        b.use_virtual_wall_clock_at(WALL0 + 250.0).unwrap();
        b.advance_time(100.0, 100).unwrap();
        b.drain().unwrap();
        b.collect(&[]).unwrap();
        assert_eq!(js(&mut b, "seen.join('|')"), "t1:0:555|p1:1700000000000|t2:250");
        assert_eq!(js(&mut b, "held.getTime() + ':' + start.getTime()"), "777:1700000000000");
        // the dates that nothing holds any more are gone
        assert_eq!(b.it.date_tracked(), 2, "start and held");
    });
}

#[test]
fn a_date_argument_of_a_pending_timer_is_a_root() {
    big(|| {
        let mut b = browser();
        js(&mut b, "var got = []; setTimeout(function (d, e) { got.push(d.toISOString(), e[0].getTime()); }, 10, new Date(86400000), [new Date(3)]);");
        for _ in 0..3 {
            b.collect(&[]).unwrap();
        }
        assert_eq!(b.it.date_tracked(), 2);
        b.advance_time(10.0, 10).unwrap();
        assert_eq!(js(&mut b, "got.join()"), "1970-01-02T00:00:00.000Z,3");
        b.collect(&[]).unwrap();
        assert_eq!(b.it.date_tracked(), 0);
    });
}

#[test]
fn a_deleted_date_global_does_not_break_timers_that_hold_the_constructor() {
    big(|| {
        let mut b = browser();
        js(&mut b, "var D = Date; var out = []; delete globalThis.Date; setTimeout(function () { out.push(new D(Date_missing()).getTime()); }, 5); function Date_missing() { return 4; }");
        b.collect(&[]).unwrap();
        assert_eq!(js(&mut b, "typeof Date"), "undefined");
        b.advance_time(5.0, 10).unwrap();
        assert_eq!(js(&mut b, "out.join()"), "4");
        // a script that uses the deleted global gets an ordinary ReferenceError, as the cached app did before Date existed
        assert_eq!(js(&mut b, "try { Date.now(); } catch (e) { e.name }"), "ReferenceError");
    });
}

#[test]
fn local_time_stays_a_gate_in_the_browser_and_does_not_stick() {
    big(|| {
        let mut b = browser();
        assert!(js(&mut b, "new Date(2020, 0, 1)").starts_with("unsupported:"));
        // Unsupported is the result of that run only. The wall clock and the realm are unchanged.
        assert_eq!(js(&mut b, "Date.now()"), "1700000000000");
        assert_eq!(js(&mut b, "new Date(Date.UTC(2020, 0, 1)).toISOString()"), "2020-01-01T00:00:00.000Z");
    });
}

#[test]
fn the_date_cap_is_a_limit_not_a_throw() {
    big(|| {
        let mut b = browser();
        b.it.set_sc_limits(bare_js::eval::ScLimits { max_dates: 3, ..Default::default() });
        assert_eq!(js(&mut b, "var a = [new Date(1), new Date(2), new Date(3)]; try { new Date(4); } catch (e) { 'caught' }"), format!("limit:{:?}", LimitKind::Sc(bare_js::eval::ScLimit::Dates)));
    });
}
