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
            _ => unreachable!(),
        }
    }
    fn roots(&self, out: &mut Vec<Value>) {
        out.extend(self.perm.get());
    }
    fn trace(&self, owner: ObjId, out: &mut Vec<Value>) {
        out.extend(self.owned.borrow().get(&owner).copied());
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
    for (i, (name, ctor)) in [("echo", false), ("callback", false), ("charge", false), ("attach", false), ("perm", false), ("capped", false), ("Thing", true), ("owned", false), ("count", false)].iter().enumerate() {
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
