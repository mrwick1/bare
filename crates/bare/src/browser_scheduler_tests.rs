//! Tests of the scheduler: timers, `queueMicrotask`, `performance.now`, `MessageChannel`. Real scripts run in a real
//! realm. Time is a virtual clock (no sleeps). Expectations come from the HTML Standard and from hand-written
//! structure; the cases that Node can answer are also in `tools/js/scheduler_check.sh`. Each test runs on a big stack.

use crate::browser::*;
use bare_html::tree;
use bare_js::eval::{Error, EvalLimits, LimitKind};
use bare_js::heap::HeapLimits;
use bare_js::value::Value;

fn big<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(64 << 20).spawn(f).unwrap().join().unwrap()
}

const T0: f64 = 1000.0;

fn make(steps: u64, lim: BrowserLimits) -> Browser {
    let mut b = Browser::new(tree::parse("<body><p id=p>x</p></body>"), lim, EvalLimits { max_steps: steps, ..Default::default() }, HeapLimits::default()).expect("browser");
    b.use_virtual_clock_at(T0).unwrap();
    b
}

fn browser() -> Browser {
    make(2_000_000, BrowserLimits::default())
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

fn host_limit(name: &'static str) -> String {
    format!("limit:{:?}", LimitKind::Host(name))
}

fn adv(b: &mut Browser, ms: f64) -> Pump {
    b.advance_time(ms, 1_000_000).expect("advance")
}

#[test]
fn timers_fire_by_due_time_then_queue_order_with_webidl_delay_conversion() {
    big(|| {
        let mut b = browser();
        js(&mut b, "var log = []; function t(n) { return function () { log.push(n + '@' + (performance.now() - 1000)) } }");
        let r = js(&mut b, "setTimeout(t('a'), 10); setTimeout(t('b'), 5); setTimeout(t('c'), 5); setTimeout(t('d'), 0); setTimeout(t('e'), -3); setTimeout(t('f'), 1.9); setTimeout(t('g'), NaN); setTimeout(t('h'), '7'); setTimeout(t('i'), 2147483648); setTimeout(t('j'), 4294967305); setTimeout(t('k'))");
        assert_eq!(r, "11");
        // nothing runs by itself
        assert_eq!(js(&mut b, "log.length"), "0");
        assert_eq!(b.sched_stats().timers, 11);
        let p = adv(&mut b, 100.0);
        assert_eq!(p, Pump { ran: 11, capped: false, next_due: None });
        assert_eq!(js(&mut b, "log.join()"), "d@0,e@0,g@0,i@0,k@0,f@1,b@5,c@5,h@7,j@9,a@10");
        assert_eq!(b.now(), T0 + 100.0);
        let s = b.sched_stats();
        assert_eq!((s.timers, s.tasks), (0, 0));
    });
}

#[test]
fn ids_are_numbers_never_reused_and_clear_cancels() {
    big(|| {
        let mut b = browser();
        assert_eq!(js(&mut b, "var log = []; var a = setTimeout(function () { log.push('a') }, 5); var b = setInterval(function () { log.push('b') }, 5); [typeof a, a, b].join()"), "number,1,2");
        assert_eq!(js(&mut b, "clearTimeout(a); clearInterval(a); clearTimeout(999); clearTimeout(); clearTimeout(null); clearTimeout('zz'); clearTimeout({}); 0"), "0");
        assert_eq!(b.sched_stats().timers, 1);
        adv(&mut b, 12.0);
        assert_eq!(js(&mut b, "log.join()"), "b,b");
        // clearTimeout and clearInterval share ids
        js(&mut b, "clearTimeout(b)");
        assert_eq!(b.sched_stats(), SchedStats { ..Default::default() });
        // ids go on after a clear and after a run: nothing is reused
        assert_eq!(js(&mut b, "var c = setTimeout(function () {}, 0); clearTimeout(c); var d = setTimeout(function () {}, 0); [c, d].join()"), "3,4");
        assert_eq!(js(&mut b, "clearTimeout(3); clearTimeout(1); 1"), "1");
        assert_eq!(b.sched_stats().timers, 1);
    });
}

#[test]
fn interval_cleared_by_its_own_callback_is_not_queued_again_and_args_and_this_pass() {
    big(|| {
        let mut b = browser();
        js(&mut b, "var log = []; var n = 0; var id = setInterval(function () { n++; log.push('i' + n); if (n == 3) clearInterval(id) }, 10)");
        js(&mut b, "setTimeout(function (a, b) { log.push(a + b); log.push(this === window) }, 15, 'x', 2)");
        let p = adv(&mut b, 100.0);
        assert_eq!(p.ran, 4);
        assert_eq!(js(&mut b, "log.join()"), "i1,x2,true,i2,i3");
        assert_eq!(b.sched_stats().tasks, 0);
        // a timer cleared by an earlier timer in the same pump never runs
        js(&mut b, "var k = setTimeout(function () { log.push('late') }, 20); setTimeout(function () { clearTimeout(k) }, 10)");
        adv(&mut b, 100.0);
        assert_eq!(js(&mut b, "log.length"), "5");
    });
}

#[test]
fn nested_timers_get_the_four_ms_clamp_from_level_six() {
    big(|| {
        let mut b = browser();
        js(&mut b, "var log = [], n = 0; function f() { n++; log.push(performance.now() - 1000); if (n < 8) setTimeout(f, 0) } setTimeout(f, 0)");
        adv(&mut b, 100.0);
        assert_eq!(js(&mut b, "log.join()"), "0,0,0,0,0,4,8,12");
        // an interval is clamped the same way after its fifth run
        js(&mut b, "var log2 = [], m = 0, t0 = performance.now(); var id = setInterval(function () { m++; log2.push(performance.now() - t0); if (m == 7) clearInterval(id) }, 1)");
        adv(&mut b, 100.0);
        assert_eq!(js(&mut b, "log2.join()"), "1,2,3,4,5,9,13");
        // a timer set from outside a timer task starts again at level 1
        js(&mut b, "var log3 = []; setTimeout(function () { log3.push(performance.now() - 1000) }, 0)");
        let before = b.now();
        adv(&mut b, 1.0);
        assert_eq!(js(&mut b, "log3.length"), "1");
        assert_eq!(b.now(), before + 1.0);
    });
}

#[test]
fn bad_callbacks_are_explicit() {
    big(|| {
        let mut b = browser();
        assert_eq!(js(&mut b, "setTimeout('x = 1', 0)"), "unsupported:setTimeout with a string callback (eval)");
        assert_eq!(js(&mut b, "setInterval('x = 1', 0)"), "unsupported:setInterval with a string callback (eval)");
        assert_eq!(js(&mut b, "setTimeout(5, 0)"), "unsupported:setTimeout with a non-function callback");
        assert_eq!(js(&mut b, "setTimeout()"), "throw:TypeError:Failed to execute 'setTimeout' on 'Window': 1 argument required, but only 0 present.");
        assert_eq!(js(&mut b, "setTimeout.call({}, function () {}, 0)"), "throw:TypeError:Illegal invocation");
        assert_eq!(js(&mut b, "typeof setTimeout.call(undefined, function () {}, 0)"), "number");
        assert_eq!(b.sched_stats().timers, 1);
    });
}

#[test]
fn a_callback_runs_as_its_own_task_with_a_checkpoint_and_a_throw_is_reported() {
    big(|| {
        let mut b = browser();
        js(&mut b, "var log = []");
        js(&mut b, "setTimeout(function () { Promise.resolve().then(function () { log.push('m1') }); queueMicrotask(function () { log.push('m2') }); log.push('t1') }, 0)");
        js(&mut b, "setTimeout(function () { throw new Error('boom') }, 0); setTimeout(function () { log.push('t3') }, 0)");
        js(&mut b, "setInterval(function () { throw 7 }, 50)");
        let p = adv(&mut b, 60.0);
        assert_eq!(p.ran, 4);
        assert_eq!(js(&mut b, "log.join()"), "t1,m1,m2,t3");
        let r = b.take_reports();
        assert_eq!(r.iter().map(|r| (r.event_type.as_str(), r.target.as_str(), r.message.as_str())).collect::<Vec<_>>(), vec![("timeout", "window", "Error: boom"), ("interval", "window", "7")]);
        // the interval that threw is queued again
        assert_eq!(b.sched_stats().timers, 1);
        assert!(p.next_due.is_some());
    });
}

#[test]
fn a_limit_in_a_task_stops_the_pump_and_no_more_js_runs() {
    big(|| {
        let mut b = make(200_000, BrowserLimits::default());
        js(&mut b, "var log = []; setTimeout(function () { for (;;) {} }, 0); setTimeout(function () { log.push('after') }, 0); setInterval(function () { log.push('iv') }, 1)");
        assert!(matches!(b.advance_time(50.0, 1000), Err(Error::Limit(LimitKind::Steps))));
        assert!(b.sched_stats().halted);
        assert!(matches!(b.advance_time(50.0, 1000), Err(Error::Limit(LimitKind::Steps))));
        assert!(matches!(b.run_scheduled_tasks(10), Err(Error::Limit(LimitKind::Steps))));
        // the realm still answers scripts, and none of the later tasks ran
        assert_eq!(js(&mut b, "log.length"), "0");
        // a Limit of a microtask in a task is the same
        let mut b = make(200_000, BrowserLimits::default());
        js(&mut b, "var log = []; setTimeout(function () { queueMicrotask(function () { for (;;) {} }) }, 0); setTimeout(function () { log.push('after') }, 1)");
        assert!(matches!(b.advance_time(5.0, 10), Err(Error::Limit(LimitKind::Steps))));
        assert!(matches!(b.advance_time(5.0, 10), Err(Error::Limit(LimitKind::Steps))));
        assert_eq!(js(&mut b, "log.length"), "0");
        // Unsupported inside a task also stops the pump
        let mut b = browser();
        js(&mut b, "var log = []; setTimeout(function () { new MessageChannel().port1.postMessage({}) }, 0); setTimeout(function () { log.push('after') }, 1)");
        assert!(matches!(b.advance_time(5.0, 10), Err(Error::Unsupported(_))));
        assert!(matches!(b.advance_time(5.0, 10), Err(Error::Unsupported(_))));
        assert_eq!(js(&mut b, "log.length"), "0");
    });
}

#[test]
fn pump_is_capped_and_resumes() {
    big(|| {
        let mut b = browser();
        js(&mut b, "var n = 0; for (var i = 0; i < 10; i++) setTimeout(function () { n++ }, 0); setTimeout(function () { n += 100 }, 50)");
        let p = b.run_scheduled_tasks(4).unwrap();
        assert_eq!(p, Pump { ran: 4, capped: true, next_due: Some(T0) });
        assert_eq!(b.run_scheduled_tasks(4).unwrap().ran, 4);
        let p = b.run_scheduled_tasks(4).unwrap();
        assert_eq!(p, Pump { ran: 2, capped: false, next_due: Some(T0 + 50.0) });
        assert_eq!(js(&mut b, "n"), "10");
        // advance_to stops at the cap, with the clock at the last task, and goes on from there
        let p = b.advance_to(T0 + 100.0, 0).unwrap();
        assert_eq!((p.ran, p.capped), (0, true), "the task at +50 is due by the target but the cap is 0");
        let p = b.advance_to(T0 + 100.0, 1).unwrap();
        assert_eq!((p.ran, p.capped, p.next_due), (1, false, None));
        assert_eq!(js(&mut b, "n"), "110");
        assert_eq!(b.now(), T0 + 100.0);
        assert!(b.advance_to(f64::NAN, 1).is_err());
        assert!(b.use_virtual_clock_at(1.0).is_err());
    });
}

#[test]
fn performance_now_follows_the_clock_and_the_monotonic_clock_never_goes_back() {
    big(|| {
        let mut b = browser();
        assert_eq!(js(&mut b, "performance.now()"), "1000");
        adv(&mut b, 2.5);
        assert_eq!(js(&mut b, "performance.now()"), "1002.5");
        assert_eq!(js(&mut b, "var e = new Event('x'); e.timeStamp"), "1002.5");
        // the default clock is monotonic and real; no sleep happens
        let mut m = Browser::new(tree::parse("<body></body>"), BrowserLimits::default(), EvalLimits::default(), HeapLimits::default()).unwrap();
        assert!(matches!(m.advance_to(5.0, 1), Err(Error::Unsupported(_))));
        assert_eq!(js(&mut m, "var a = performance.now(), b = performance.now(); var log = []; setTimeout(function () { log.push('now') }, 0); setTimeout(function () { log.push('far') }, 3600000); a <= b && a >= 0"), "true");
        let p = m.run_scheduled_tasks(100).unwrap();
        assert_eq!(p.ran, 1);
        assert!(p.next_due.unwrap() > m.now() + 3_000_000.0);
        assert_eq!(js(&mut m, "log.join()"), "now");
        // nothing is due: the pump returns at once
        assert_eq!(m.run_scheduled_tasks(100).unwrap().ran, 0);
    });
}

#[test]
fn queue_microtask_is_a_real_fifo_job() {
    big(|| {
        let mut b = browser();
        js(&mut b, "var log = []; Promise.resolve().then(function () { log.push('p1') }); queueMicrotask(function () { log.push('q1'); queueMicrotask(function () { log.push('q3') }) }); Promise.resolve().then(function () { log.push('p2') }); queueMicrotask(function () { log.push('q2') }); log.push('sync')");
        assert_eq!(js(&mut b, "log.join()"), "sync");
        assert_eq!(b.it.pending_jobs(), 4);
        assert_eq!(b.drain().unwrap(), 5);
        assert_eq!(js(&mut b, "log.join()"), "sync,p1,q1,p2,q2,q3");
        assert_eq!(js(&mut b, "queueMicrotask()"), "throw:TypeError:Failed to execute 'queueMicrotask' on 'Window': parameter 1 is not of type 'Function'.");
        assert_eq!(js(&mut b, "queueMicrotask('x = 1')"), "throw:TypeError:Failed to execute 'queueMicrotask' on 'Window': parameter 1 is not of type 'Function'.");
        assert_eq!(b.it.pending_jobs(), 0);
    });
}

#[test]
fn queue_microtask_throw_is_reported_and_later_jobs_run_but_a_limit_poisons() {
    big(|| {
        let mut b = make(200_000, BrowserLimits::default());
        js(&mut b, "var log = []; queueMicrotask(function () { throw new RangeError('r') }); queueMicrotask(function () { log.push('next') })");
        assert_eq!(b.drain().unwrap(), 2);
        assert_eq!(js(&mut b, "log.join()"), "next");
        let r = b.take_reports();
        assert_eq!(r.len(), 1);
        assert_eq!((r[0].event_type.as_str(), r[0].message.as_str()), ("microtask", "RangeError: r"));
        js(&mut b, "queueMicrotask(function () { for (;;) {} }); queueMicrotask(function () { log.push('never') })");
        assert!(matches!(b.drain(), Err(Error::Limit(LimitKind::Steps))));
        assert!(matches!(b.drain(), Err(Error::Limit(LimitKind::Steps))));
        assert_eq!(js(&mut b, "log.join()"), "next");
        // the pump repeats the poisoned checkpoint and runs no timer
        js(&mut b, "setTimeout(function () { log.push('timer') }, 0)");
        assert!(matches!(b.run_scheduled_tasks(5), Err(Error::Limit(LimitKind::Steps))));
        assert_eq!(js(&mut b, "log.join()"), "next");
    });
}

#[test]
fn microtasks_left_by_a_script_run_before_the_first_task() {
    big(|| {
        let mut b = browser();
        js(&mut b, "var log = []; setTimeout(function () { log.push('t') }, 0); queueMicrotask(function () { log.push('m') })");
        adv(&mut b, 1.0);
        assert_eq!(js(&mut b, "log.join()"), "m,t");
    });
}

#[test]
fn message_channel_delivers_in_order_as_tasks_with_real_event_fields() {
    big(|| {
        let mut b = browser();
        js(&mut b, "var log = [], ev; var c = new MessageChannel(); c.port1.onmessage = function (e) { ev = ev || e; log.push('m:' + e.data) }");
        js(&mut b, "c.port2.postMessage('a'); c.port2.postMessage(null); c.port2.postMessage(7); log.push('sync')");
        assert_eq!(b.sched_stats().queued_messages, 3);
        let p = adv(&mut b, 0.0);
        assert_eq!(p.ran, 3);
        assert_eq!(js(&mut b, "log.join()"), "sync,m:a,m:null,m:7");
        assert_eq!(js(&mut b, "[ev instanceof MessageEvent, ev instanceof Event, ev.type, ev.target === c.port1, ev.currentTarget === null, ev.origin === '', ev.lastEventId === '', ev.source === null, ev.ports.length, ev.ports === ev.ports, ev.bubbles, ev.cancelable, ev.eventPhase].join()"), "true,true,message,true,true,true,true,true,0,true,false,false,0");
        // stable wrappers and an entangled pair
        assert_eq!(js(&mut b, "[c.port1 === c.port1, c.port1 !== c.port2, c.port1 instanceof MessagePort, c.port1 instanceof EventTarget, c instanceof MessageChannel, typeof c.port1.postMessage].join()"), "true,true,true,true,true,function");
        assert_eq!(js(&mut b, "c.port1.onmessage === c.port1.onmessage && typeof c.port1.onmessage"), "function");
        assert_eq!(js(&mut b, "c.port2.onmessage"), "null");
        // primitives round-trip exactly
        js(&mut b, "var got = []; c.port2.onmessage = function (e) { got.push(e.data) }; c.port1.postMessage(-0); c.port1.postMessage(NaN); c.port1.postMessage('\\ud800x'); c.port1.postMessage(undefined); c.port1.postMessage(true)");
        adv(&mut b, 0.0);
        assert_eq!(js(&mut b, "[1 / got[0], got[1] !== got[1], got[2].charCodeAt(0), got[2].length, got[3], got[4], got.length].join()"), "-Infinity,true,55296,2,,true,5");
    });
}

#[test]
fn message_ports_wait_for_start_and_ping_pong_makes_new_tasks() {
    big(|| {
        let mut b = browser();
        js(&mut b, "var log = []; var c = new MessageChannel(); c.port1.addEventListener('message', function (e) { log.push('l:' + e.data) }); c.port2.postMessage('w1'); c.port2.postMessage('w2')");
        // addEventListener does not start the port: the messages wait in the port
        adv(&mut b, 10.0);
        assert_eq!(js(&mut b, "log.length"), "0");
        assert_eq!((b.sched_stats().queued_messages, b.sched_stats().tasks), (2, 0));
        js(&mut b, "c.port1.start(); c.port2.postMessage('w3')");
        let p = adv(&mut b, 0.0);
        assert_eq!(p.ran, 3);
        assert_eq!(js(&mut b, "log.join()"), "l:w1,l:w2,l:w3");
        // ping-pong: each reply is a later task, after the messages already queued
        js(&mut b, "var d = new MessageChannel(), order = []; d.port1.onmessage = function (e) { order.push('1:' + e.data); if (e.data < 3) d.port1.postMessage(e.data + 1) }; d.port2.onmessage = function (e) { order.push('2:' + e.data); d.port2.postMessage(e.data + 1) }; d.port2.postMessage(0); d.port1.postMessage(10)");
        adv(&mut b, 0.0);
        assert_eq!(js(&mut b, "order.join()"), "1:0,2:10,2:1,1:11,1:2,2:3,1:4");
        // order against timers: queue order at one due time
        js(&mut b, "var o2 = []; var e2 = new MessageChannel(); e2.port1.onmessage = function () { o2.push('msg') }; setTimeout(function () { o2.push('t0') }, 0); e2.port2.postMessage(1); setTimeout(function () { o2.push('t5') }, 5)");
        adv(&mut b, 10.0);
        assert_eq!(js(&mut b, "o2.join()"), "t0,msg,t5");
    });
}

#[test]
fn onmessage_setter_replaces_in_place_clears_and_an_unset_port_drops_messages() {
    big(|| {
        let mut b = browser();
        js(&mut b, "var log = []; var c = new MessageChannel(); var p = c.port1; function f1() { log.push('f1') } function f2() { log.push('f2') }");
        js(&mut b, "p.onmessage = f1; p.addEventListener('message', function () { log.push('L') }); p.onmessage = f2; c.port2.postMessage(1)");
        adv(&mut b, 0.0);
        assert_eq!(js(&mut b, "log.join()"), "f2,L");
        assert_eq!(js(&mut b, "p.onmessage === f2"), "true");
        js(&mut b, "p.onmessage = null; p.onmessage = 5; log.length = 0; c.port2.postMessage(2)");
        adv(&mut b, 0.0);
        assert_eq!(js(&mut b, "[p.onmessage, log.join()].join('|')"), "|L");
        assert_eq!(b.sched_stats().queued_messages, 0);
        assert_eq!(js(&mut b, "var h = { handleEvent: function () {} }; p.onmessage = h; p.onmessage === null"), "true");
    });
}

#[test]
fn close_disentangles_drops_later_posts_and_keeps_what_was_queued_before() {
    big(|| {
        let mut b = browser();
        js(&mut b, "var log = []; var c = new MessageChannel(); c.port1.onmessage = function (e) { log.push('1:' + e.data) }; c.port2.onmessage = function (e) { log.push('2:' + e.data) }");
        js(&mut b, "c.port2.postMessage('x'); c.port1.postMessage('z'); c.port1.close(); c.port2.postMessage('y'); c.port1.postMessage('after'); c.port1.close()");
        let s = b.sched_stats();
        assert_eq!((s.queued_messages, s.tasks), (2, 2), "'y' and 'after' were dropped at once");
        adv(&mut b, 0.0);
        assert_eq!(js(&mut b, "log.join()"), "1:x,2:z");
        let s = b.sched_stats();
        assert_eq!((s.queued_messages, s.queued_message_bytes, s.tasks), (0, 0, 0));
        // a closed port can be closed again; posts to it are dropped
        js(&mut b, "c.port1.start(); c.port2.close(); c.port2.postMessage('q'); c.port1.postMessage('q')");
        adv(&mut b, 0.0);
        assert_eq!(js(&mut b, "log.join()"), "1:x,2:z");
        // closing from a listener does not drop what is already queued, and posts after it are dropped
        js(&mut b, "var l2 = [], l3 = [], d = new MessageChannel(); d.port2.onmessage = function (e) { l3.push(e.data) }; d.port1.onmessage = function (e) { l2.push(e.data); d.port1.close(); d.port1.postMessage('back') }; d.port2.postMessage('a'); d.port2.postMessage('b'); d.port2.postMessage('c')");
        adv(&mut b, 0.0);
        assert_eq!(js(&mut b, "l2.join() + '|' + l3.length"), "a,b,c|0");
        assert_eq!(b.sched_stats().queued_messages, 0);
    });
}

#[test]
fn payload_and_misuse_gates_are_explicit() {
    big(|| {
        let mut b = browser();
        js(&mut b, "var c = new MessageChannel(); var p = c.port1");
        assert_eq!(js(&mut b, "p.postMessage({})"), "unsupported:MessagePort.postMessage of an object (structured clone)");
        assert_eq!(js(&mut b, "p.postMessage([1])"), "unsupported:MessagePort.postMessage of an object (structured clone)");
        assert_eq!(js(&mut b, "try { p.postMessage(function () {}) } catch (e) { [e.name, e.code, e instanceof Error].join() }"), "DataCloneError,25,true");
        assert_eq!(js(&mut b, "p.postMessage('a', [1])"), "unsupported:MessagePort.postMessage transfer list");
        assert_eq!(js(&mut b, "p.postMessage('a', { transfer: [1] })"), "unsupported:MessagePort.postMessage transfer list");
        assert_eq!(js(&mut b, "p.postMessage('a', []) === undefined && p.postMessage('a', { transfer: [] }) === undefined && p.postMessage('a', {}) === undefined && p.postMessage('a', null) === undefined"), "true");
        assert_eq!(js(&mut b, "p.postMessage('a', 5)"), "throw:TypeError:Failed to execute 'postMessage' on 'MessagePort': The transfer argument is not an object.");
        assert_eq!(js(&mut b, "p.postMessage()"), "throw:TypeError:Failed to execute 'postMessage' on 'MessagePort': 1 argument required, but only 0 present.");
        assert_eq!(js(&mut b, "new MessageEvent('message')"), "throw:TypeError:MessageEvent is not a constructor");
        assert_eq!(js(&mut b, "new MessagePort()"), "throw:TypeError:MessagePort is not a constructor");
        assert_eq!(js(&mut b, "MessageChannel()"), "throw:TypeError:Failed to construct: please use the 'new' operator");
        assert_eq!(js(&mut b, "MessagePort.prototype.postMessage.call({}, 1)"), "throw:TypeError:Illegal invocation");
        assert_eq!(js(&mut b, "Object.getOwnPropertyDescriptor(MessagePort.prototype, 'onmessage').get.call(document)"), "throw:TypeError:Illegal invocation");
        // nothing from the failed posts was queued
        assert_eq!(b.sched_stats().queued_messages, 4);
    });
}

#[test]
fn limits_are_checked_before_growth() {
    big(|| {
        let mut b = browser();
        b.set_sched_limits(SchedLimits { max_timers: 3, max_timer_args: 2, max_ports: 2, max_queued_messages: 2, max_queued_message_bytes: 100 });
        assert_eq!(js(&mut b, "setTimeout(function () {}, 0, 1, 2, 3)"), host_limit("timer-args"));
        assert_eq!(js(&mut b, "var t1 = setTimeout(function () {}, 0); setTimeout(function () {}, 0); setInterval(function () {}, 5); 0"), "0");
        assert_eq!(js(&mut b, "setTimeout(function () {}, 0)"), host_limit("timers"));
        js(&mut b, "clearTimeout(t1)");
        assert_eq!(js(&mut b, "typeof setTimeout(function () {}, 0)"), "number");
        assert_eq!(js(&mut b, "var c = new MessageChannel(); new MessageChannel()"), host_limit("message-ports"));
        assert_eq!(js(&mut b, "c.port1.postMessage('a'); c.port1.postMessage('b'); c.port1.postMessage('c')"), host_limit("messages"));
        assert_eq!(b.sched_stats().queued_messages, 2);
        let mut b = browser();
        b.set_sched_limits(SchedLimits { max_queued_message_bytes: 100, ..Default::default() });
        assert_eq!(js(&mut b, "var c = new MessageChannel(); c.port1.postMessage('a'); c.port1.postMessage('0123456789012345678901234567890123456789')"), host_limit("message-bytes"));
        let s = b.sched_stats();
        assert_eq!((s.queued_messages, s.queued_message_bytes), (1, 18));
        // the wrapper cap also bounds ports
        let mut b = make(2_000_000, BrowserLimits { max_wrappers: 4, ..Default::default() });
        assert_eq!(js(&mut b, "new MessageChannel(); new MessageChannel()"), host_limit("dom-wrappers"));
    });
}

#[test]
fn timers_and_queued_messages_keep_their_callbacks_alive_across_collect_and_scripts() {
    big(|| {
        let mut b = browser();
        // script 1 defines state, script 2 schedules, script 3 posts. Nothing but the queues holds the closures.
        js(&mut b, "var log = []; function mk(tag, n) { var big = { tag: tag, items: [1, 2, 3] }; return function (a) { log.push(tag + ':' + big.items.length + ':' + (a === undefined ? '-' : a.v === undefined ? a : a.v)) } }");
        js(&mut b, "setTimeout(mk('t', 0), 5, 'arg'); setInterval(mk('i'), 20); var ic = setInterval(mk('i2'), 100); clearInterval(ic); 0");
        js(&mut b, "(function () { var c = new MessageChannel(); c.port1.onmessage = function (e) { log.push('m:' + e.data) }; c.port2.postMessage('payload' + 1); c.port2.postMessage('payload' + 2) })(); queueMicrotask(mk('mt'))");
        b.collect(&[]).unwrap();
        let s = b.sched_stats();
        assert_eq!((s.timers, s.ports, s.queued_messages), (2, 2, 2));
        b.drain().unwrap();
        b.collect(&[]).unwrap();
        adv(&mut b, 45.0);
        assert_eq!(js(&mut b, "log.join()"), "mt:3:-,m:payload1,m:payload2,t:3:arg,i:3:-,i:3:-");
        b.collect(&[]).unwrap();
        let s = b.sched_stats();
        assert_eq!((s.timers, s.tasks, s.queued_messages, s.queued_message_bytes), (1, 1, 0, 0));
        // the unreachable channel is gone with its listener; the interval keeps running
        assert_eq!(s.ports, 0);
        assert_eq!(b.stats().listeners, 0);
        adv(&mut b, 20.0);
        assert_eq!(js(&mut b, "log.length"), "7");
    });
}

#[test]
fn unreachable_channels_prune_with_their_queued_messages_and_slots_do_not_alias() {
    big(|| {
        let mut b = browser();
        js(&mut b, "var keep = new MessageChannel(); var kept = []; keep.port1.onmessage = function (e) { kept.push(e.data) }");
        for round in 0..5 {
            // 100 channels, each port with a listener that does not start it and a queued payload: only garbage
            js(&mut b, "(function () { for (var i = 0; i < 100; i++) { var c = new MessageChannel(); c.port1.addEventListener('message', function () {}); c.port2.addEventListener('message', function () {}); c.port2.postMessage('x' + i); c.port1.postMessage('y' + i); } })()");
            let s = b.sched_stats();
            assert_eq!((s.ports, s.queued_messages), (202, 200), "round {round}");
            b.collect(&[]).unwrap();
            let s = b.sched_stats();
            assert_eq!((s.ports, s.queued_messages, s.queued_message_bytes, s.tasks), (2, 0, 0, 0), "round {round}");
            assert_eq!(b.stats().listeners, 1, "round {round}");
            // the live channel still works after the slots were reused
            js(&mut b, &format!("keep.port2.postMessage({round})"));
            adv(&mut b, 0.0);
        }
        assert_eq!(js(&mut b, "kept.join()"), "0,1,2,3,4");
        // an old port handle in the script still names its own port, never a new one
        assert_eq!(js(&mut b, "var old = keep.port2; keep = null; (function () { for (var i = 0; i < 50; i++) new MessageChannel() })(); old.postMessage('z'); typeof old"), "object");
    });
}

#[test]
fn burst_with_collect_has_no_queue_or_metadata_drift() {
    big(|| {
        let mut b = browser();
        js(&mut b, "var sum = 0, count = 0, fired = 0; var c = new MessageChannel(); c.port1.onmessage = function (e) { sum += e.data; count++ }");
        let mut expect = 0u64;
        for round in 0..20u64 {
            js(&mut b, &format!("for (var i = 0; i < 300; i++) c.port2.postMessage(i * {}); for (var i = 0; i < 100; i++) setTimeout(function () {{ fired++ }}, i % 7)", round + 1));
            expect += (0..300u64).map(|i| i * (round + 1)).sum::<u64>();
            let mut total = 0;
            loop {
                let p = adv_capped(&mut b, 128);
                total += p.ran;
                b.collect(&[]).unwrap();
                if !p.capped {
                    break;
                }
            }
            assert_eq!(total, 400, "round {round}");
            let s = b.sched_stats();
            assert_eq!((s.timers, s.tasks, s.queued_messages, s.queued_message_bytes), (0, 0, 0, 0), "round {round}");
            assert_eq!(s.message_events, 0);
            assert_eq!(b.stats().events, 0);
        }
        assert_eq!(js(&mut b, "[count, fired, sum].join()"), format!("6000,2000,{expect}"));
        assert_eq!(b.stats().wrappers, 4, "window, document, two ports");
    });
}

fn adv_capped(b: &mut Browser, max: usize) -> Pump {
    b.advance_time(10.0, max).expect("advance")
}

#[test]
fn a_message_event_keeps_its_data_until_collect() {
    big(|| {
        let mut b = browser();
        // events are freed only by collect, so a held event keeps its data
        js(&mut b, "var held; var c = new MessageChannel(); c.port1.onmessage = function (e) { held = e }; c.port2.postMessage('data' + 5)");
        adv(&mut b, 0.0);
        b.collect(&[]).unwrap();
        assert_eq!(js(&mut b, "held.data + held.ports.length"), "data50");
        assert_eq!(b.sched_stats().message_events, 1);
        js(&mut b, "held = null");
        b.collect(&[]).unwrap();
        assert_eq!(b.sched_stats().message_events, 0);
    });
}

#[test]
fn a_throwing_message_listener_is_reported_and_later_messages_still_arrive() {
    big(|| {
        let mut b = browser();
        js(&mut b, "var log = []; var c = new MessageChannel(); c.port1.onmessage = function (e) { if (e.data === 1) throw new TypeError('bad'); log.push(e.data) }; c.port2.postMessage(0); c.port2.postMessage(1); c.port2.postMessage(2)");
        assert_eq!(adv(&mut b, 0.0).ran, 3);
        assert_eq!(js(&mut b, "log.join()"), "0,2");
        let r = b.take_reports();
        assert_eq!(r.iter().map(|r| (r.event_type.as_str(), r.target.as_str(), r.message.as_str())).collect::<Vec<_>>(), vec![("message", "object", "TypeError: bad")]);
        // a message listener that makes a microtask: it runs before the next message
        js(&mut b, "var o = []; c.port1.onmessage = function (e) { o.push('m' + e.data); queueMicrotask(function () { o.push('q' + e.data) }) }; c.port2.postMessage(1); c.port2.postMessage(2)");
        adv(&mut b, 0.0);
        assert_eq!(js(&mut b, "o.join()"), "m1,q1,m2,q2");
    });
}

#[test]
fn pump_observes_a_failed_empty_microtask_checkpoint() {
    big(|| {
        let mut b = make(200, BrowserLimits::default());
        js(&mut b, "var fired = 0; setTimeout(function(){fired++}, 0); queueMicrotask(function(){while(true){}});");
        assert!(matches!(b.drain(), Err(Error::Limit(LimitKind::Steps))));
        assert_eq!(b.it.pending_jobs(), 0);
        assert!(matches!(b.run_scheduled_tasks(1), Err(Error::Limit(LimitKind::Steps))));
        let k = b.it.heap.key_from_str("fired").unwrap();
        assert!(matches!(b.it.heap.get(b.it.global_object(), k), Ok(bare_js::heap::Get::Value(Value::Number(0.0)))));
    });
}

const SVC: &str = "var log = [];
    class Svc { #n = 0; #enc = new TextEncoder(); #dec = new TextDecoder();
      async handle(tag, payload) {
        await null;
        this.#n++;
        var bytes = this.#enc.encode(JSON.stringify({ tag: tag, n: this.#n, payload: payload }));
        await Promise.resolve();
        var obj = JSON.parse(this.#dec.decode(bytes));
        var m = /a(b+)/.exec(obj.payload);
        var sum = 0; for (var x of bytes) sum += x;
        log.push(obj.tag + obj.n + ':' + m[1].length + ':' + bytes.length + ':' + sum);
        return obj.n;
      } }
    var svc = new Svc();";

#[test]
fn async_class_callbacks_from_timers_microtasks_and_messages_use_json_regexps_and_bytes() {
    big(|| {
        let mut b = browser();
        js(&mut b, SVC);
        js(&mut b, "setTimeout(function () { svc.handle('t', 'abbb'); log.push('timer'); }, 5);
                    queueMicrotask(function () { svc.handle('q', 'ab'); log.push('micro'); });
                    var ch = new MessageChannel(); ch.port1.onmessage = function (e) { svc.handle('m', e.data); log.push('msg'); };
                    ch.port2.postMessage('abbbb');
                    Promise.resolve().then(function () { log.push('p'); });
                    log.push('sync'); 0");
        assert_eq!(js(&mut b, "log.join()"), "sync");
        b.drain().unwrap();
        // the microtask queue ran: queueMicrotask, then the Promise job, then both continuations of the async method
        assert_eq!(js(&mut b, "log.join()"), "sync,micro,p,q1:1:32:2379");
        let p = adv(&mut b, 5.0);
        assert_eq!(p.ran, 2);
        // each task is followed by its own checkpoint, which finishes the async continuations of that task
        assert_eq!(js(&mut b, "log.join()"), "sync,micro,p,q1:1:32:2379,msg,m2:4:35:2670,timer,t3:3:34:2580");
        assert_eq!(b.it.pending_jobs(), 0);
        assert_eq!(b.it.async_stats().activations, 0);
        // collection keeps nothing the page cannot reach, and the class instance stays valid
        b.collect(&[]).unwrap();
        assert_eq!(js(&mut b, "svc.handle('z', 'abb'); 0"), "0");
        b.drain().unwrap();
        assert_eq!(js(&mut b, "log[log.length - 1]"), "z4:2:33:2489");
    });
}

#[test]
fn a_limit_in_a_resumed_class_method_is_sticky_and_replays_no_effect() {
    big(|| {
        let mut b = make(200_000, BrowserLimits::default());
        js(&mut b, "var log = []; class A { async m() { await null; log.push('m'); for (;;) {} } }
                    setTimeout(function () { new A().m(); }, 0); setTimeout(function () { log.push('later'); }, 1);
                    queueMicrotask(function () { log.push('micro'); }); 0");
        // the script's own checkpoint ran `micro`; the timer task starts the method and its continuation hits the cap
        b.drain().unwrap();
        assert!(matches!(b.advance_time(5.0, 10), Err(Error::Limit(LimitKind::Steps))));
        assert_eq!(js(&mut b, "log.join()"), "micro,m");
        // every later drain and pump repeats the error and runs nothing, also with an empty queue
        assert!(matches!(b.drain(), Err(Error::Limit(LimitKind::Steps))));
        assert!(matches!(b.advance_time(5.0, 10), Err(Error::Limit(LimitKind::Steps))));
        assert!(matches!(b.run_scheduled_tasks(10), Err(Error::Limit(LimitKind::Steps))));
        assert_eq!(js(&mut b, "log.join()"), "micro,m");
        assert_eq!(b.it.pending_jobs(), 0);
    });
}

#[test]
fn an_async_callback_that_hits_a_gate_stops_the_pump_with_unsupported() {
    big(|| {
        let mut b = browser();
        js(&mut b, "var log = []; setTimeout(async function () { await null; log.push('a'); new MessageChannel().port1.postMessage({}); log.push('never'); }, 0); setTimeout(function () { log.push('later'); }, 1); 0");
        assert!(matches!(b.advance_time(5.0, 10), Err(Error::Unsupported(_))));
        assert!(matches!(b.advance_time(5.0, 10), Err(Error::Unsupported(_))));
        assert_eq!(js(&mut b, "log.join()"), "a");
    });
}
