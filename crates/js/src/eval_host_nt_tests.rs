//! Host constructors that opt in to a different `new.target` (`Interp::host_ctor_with_new_target`), and the ones that do
//! not. The target is visible only for the length of its own call. Each test runs on a big stack.

use super::*;
use crate::heap::Heap;

fn big<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(64 << 20).spawn(f).unwrap().join().unwrap()
}

struct H;

impl Host for H {
    fn call(&self, it: &mut Interp<'_>, id: u32, _this: Value, args: &[Value], is_new: bool) -> Result<Value, Error> {
        let a = |i: usize| args.get(i).copied().unwrap_or(Value::Undefined);
        let default = it.array_prototype();
        match id {
            // Opt-in constructor: call a0 (if any) first, then read the prototype, then make the object.
            0 => {
                assert!(is_new);
                if it.is_callable_value(a(0)) {
                    it.call(a(0), Value::Undefined, &[])?;
                }
                let proto = it.host_proto_from_new_target(default)?;
                Ok(Value::Object(it.heap.new_object(Some(proto)).unwrap()))
            }
            // The prototype a host function sees: the default unless it runs as an opt-in constructor.
            1 => Ok(Value::Object(it.host_proto_from_new_target(default)?)),
            // A plain host constructor, not opted in.
            2 => Ok(Value::Object(it.heap.new_object(Some(it.object_prototype())).unwrap())),
            // charge a0 steps, then behave as 0
            3 => {
                let n = it.to_js_number(a(0))? as u64;
                it.charge(n)?;
                let proto = it.host_proto_from_new_target(default)?;
                Ok(Value::Object(it.heap.new_object(Some(proto)).unwrap()))
            }
            _ => unreachable!(),
        }
    }
}

fn setup_with(limits: EvalLimits) -> Interp<'static> {
    let mut it = Interp::realm(Heap::default(), limits).unwrap();
    it.set_host(Rc::new(H)).unwrap();
    let g = it.global_object();
    let fns = [
        ("NtCtor", it.host_ctor_with_new_target(0, "NtCtor", 0).unwrap()),
        ("probe", it.host_function(1, "probe", 0, false).unwrap()),
        ("Plain", it.host_function(2, "Plain", 0, true).unwrap()),
        ("Heavy", it.host_ctor_with_new_target(3, "Heavy", 1).unwrap()),
    ];
    for (name, f) in fns {
        let k = it.heap.key_from_str(name).unwrap();
        it.define_data(g, k, Value::Object(f), true, false, true).unwrap();
        if name != "probe" {
            // a host constructor's owner defines its `prototype`
            let proto = it.heap.new_object(Some(it.object_prototype())).unwrap();
            let kp = it.heap.key_from_str("prototype").unwrap();
            it.define_data(f, kp, Value::Object(proto), false, false, false).unwrap();
        }
    }
    it
}

fn setup() -> Interp<'static> {
    setup_with(EvalLimits { max_steps: 100_000, ..Default::default() })
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

const GATE: &str = "unsupported:subclassing a host constructor (new.target is not passed to the constructor)";

#[test]
fn only_the_opted_in_host_constructor_takes_a_different_new_target() {
    big(|| {
        let mut it = setup();
        assert_eq!(out(&mut it, "function G() {} var P = {}; G.prototype = P; Object.getPrototypeOf(Reflect.construct(NtCtor, [], G)) === P"), "true");
        assert_eq!(out(&mut it, "class D extends NtCtor {}; var d = new D(); Object.getPrototypeOf(d) === D.prototype && d instanceof D"), "true");
        assert_eq!(out(&mut it, "class E extends D { constructor() { super(); this.x = 1; } }; var e = new E(); [e.x, e instanceof D, Object.getPrototypeOf(e) === E.prototype].join()"), "1,true,true");
        // plain construction, and a construction whose new.target is the function itself: no `prototype` read, the default
        // (a real host constructor passes its own read-only `prototype` as the default)
        assert_eq!(out(&mut it, "Object.getPrototypeOf(new NtCtor()) === Array.prototype"), "true");
        assert_eq!(out(&mut it, "Object.getPrototypeOf(Reflect.construct(NtCtor, [], NtCtor)) === Array.prototype"), "true");
        // the generic gate is unchanged for every other host constructor
        assert_eq!(out(&mut it, "class Q extends Plain {}; new Q()"), GATE);
        assert_eq!(out(&mut it, "function G2() {} Reflect.construct(Plain, [], G2)"), GATE);
        assert_eq!(out(&mut it, "typeof new Plain() + typeof Reflect.construct(Plain, [], Plain)"), "objectobject");
        // a non-object prototype falls back to the default
        assert_eq!(out(&mut it, "G.prototype = 3; Object.getPrototypeOf(Reflect.construct(NtCtor, [], G)) === Array.prototype"), "true");
        // malformed new.target never reaches the host
        for nt in ["{}", "undefined", "null", "1", "() => 1", "async function () {}", "probe"] {
            assert_eq!(out(&mut it, &format!("try {{ Reflect.construct(NtCtor, [], {nt}) }} catch (e) {{ e.name }}")), "TypeError", "{nt}");
        }
        // bound constructors forward the target
        assert_eq!(out(&mut it, "var B = NtCtor.bind(null); Object.getPrototypeOf(Reflect.construct(B, [], D)) === D.prototype"), "true");
        assert_eq!(out(&mut it, "var BD = D.bind(null); Object.getPrototypeOf(new BD()) === D.prototype"), "true");
    });
}

#[test]
fn the_target_lives_only_for_its_own_call() {
    big(|| {
        let mut it = setup();
        assert_eq!(out(&mut it, "probe() === Array.prototype"), "true");
        // inside the constructor, a getter and a callback see no target of the outer call, nested constructors see their own
        out(&mut it, "var seen = []; function A() {} A.prototype = {tag: 'A'}; function B() {} B.prototype = {tag: 'B'};");
        out(&mut it, "var inner; var cb = function () { seen.push(probe() === Array.prototype); inner = Reflect.construct(NtCtor, [], B); seen.push(probe() === Array.prototype); };");
        assert_eq!(out(&mut it, "var o = Reflect.construct(NtCtor, [cb], A); [Object.getPrototypeOf(o).tag, Object.getPrototypeOf(inner).tag, seen.join(), probe() === Array.prototype].join()"), "A,B,true,true,true");
        // a throw, a gate and a limit inside the call leave nothing behind
        out(&mut it, "function T() {} Object.defineProperty(T, 'x', {value: 1});");
        assert_eq!(out(&mut it, "try { Reflect.construct(NtCtor, [function () { throw new RangeError('x'); }], A) } catch (e) { e.name + (probe() === Array.prototype) }"), "RangeErrortrue");
        assert_eq!(out(&mut it, "Reflect.construct(NtCtor, [function () { Reflect.construct(Plain, [], A); }], A)"), GATE);
        assert_eq!(out(&mut it, "probe() === Array.prototype"), "true");
        assert_eq!(out(&mut it, "Reflect.construct(Heavy, [1e9], A)"), "limit:Steps");
        assert_eq!(out(&mut it, "probe() === Array.prototype && Object.getPrototypeOf(new Heavy(1)) === Array.prototype"), "true");
        // the target is not a root once the call ends, and a collection between calls is fine
        it.collect(&[]).unwrap();
        assert_eq!(out(&mut it, "Object.getPrototypeOf(Reflect.construct(NtCtor, [], A)).tag + probe().length"), "A0");
    });
}

#[test]
fn a_prototype_getter_error_reaches_the_caller_and_depth_is_guarded() {
    big(|| {
        let mut it = setup_with(EvalLimits { max_steps: 100_000, max_call_depth: 12, ..Default::default() });
        out(&mut it, "var nt = function () {}.bind(); Object.defineProperty(nt, 'prototype', {get: function () { throw new TypeError('pg'); }});");
        assert_eq!(out(&mut it, "try { Reflect.construct(NtCtor, [], nt) } catch (e) { e.message + (probe() === Array.prototype) }"), "pgtrue");
        // recursion through the callback ends at the call depth limit, and the outer state unwinds cleanly
        out(&mut it, "function R() {} var rec = function () { Reflect.construct(NtCtor, [rec], R); };");
        assert_eq!(out(&mut it, "rec()"), "limit:CallDepth");
        assert_eq!(out(&mut it, "probe() === Array.prototype && Object.getPrototypeOf(Reflect.construct(NtCtor, [], R)) === R.prototype"), "true");
        // a step limit that ends the callback
        out(&mut it, "var spin = function () { while (true) {} };");
        assert_eq!(out(&mut it, "Reflect.construct(NtCtor, [spin], R)"), "limit:Steps");
        assert_eq!(out(&mut it, "probe() === Array.prototype"), "true");
    });
}

#[test]
fn scripts_with_the_same_function_ids_and_promises_do_not_mix() {
    big(|| {
        let mut it = setup();
        out(&mut it, "class A extends NtCtor { a() { return 'a'; } }");
        it.collect(&[]).unwrap();
        out(&mut it, "class B extends NtCtor { b() { return 'b'; } }");
        assert_eq!(out(&mut it, "var x = new A(), y = new B(); [x.a(), y.b(), x instanceof A, y instanceof B, x instanceof B].join()"), "a,b,true,true,false");
        // a sticky Promise limit inside the callback ends the construction with the limit
        it.set_promise_limits(PromiseLimits { max_promises: 3, ..Default::default() });
        assert_eq!(out(&mut it, "Reflect.construct(NtCtor, [function () { for (var i = 0; i < 9; i++) Promise.resolve(i); }], A)"), "limit:Promises");
        it.set_promise_limits(PromiseLimits::default());
        assert_eq!(out(&mut it, "(probe() === Array.prototype) + new B().b()"), "trueb");
    });
}
