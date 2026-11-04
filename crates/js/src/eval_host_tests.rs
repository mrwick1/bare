//! Host hook tests: calls, constructors, reentrancy, guards, and the collector contract. The test host is a
//! few numbered methods, not a DOM. Each test runs on a big stack.

use super::*;
use crate::heap::Heap;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;

fn big<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(64 << 20).spawn(f).unwrap().join().unwrap()
}

#[derive(Default)]
struct T {
    /// Values kept by `owner`, traced and pruned.
    owned: RefCell<HashMap<ObjId, Value>>,
    perm: Cell<Option<Value>>,
    seen_this: RefCell<Vec<String>>,
    pruned: Cell<usize>,
    reported: RefCell<Vec<String>>,
}

impl Host for T {
    fn call(&self, it: &mut Interp<'_>, id: u32, this: Value, args: &[Value], is_new: bool) -> Result<Value, Error> {
        let a = |i: usize| args.get(i).copied().unwrap_or(Value::Undefined);
        match id {
            // echo: number of arguments, remembers `this`
            0 => {
                self.seen_this.borrow_mut().push(format!("{}{}", it.display(this), if is_new { "/new" } else { "" }));
                Ok(Value::Number(args.len() as f64))
            }
            // call a1 with a2 as the argument (reentrant; no borrow held)
            1 => it.call(a(0), Value::Undefined, &[a(1)]),
            // charge n steps
            2 => {
                let n = it.to_js_number(a(0))? as u64;
                it.charge(n).map(|_| Value::Undefined)
            }
            // attach(owner, value)
            3 => {
                let Value::Object(o) = a(0) else { return Err(it.type_error("owner")) };
                self.owned.borrow_mut().insert(o, a(1));
                Ok(Value::Undefined)
            }
            // make arg a permanent root
            4 => {
                self.perm.set(Some(a(0)));
                Ok(Value::Undefined)
            }
            5 => Err(Error::Limit(LimitKind::Host("test-cap"))),
            // constructor: a new object
            6 => {
                assert!(is_new);
                Ok(Value::Object(it.heap.new_object(Some(it.object_prototype())).unwrap()))
            }
            // owned value of arg0, or "none"
            7 => {
                let Value::Object(o) = a(0) else { return Ok(Value::Undefined) };
                Ok(self.owned.borrow().get(&o).copied().unwrap_or(Value::Null))
            }
            8 => Ok(Value::Number(self.owned.borrow().len() as f64)),
            // queueMicrotask(cb)
            9 => it.queue_microtask(a(0)).map(|_| Value::Undefined),
            _ => unreachable!(),
        }
    }
    fn roots(&self, out: &mut Vec<Value>) {
        out.extend(self.perm.get());
    }
    fn trace(&self, owner: ObjId, out: &mut Vec<Value>) {
        out.extend(self.owned.borrow().get(&owner).copied());
    }
    fn report_exception(&self, it: &mut Interp<'_>, thrown: Value) {
        self.reported.borrow_mut().push(it.display(thrown));
    }
    fn prune(&self, live: &dyn Fn(ObjId) -> bool) {
        let mut m = self.owned.borrow_mut();
        let before = m.len();
        m.retain(|&o, _| live(o));
        self.pruned.set(self.pruned.get() + before - m.len());
    }
}

fn setup() -> (Interp<'static>, Rc<T>) {
    let mut it = Interp::realm(Heap::default(), EvalLimits { max_steps: 100_000, ..Default::default() }).unwrap();
    let t = Rc::new(T::default());
    it.set_host(t.clone()).unwrap();
    let g = it.global_object();
    for (i, (name, ctor)) in [("echo", false), ("callback", false), ("charge", false), ("attach", false), ("perm", false), ("capped", false), ("Thing", true), ("owned", false), ("count", false), ("qm", false)].iter().enumerate() {
        let f = it.host_function(i as u32, name, 0, *ctor).unwrap();
        let k = it.heap.key_from_str(name).unwrap();
        it.define_data(g, k, Value::Object(f), true, false, true).unwrap();
    }
    (it, t)
}

fn show(it: &mut Interp, r: Result<Value, Error>) -> String {
    match r {
        Ok(v) => it.display(v),
        Err(Error::Throw(v)) if matches!(v, Value::Object(_)) => format!("throw:{}:{}", it.get_str(v, "name"), it.get_str(v, "message")),
        Err(Error::Limit(l)) => format!("limit:{l:?}"),
        Err(Error::Unsupported(u)) => format!("unsupported:{}", u.what),
        Err(e) => format!("other:{e:?}"),
    }
}

fn out(it: &mut Interp, src: &str) -> String {
    let r = it.run_script(src);
    show(it, r)
}

#[test]
fn host_function_gets_this_and_args_and_a_constructor_works() {
    big(|| {
        let (mut it, t) = setup();
        assert_eq!(out(&mut it, "echo(1, 2, 3)"), "3");
        assert_eq!(out(&mut it, "var o = { m: echo }; o.m()"), "0");
        assert_eq!(t.seen_this.borrow().last().unwrap(), "object");
        assert_eq!(out(&mut it, "typeof new Thing()"), "object");
        assert_eq!(out(&mut it, "new Thing() instanceof Object"), "true");
        // a host function that is not a constructor
        assert_eq!(out(&mut it, "try { new echo() } catch (e) { e.name }"), "TypeError");
        // a host function as an accessor: the getter runs with the receiver as `this`
        out(&mut it, "var r = {}; Object.defineProperty(r, 'x', { get: echo, configurable: true });");
        assert_eq!(out(&mut it, "r.x"), "0");
        assert_eq!(t.seen_this.borrow().last().unwrap(), "object");
    });
}

#[test]
fn reentrant_calls_and_guards() {
    big(|| {
        let (mut it, _t) = setup();
        // host -> JS -> host works
        assert_eq!(out(&mut it, "callback(function (x) { return callback(function (y) { return y + 1 }, x) }, 41)"), "42");
        // unbounded host<->JS recursion ends with the 100-call guard, not a native stack overflow
        assert_eq!(out(&mut it, "function f(x) { return callback(f, x) } f(1)"), "limit:CallDepth");
        // the interpreter works after the limit
        assert_eq!(out(&mut it, "1 + 1"), "2");
        // host work is charged to the step budget
        assert_eq!(out(&mut it, "charge(1000000)"), "limit:Steps");
        // a host cap is its own distinct, uncatchable limit
        assert_eq!(out(&mut it, "try { capped() } catch (e) { 'caught' }"), "limit:Host(\"test-cap\")");
    });
}

#[test]
fn host_scope_shares_one_budget_and_blocks_collect() {
    big(|| {
        let (mut it, _t) = setup();
        let r = it.host_scope(|it| {
            assert!(matches!(it.collect(&[]), Err(Error::Unsupported(_))));
            it.charge(50_000)?;
            it.charge(60_000)
        });
        assert!(matches!(r, Err(Error::Limit(LimitKind::Steps))));
        assert!(it.collect(&[]).is_ok());
    });
}

#[test]
fn collector_traces_owned_values_roots_and_prunes_dead_owners() {
    big(|| {
        let (mut it, t) = setup();
        out(&mut it, "var keep = {}; attach(keep, { tag: 'kept' }); var drop = {}; attach(drop, { tag: 'dropped' }); perm({ tag: 'perm' }); 0");
        assert_eq!(out(&mut it, "count()"), "2");
        out(&mut it, "drop = null;");
        it.collect(&[]).unwrap();
        // the owner of the dropped entry is gone and its metadata was pruned
        assert_eq!(out(&mut it, "count()"), "1");
        assert_eq!(t.pruned.get(), 1);
        // the value held only through the live owner survived, and so did the permanent root
        assert_eq!(out(&mut it, "owned(keep).tag"), "kept");
        let Some(Value::Object(p)) = t.perm.get() else { panic!() };
        assert!(it.heap.get_prototype_of(p).is_ok());
        // a reused slot of the dead owner cannot alias: new objects have no entry
        out(&mut it, "var fresh = []; for (var i = 0; i < 50; i++) fresh.push({}); 0");
        assert_eq!(out(&mut it, "fresh.map ? 'x' : String(owned(fresh[0]))"), "null");
        // a second collection changes nothing more
        it.collect(&[]).unwrap();
        assert_eq!(t.pruned.get(), 1);
        // a value that only the host kept alive through a cycle is collected with its owner
        out(&mut it, "keep = null;");
        it.collect(&[]).unwrap();
        assert_eq!(t.pruned.get(), 2);
        assert_eq!(out(&mut it, "count()"), "0");
    });
}

#[test]
fn second_host_is_refused() {
    big(|| {
        let (mut it, _t) = setup();
        assert!(matches!(it.set_host(Rc::new(T::default())), Err(Error::Unsupported(_))));
    });
}

#[test]
fn queued_microtask_runs_fifo_with_promise_jobs_and_reports_a_throw() {
    big(|| {
        let (mut it, t) = setup();
        out(&mut it, "var log = []; qm(function () { log.push('a') }); Promise.resolve().then(function () { log.push('p') }); qm(function () { log.push('b') }); 0");
        assert_eq!(it.pending_jobs(), 3);
        assert_eq!(it.drain_microtasks().unwrap(), 3);
        assert_eq!(out(&mut it, "log.join()"), "a,p,b");
        // a throw is reported and the queue goes on
        out(&mut it, "qm(function () { throw 7 }); qm(function () { log.push('c') }); 0");
        assert_eq!(it.drain_microtasks().unwrap(), 2);
        assert_eq!(out(&mut it, "log.join()"), "a,p,b,c");
        assert_eq!(*t.reported.borrow(), vec!["7".to_string()]);
        // not callable: a TypeError at the call, nothing queued
        assert_eq!(out(&mut it, "try { qm(1) } catch (e) { e.name }"), "TypeError");
        assert_eq!(it.pending_jobs(), 0);
    });
}

#[test]
fn queued_microtask_keeps_its_callback_alive_and_a_limit_poisons_later_drains() {
    big(|| {
        let (mut it, _t) = setup();
        out(&mut it, "var n = 0; (function () { var big = { v: 41 }; qm(function () { n = big.v + 1 }) })(); 0");
        it.collect(&[]).unwrap();
        it.drain_microtasks().unwrap();
        assert_eq!(out(&mut it, "n"), "42");
        // a limit inside the callback ends the drain; the next drain repeats it and runs nothing
        out(&mut it, "qm(function () { charge(1000000) }); qm(function () { n = 0 }); 0");
        assert!(matches!(it.drain_microtasks(), Err(Error::Limit(LimitKind::Steps))));
        assert!(matches!(it.drain_microtasks(), Err(Error::Limit(LimitKind::Steps))));
        assert_eq!(out(&mut it, "n"), "42");
        // the queue limit
        let (mut it, _t) = setup();
        it.set_promise_limits(PromiseLimits { max_queue: 2, ..Default::default() });
        assert_eq!(out(&mut it, "qm(function () {}); qm(function () {}); qm(function () {})"), "limit:PromiseQueue");
    });
}

#[test]
fn host_promises_settle_once_through_the_promise_machinery() {
    big(|| {
        let (mut it, _t) = setup();
        out(&mut it, "var log = [];");
        let (ok, bad, thenable) = it
            .host_scope(|it| {
                let (a, b, c) = (it.host_new_promise()?, it.host_new_promise()?, it.host_new_promise()?);
                let g = it.global_object();
                for (n, p) in [("pa", a), ("pb", b), ("pc", c)] {
                    let k = it.heap.key_from_str(n).unwrap();
                    it.define_data(g, k, p, true, false, true)?;
                }
                Ok((a, b, c))
            })
            .unwrap();
        out(&mut it, "pa.then(function (v) { log.push('a:' + v); }); pb.then(null, function (e) { log.push('b:' + e); }); pc.then(function (v) { log.push('c:' + v); }); 0");
        assert!(matches!(it.promise_status(ok), Some(PromiseStatus::Pending)));
        it.host_scope(|it| {
            let (one, two) = (it.heap.intern_str("one").unwrap(), it.heap.intern_str("two").unwrap());
            it.host_resolve_promise(ok, Value::String(one))?;
            it.host_reject_promise(bad, Value::String(two))
        })
        .unwrap();
        // Reactions are microtasks: nothing ran in the settle call.
        assert_eq!(out(&mut it, "log.join()"), "");
        assert_eq!(it.pending_jobs(), 2);
        assert!(it.drain_microtasks().is_ok());
        assert_eq!(out(&mut it, "log.join()"), "a:one,b:two");
        // Resolving with a thenable assimilates it in a job, like the resolve function of `new Promise`.
        let th = it.run_script("var th = { then: function (res) { res('from-thenable'); } }; th").unwrap();
        it.host_scope(|it| it.host_resolve_promise(thenable, th)).unwrap();
        assert!(matches!(it.promise_status(thenable), Some(PromiseStatus::Pending)));
        it.drain_microtasks().unwrap();
        assert_eq!(out(&mut it, "log.join()"), "a:one,b:two,c:from-thenable");
        // A value that is not a promise is refused, not ignored.
        let r = it.host_scope(|it| it.host_resolve_promise(Value::Number(1.0), Value::Undefined));
        assert!(matches!(r, Err(Error::Unsupported(_))));
        // A pending promise that nothing roots is collected with its reactions (the host must root what it queues).
        it.host_scope(|it| it.host_new_promise().map(|_| ())).unwrap();
        it.collect(&[]).unwrap();
    });
}
