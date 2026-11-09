//! Rejection tracking (HTML "HostPromiseRejectionTracker"): the pending list is filled at reject time, and the host hears
//! about each promise that is still unhandled after the checkpoint, and about each reported promise that got a handler later.

use super::*;
use crate::heap::Heap;
use std::cell::RefCell;

fn big<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(64 << 20).spawn(f).unwrap().join().unwrap()
}

/// Records the reasons it hears, as text.
#[derive(Default)]
struct R {
    unhandled: RefCell<Vec<String>>,
    handled: RefCell<Vec<String>>,
    dropped: RefCell<u32>,
}

fn reason(it: &Interp<'_>, p: Value) -> String {
    match it.promise_status(p) {
        Some(PromiseStatus::Rejected(v)) => it.display(v),
        _ => "not rejected".into(),
    }
}

impl Host for R {
    fn call(&self, _: &mut Interp<'_>, _: u32, _: Value, _: &[Value], _: bool) -> Result<Value, Error> {
        unreachable!()
    }
    fn promise_rejections(&self, it: &mut Interp<'_>, unhandled: &[Value], handled: &[Value], dropped: u32) -> Result<(), Error> {
        self.unhandled.borrow_mut().extend(unhandled.iter().map(|&p| reason(it, p)));
        self.handled.borrow_mut().extend(handled.iter().map(|&p| reason(it, p)));
        *self.dropped.borrow_mut() += dropped;
        Ok(())
    }
}

fn setup() -> (Interp<'static>, Rc<R>) {
    let mut it = Interp::realm(Heap::default(), EvalLimits { max_steps: 10_000_000, ..Default::default() }).unwrap();
    let r = Rc::new(R::default());
    it.set_host(r.clone()).unwrap();
    (it, r)
}

fn take(v: &RefCell<Vec<String>>) -> String {
    std::mem::take(&mut *v.borrow_mut()).join(",")
}

#[test]
fn the_pending_list_holds_the_promises_rejected_with_no_handler() {
    big(|| {
        let (mut it, r) = setup();
        it.run_script("var a = Promise.reject(1); var b = Promise.reject(2); b.catch(function () {}); var c = Promise.resolve(3).then(function () { throw 4; }); new Promise(function () {}); Promise.resolve(8).then(function () {}); async function f() { throw 5; } var e = f(); var g = Promise.reject(6); g.then(null, function () {});").unwrap();
        // At reject time a, b, e and g had no handler; c is not rejected yet; fulfilled and pending ones are never listed.
        let pending: Vec<String> = it.pending_rejections().into_iter().map(|p| reason(&it, p)).collect();
        assert_eq!(pending.join(","), "1,2,5,6");
        it.drain_microtasks().unwrap();
        // After the checkpoint only the still unhandled ones are reported, in reject order.
        assert_eq!(take(&r.unhandled), "1,5,4");
        assert_eq!(take(&r.handled), "");
        assert!(it.pending_rejections().is_empty());
        // A later handler on a reported promise is reported once.
        it.run_script("a.catch(function () {}); a.catch(function () {}); c.then(null, function () {});").unwrap();
        it.drain_microtasks().unwrap();
        assert_eq!(take(&r.unhandled), "");
        assert_eq!(take(&r.handled), "1,4");
        // An await is a handler too.
        it.run_script("(async function () { try { await e; } catch (x) {} })();").unwrap();
        it.drain_microtasks().unwrap();
        assert_eq!(take(&r.handled), "5");
        it.drain_microtasks().unwrap();
        assert_eq!(take(&r.unhandled) + &take(&r.handled), "");
    });
}

#[test]
fn a_rejection_that_a_host_reaction_handles_is_not_reported() {
    big(|| {
        let (mut it, r) = setup();
        it.run_script("var p = Promise.reject(1);").unwrap();
        let (g, k) = (it.global_object(), it.heap.key_from_str("p").unwrap());
        let p = it.get_value(Value::Object(g), k).unwrap();
        it.host_promise_then(p, Value::Undefined, Value::Undefined).unwrap();
        it.drain_microtasks().unwrap();
        // The derived promise of the host reaction is rejected with no handler: it is the one reported.
        assert_eq!(take(&r.unhandled), "1");
        assert_eq!(it.pending_rejections().len(), 0);
    });
}

#[test]
fn the_pending_list_is_bounded() {
    big(|| {
        let (mut it, r) = setup();
        it.run_script("for (var i = 0; i < 5000; i++) Promise.reject(i);").unwrap();
        assert_eq!(it.pending_rejections().len(), MAX_PENDING_REJECTIONS);
        it.drain_microtasks().unwrap();
        assert_eq!(r.unhandled.borrow().len(), MAX_PENDING_REJECTIONS);
        assert_eq!(*r.dropped.borrow() as usize, 5000 - MAX_PENDING_REJECTIONS);
        // The list of reported promises is bounded too: a late handler on the oldest ones is not heard.
        assert!(it.promise.reported.len() <= MAX_REPORTED_REJECTIONS);
    });
}

#[test]
fn pending_rejections_survive_a_collection_and_reported_ones_do_not_keep_promises_alive() {
    big(|| {
        let (mut it, r) = setup();
        it.run_script("(function () { Promise.reject(1); })();").unwrap();
        it.collect(&[]).unwrap();
        it.drain_microtasks().unwrap();
        assert_eq!(take(&r.unhandled), "1");
        assert_eq!(it.promise.reported.len(), 1);
        it.collect(&[]).unwrap();
        assert_eq!(it.promise.reported.len(), 0);
    });
}
