//! `URLSearchParams` subclassing: `new.target` reaches the host constructor, and only this one. Order follows Web IDL:
//! the init is converted first (overload resolution), then `Get(newTarget, "prototype")`, then the object is made.
//! Node builds `URLSearchParams` as a JS class and reads the prototype first, so getter order is checked against the
//! specification text, not against Node. Each test runs on a big stack.

use crate::browser::*;
use bare_html::tree;
use bare_js::eval::{Error, EvalLimits, LimitKind, PromiseLimits};
use bare_js::heap::HeapLimits;
use bare_js::value::Value;

fn big<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(64 << 20).spawn(f).unwrap().join().unwrap()
}

fn mk(eval: EvalLimits) -> Browser {
    let page = "<!doctype html><html><head></head><body></body></html>";
    let mut b = Browser::new(tree::parse(page), BrowserLimits::default(), eval, HeapLimits::default()).expect("browser");
    b.set_document_url("https://staging.example.test/a?x=1").expect("url");
    b
}

fn at() -> Browser {
    mk(EvalLimits { max_steps: 5_000_000, ..Default::default() })
}

fn js(b: &mut Browser, src: &str) -> String {
    match b.run_script(src) {
        Ok(v) => b.it.display(v),
        Err(Error::Throw(v)) if matches!(v, Value::Object(_)) => format!("throw:{}:{}", b.it.get_str(v, "name"), b.it.get_str(v, "message")),
        Err(Error::Limit(l)) => format!("limit:{l:?}"),
        Err(Error::Unsupported(u)) => format!("unsupported:{}", u.what),
        Err(e) => format!("other:{e:?}"),
    }
}

/// A bound function (it has no own `prototype`) with a `prototype` accessor, to see when the getter runs.
const NT: &str = "function mkP(t) { var p = Object.create(URLSearchParams.prototype); p.tag = t; return p; } function mkNt(get) { var nt = function () {}.bind(); Object.defineProperty(nt, 'prototype', { get: get, configurable: true }); return nt; }";

#[test]
fn a_class_extending_url_search_params_gets_native_state() {
    big(|| {
        let mut b = at();
        js(&mut b, "class L extends URLSearchParams {}; var p = new L('a=1');");
        assert_eq!(js(&mut b, "p.get('a') + p.size"), "11");
        assert_eq!(js(&mut b, "p.append('b', '2'); p.set('a', '3'); p.toString()"), "a=3&b=2");
        assert_eq!(js(&mut b, "[p instanceof L, p instanceof URLSearchParams, Object.getPrototypeOf(p) === L.prototype, Object.getPrototypeOf(L.prototype) === URLSearchParams.prototype].join()"), "true,true,true,true");
        assert_eq!(js(&mut b, "[...p].join('|')"), "a,3|b,2");
        assert_eq!(js(&mut b, "new L([['x', '1'], ['y', '2']]).toString() + new L({k: 'v'}) + new L().size"), "x=1&y=2k=v0");
        // methods see the instance, and the intrinsic ones stay unaffected
        assert_eq!(js(&mut b, "Object.getPrototypeOf(new URLSearchParams('a=1')) === URLSearchParams.prototype"), "true");
        assert_eq!(js(&mut b, "var u = new URL('http://h.test/?x=1'); u.searchParams.append('y', '2'); [u.href, Object.getPrototypeOf(u.searchParams) === URLSearchParams.prototype, u.searchParams.constructor === URLSearchParams, u.searchParams instanceof L].join()"), "http://h.test/?x=1&y=2,true,true,false");
        assert_eq!(js(&mut b, "new URLSearchParams(p).toString()"), "a=3&b=2");
    });
}

#[test]
fn explicit_and_further_derived_and_bound_constructors() {
    big(|| {
        let mut b = at();
        js(&mut b, "class M extends URLSearchParams { constructor(x) { super(x); this.tag = 'm'; } extra() { return this.get('a') + this.tag; } }; class N extends M {}; class O extends N { constructor() { super('a=9'); } }");
        assert_eq!(js(&mut b, "new M('a=1').extra()"), "1m");
        assert_eq!(js(&mut b, "var n = new N('a=2'); [n.extra(), n instanceof M, n instanceof URLSearchParams, Object.getPrototypeOf(n) === N.prototype].join()"), "2m,true,true,true");
        assert_eq!(js(&mut b, "new O().extra()"), "9m");
        // a class instance made by a bound class, and Reflect.construct through a bound host constructor
        assert_eq!(js(&mut b, "var BL = N.bind(null, 'a=5'); var q = new BL(); [q.extra(), Object.getPrototypeOf(q) === N.prototype].join()"), "5m,true");
        assert_eq!(js(&mut b, "var BH = URLSearchParams.bind(null, 'a=6'); var r = Reflect.construct(BH, [], N); [Object.getPrototypeOf(r) === N.prototype, r.get('a')].join()"), "true,6");
        assert_eq!(js(&mut b, "var s = new BH(); [Object.getPrototypeOf(s) === URLSearchParams.prototype, s.get('a')].join()"), "true,6");
        // a function constructor can call super-like behavior only through Reflect.construct
        assert_eq!(js(&mut b, "function F(x) { return Reflect.construct(URLSearchParams, [x], new.target); } F.prototype = Object.create(URLSearchParams.prototype); var f = new F('k=v'); [f.get('k'), Object.getPrototypeOf(f) === F.prototype, f instanceof F].join()"), "v,true,true");
        // not called as a function
        assert_eq!(js(&mut b, "try { L2 = class extends URLSearchParams {}; L2() } catch (e) { e.name }"), "TypeError");
        assert_eq!(js(&mut b, "try { URLSearchParams() } catch (e) { e.name }"), "TypeError");
    });
}

#[test]
fn reflect_construct_with_a_different_new_target() {
    big(|| {
        let mut b = at();
        js(&mut b, "function G() {} var P = {tag: 'P'}; G.prototype = P;");
        assert_eq!(js(&mut b, "var r = Reflect.construct(URLSearchParams, ['a=1'], G); [Object.getPrototypeOf(r) === P, URLSearchParams.prototype.get.call(r, 'a')].join()"), "true,1");
        // not an object: the intrinsic prototype
        for v in ["1", "'s'", "undefined", "null", "true"] {
            assert_eq!(js(&mut b, &format!("G.prototype = {v}; var r = Reflect.construct(URLSearchParams, ['a=1'], G); Object.getPrototypeOf(r) === URLSearchParams.prototype && r.get('a')")), "1", "{v}");
        }
        // a prototype with no brand parent still holds native state
        assert_eq!(js(&mut b, "G.prototype = Object.prototype; var r = Reflect.construct(URLSearchParams, ['a=1'], G); typeof r.get + URLSearchParams.prototype.toString.call(r)"), "undefineda=1");
    });
}

#[test]
fn malformed_new_target_is_refused_before_the_host_runs() {
    big(|| {
        let mut b = at();
        for nt in ["{}", "undefined", "null", "1", "'x'", "() => 1", "async function () {}", "Math.max"] {
            let r = js(&mut b, &format!("try {{ Reflect.construct(URLSearchParams, [], {nt}); 'built' }} catch (e) {{ e.name }}"));
            assert_eq!(r, "TypeError", "{nt}");
        }
        assert_eq!(b.navigation_stats().params, 0);
    });
}

#[test]
fn init_is_converted_before_the_prototype_is_read() {
    big(|| {
        let mut b = at();
        js(&mut b, NT);
        js(&mut b, "var log = []; var nt = mkNt(function () { log.push('proto'); return mkP(1); });");
        // record getters, an iterable, and a USVString conversion all run first
        assert_eq!(js(&mut b, "var rec = {get a() { log.push('init'); return '1'; }}; var r = Reflect.construct(URLSearchParams, [rec], nt); log.join() + '|' + r.get('a') + Object.getPrototypeOf(r).tag"), "init,proto|11");
        assert_eq!(js(&mut b, "log = []; var it = {[Symbol.iterator]() { log.push('iter'); var i = 0; return {next() { log.push('next'); return i++ ? {done: true} : {done: false, value: ['k', 'v']}; }}; }}; var r = Reflect.construct(URLSearchParams, [it], nt); log.join() + '|' + r.get('k')"), "iter,next,next,proto|v");
        assert_eq!(js(&mut b, "log = []; Reflect.construct(URLSearchParams, [{a: {toString() { log.push('str'); return '1'; }}}], nt); log.join()"), "str,proto");
        // a throwing init getter: the prototype is never read
        assert_eq!(js(&mut b, "log = []; try { Reflect.construct(URLSearchParams, [{get a() { throw new RangeError('init'); }}], nt) } catch (e) { e.message + '|' + log.length }"), "init|0");
        // a bad pair is a TypeError, also before the prototype
        assert_eq!(js(&mut b, "log = []; try { Reflect.construct(URLSearchParams, [[['only']]], nt) } catch (e) { e.name + '|' + log.length }"), "TypeError|0");
        b.collect(&[]).unwrap();
        assert_eq!(b.navigation_stats().params, 1, "only `r` (the last good one) lives: no failed construction leaves a params object");
    });
}

#[test]
fn throwing_and_reentrant_prototype_getters() {
    big(|| {
        let mut b = at();
        js(&mut b, NT);
        assert_eq!(js(&mut b, "var t = mkNt(function () { throw new RangeError('proto'); }); try { Reflect.construct(URLSearchParams, ['a=1'], t) } catch (e) { e.message }"), "proto");
        assert_eq!(b.navigation_stats().params, 0);
        // a nested construction sees only its own new.target; the outer one is intact afterwards
        js(&mut b, "var inner; var outerP = mkP('outer'), innerP = mkP('inner'), plain;");
        js(&mut b, "var ntIn = mkNt(function () { return innerP; }); var ntOut = mkNt(function () { inner = Reflect.construct(URLSearchParams, ['i=1'], ntIn); plain = new URLSearchParams('p=1'); return outerP; });");
        assert_eq!(js(&mut b, "var o = Reflect.construct(URLSearchParams, ['o=1'], ntOut); [Object.getPrototypeOf(o).tag, Object.getPrototypeOf(inner).tag, Object.getPrototypeOf(plain) === URLSearchParams.prototype, o.get('o'), inner.get('i'), plain.get('p')].join()"), "outer,inner,true,1,1,1");
        // a plain host call inside a getter, then a generic construction: nothing leaks to the next call
        assert_eq!(js(&mut b, "var ntX = mkNt(function () { new URLSearchParams(); return outerP; }); Reflect.construct(URLSearchParams, [], ntX); Object.getPrototypeOf(new URLSearchParams()) === URLSearchParams.prototype"), "true");
        // a getter that constructs a subclass instance
        assert_eq!(js(&mut b, "class S extends URLSearchParams {}; var ntS = mkNt(function () { return new S('z=1').constructor === S ? S.prototype : null; }); var r = Reflect.construct(URLSearchParams, [], ntS); Object.getPrototypeOf(r) === S.prototype"), "true");
        b.collect(&[]).unwrap();
        assert_eq!(b.navigation_stats().params, 4, "o, inner, plain and r");
    });
}

#[test]
fn foreign_and_unbranded_objects_are_rejected() {
    big(|| {
        let mut b = at();
        js(&mut b, "class L extends URLSearchParams {}");
        for src in [
            "URLSearchParams.prototype.get.call(Object.create(L.prototype), 'a')",
            "URLSearchParams.prototype.append.call(Reflect.construct(Object, [], L), 'a', 'b')",
            "Object.getOwnPropertyDescriptor(URLSearchParams.prototype, 'size').get.call(L.prototype)",
            "URLSearchParams.prototype.toString.call(Object.create(URLSearchParams.prototype))",
            "URLSearchParams.prototype.get.call(new URL('http://a.test/'), 'a')",
        ] {
            assert_eq!(js(&mut b, &format!("try {{ {src} }} catch (e) {{ e.name + ':' + e.message }}")), "TypeError:Illegal invocation", "{src}");
        }
        assert_eq!(js(&mut b, "URLSearchParams.prototype.get.call(new L('a=1'), 'a')"), "1");
    });
}

#[test]
fn other_host_constructors_stay_gated() {
    big(|| {
        let mut b = at();
        let gate = "unsupported:subclassing a host constructor (new.target is not passed to the constructor)";
        for (name, args) in [("URL", "'http://a.test/'"), ("Headers", ""), ("Request", "'http://a.test/'"), ("Response", "")] {
            assert_eq!(js(&mut b, &format!("class X extends {name} {{}}; new X({args})")), gate, "{name}");
            assert_eq!(js(&mut b, &format!("function G() {{}} Reflect.construct({name}, [{args}], G)")), gate, "Reflect {name}");
        }
        assert_eq!(js(&mut b, "typeof new Event('x') + typeof new URL('http://a.test/')"), "objectobject");
    });
}

#[test]
fn limits_and_failures_leave_no_new_target_behind() {
    big(|| {
        let mut b = mk(EvalLimits { max_steps: 5_000_000, max_call_depth: 20, ..Default::default() });
        js(&mut b, NT);
        js(&mut b, "class L extends URLSearchParams {}");
        // the depth guard: a getter that constructs again reaches the limit, which no `catch` can swallow
        js(&mut b, "var ntD; ntD = mkNt(function () { return Reflect.construct(URLSearchParams, [], ntD); });");
        assert_eq!(js(&mut b, "try { Reflect.construct(URLSearchParams, [], ntD) } catch (e) { 'caught' }"), format!("limit:{:?}", LimitKind::CallDepth));
        assert_eq!(js(&mut b, "var p = new L('a=1'); [Object.getPrototypeOf(p) === L.prototype, Object.getPrototypeOf(new URLSearchParams()) === URLSearchParams.prototype].join()"), "true,true");
        // an Unsupported gate inside the getter, then a clean construction
        assert_eq!(js(&mut b, "class E extends URL {}; var ntU = mkNt(function () { new E('http://a.test/'); return {}; }); Reflect.construct(URLSearchParams, [], ntU)"), "unsupported:subclassing a host constructor (new.target is not passed to the constructor)");
        assert_eq!(js(&mut b, "Object.getPrototypeOf(new L()) === L.prototype && Object.getPrototypeOf(new URLSearchParams()) === URLSearchParams.prototype"), "true");
        // a step limit inside the getter
        let mut s = mk(EvalLimits { max_steps: 3_000, ..Default::default() });
        js(&mut s, NT);
        assert_eq!(js(&mut s, "var ntS = mkNt(function () { while (true) {} }); Reflect.construct(URLSearchParams, [], ntS)"), format!("limit:{:?}", LimitKind::Steps));
        // a sticky Promise limit inside the getter
        let mut q = at();
        q.it.set_promise_limits(PromiseLimits { max_promises: 4, ..Default::default() });
        js(&mut q, NT);
        let r = js(&mut q, "var ntP = mkNt(function () { for (var i = 0; i < 10; i++) Promise.resolve(i); return {}; }); try { Reflect.construct(URLSearchParams, [], ntP) } catch (e) { 'caught' }");
        assert_eq!(r, format!("limit:{:?}", LimitKind::Promises));
        q.it.set_promise_limits(PromiseLimits::default());
        q.collect(&[]).unwrap();
        assert_eq!(js(&mut q, "class L extends URLSearchParams {}; new L('a=1').get('a')"), "1");
        assert_eq!(q.navigation_stats().params, 1);
    });
}

#[test]
fn colliding_ids_across_scripts_and_collection_between_calls() {
    big(|| {
        let mut b = at();
        js(&mut b, "class A1 extends URLSearchParams { who() { return 'a' + this.get('k'); } }; var a = new A1('k=1');");
        b.collect(&[]).unwrap();
        js(&mut b, "class B1 extends URLSearchParams { who() { return 'b' + this.get('k'); } }; class C1 extends B1 {}; var c = new C1('k=2');");
        assert_eq!(js(&mut b, "[a.who(), c.who(), new A1('k=3').who(), new C1('k=4').who(), Object.getPrototypeOf(a) === A1.prototype, Object.getPrototypeOf(c) === C1.prototype].join()"), "a1,b2,a3,b4,true,true");
        // the class of another script as new.target, Reflect.construct from a third
        js(&mut b, "var viaReflect = function () { return Reflect.construct(URLSearchParams, ['k=5'], C1); };");
        b.collect(&[]).unwrap();
        assert_eq!(js(&mut b, "var r = viaReflect(); r.who() + (r instanceof B1)"), "b5true");
        // dropped instances are freed, kept ones survive
        js(&mut b, "for (var i = 0; i < 50; i++) new A1('k=' + i); 1");
        b.collect(&[]).unwrap();
        assert_eq!(b.navigation_stats().params, 3, "a, c and r");
        js(&mut b, "a = c = r = null;");
        b.collect(&[]).unwrap();
        assert_eq!(b.navigation_stats().params, 0);
        assert_eq!(b.navigation_stats().param_bytes, 0);
    });
}

#[test]
fn a_limit_in_a_subclass_job_poisoned_checkpoint_never_replays() {
    big(|| {
        let mut b = mk(EvalLimits { max_steps: 3000, ..Default::default() });
        js(&mut b, NT);
        js(&mut b, "var hits=0, later=0; var target=mkNt(function () { hits++; while(true) {} }); Promise.resolve().then(function () { Reflect.construct(URLSearchParams, [], target); }); Promise.resolve().then(function () { later++; });");
        assert!(matches!(b.drain(), Err(Error::Limit(LimitKind::Steps))));
        assert!(matches!(b.drain(), Err(Error::Limit(LimitKind::Steps))));
        assert_eq!(js(&mut b, "[hits,later].join()"), "1,0");
    });
}

#[test]
fn a_depth_limit_in_a_promise_job_is_sticky_and_never_replays() {
    big(|| {
        let mut b = mk(EvalLimits { max_steps: 5_000_000, max_call_depth: 20, ..Default::default() });
        js(&mut b, "var hits = 0, later = 0; function deep(n) { hits++; return n === 0 ? 0 : 1 + deep(n - 1) } Promise.resolve().then(function () { deep(40) }); Promise.resolve().then(function () { later++ });");
        assert!(matches!(b.drain(), Err(Error::Limit(LimitKind::CallDepth))));
        let hits = js(&mut b, "hits");
        assert!(matches!(b.drain(), Err(Error::Limit(LimitKind::CallDepth))));
        assert_eq!(js(&mut b, "[hits, later].join()"), format!("{hits},0"));
    });
}

#[test]
fn host_constructor_recursion_stops_at_the_ceilings_on_a_real_stack() {
    // 8 MiB in a release build and 64 MiB in a debug build, the stacks of a real run
    let mib: usize = if cfg!(debug_assertions) { 64 } else { 8 };
    std::thread::Builder::new().stack_size(mib << 20).spawn(|| {
        let mut b = mk(EvalLimits { max_steps: 5_000_000, max_traversal_depth: 768, max_call_depth: 128, ..Default::default() });
        js(&mut b, NT);
        js(&mut b, "var ntD; ntD = mkNt(function () { return Reflect.construct(URLSearchParams, [], ntD); });");
        assert_eq!(js(&mut b, "try { Reflect.construct(URLSearchParams, [], ntD) } catch (e) { 'caught' }"), format!("limit:{:?}", LimitKind::CallDepth));
        assert_eq!(js(&mut b, "class L extends URLSearchParams {}; new L('a=1').get('a')"), "1");
    }).unwrap().join().unwrap();
}
