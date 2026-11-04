//! Class and async interaction tests. `tools/fixtures/js/interact-cases.txt` is checked against Node
//! (`tools/js/interact_check.sh`). The tests here cover what Node cannot show: script ownership of class code,
//! preflight gates, metadata collection and the checkpoint contract.

use super::*;
use crate::heap::Heap;
use crate::parse::{parse_script, Limits};

fn big<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(64 << 20).spawn(f).unwrap().join().unwrap()
}

fn esc(s: &str) -> String {
    s.replace('\\', "\\\\").replace('\n', "\\n").replace('\r', "\\r").replace('\t', "\\t")
}

fn fail(it: &mut Interp, e: Error) -> String {
    match e {
        Error::Throw(v) if matches!(v, Value::Object(_)) => format!("throw:{}:{}", esc(&it.get_str(v, "name")), esc(&it.get_str(v, "message"))),
        Error::Throw(v) => format!("throw-value:{}", esc(&it.display(v))),
        Error::Unsupported(u) => format!("unsupported:{}", u.what),
        Error::Limit(l) => format!("limit:{l:?}"),
        e => format!("internal-error:{e:?}"),
    }
}

fn run_case(src: &str) -> String {
    let prog = parse_script(src, Limits::default()).unwrap_or_else(|e| panic!("{src:?}: {e}"));
    let mut it = Interp::new(&prog, Heap::default(), EvalLimits { max_steps: 5_000_000, ..Default::default() }).expect("setup");
    let reader = match it.run() {
        Ok(v) => v,
        Err(e) => return fail(&mut it, e),
    };
    let read = |it: &mut Interp| match it.call(reader, Value::Undefined, &[]) {
        Ok(v) => Ok(esc(&it.display(v))),
        Err(e) => Err(fail(it, e)),
    };
    let pre = match read(&mut it) {
        Ok(s) => s,
        Err(s) => return s,
    };
    if let Err(e) = it.drain_microtasks() {
        return fail(&mut it, e);
    }
    match read(&mut it) {
        Ok(post) => format!("{pre} ## {post}"),
        Err(s) => s,
    }
}

#[test]
fn corpus_matches_node() {
    let (cases, expected) = (include_str!("../../../tools/fixtures/js/interact-cases.txt"), include_str!("../../../tools/fixtures/js/interact-cases.expected"));
    big(move || {
        let mut list: Vec<(&str, Vec<&str>)> = Vec::new();
        for line in cases.lines() {
            if let Some(n) = line.strip_prefix("=== ") {
                list.push((n.trim(), Vec::new()));
            } else if let Some(c) = list.last_mut() {
                c.1.push(line);
            }
        }
        let want: Vec<&str> = expected.lines().collect();
        assert_eq!(list.len(), want.len());
        assert!(list.len() >= 20);
        let mut bad = Vec::new();
        for ((name, lines), w) in list.iter().zip(want) {
            let got = format!("{name}\t{}", run_case(lines.join("\n").trim_end_matches('\n')));
            if got != w {
                bad.push(format!("node: {w}\nbare: {got}"));
            }
        }
        assert!(bad.is_empty(), "{} differences:\n{}", bad.len(), bad.join("\n"));
    });
}

// ------------------------------------------------------------------ helpers

fn realm(limits: EvalLimits) -> Interp<'static> {
    Interp::realm(Heap::default(), limits).expect("realm")
}

fn out(it: &mut Interp, src: &str) -> String {
    match it.run_script(src) {
        Ok(v) => it.display(v),
        Err(e) => fail(it, e),
    }
}

fn lim() -> EvalLimits {
    EvalLimits { max_steps: 2_000_000, ..Default::default() }
}

fn global(it: &mut Interp, name: &str) -> Value {
    let k = it.heap.key_from_str(name).unwrap();
    match it.heap.get(it.global_object(), k).unwrap() {
        Get::Value(v) => v,
        _ => Value::Undefined,
    }
}

// ------------------------------------------------------------------ script ownership

/// Two scripts with the same node shapes: the first class, field initializer, getter and method of each have the
/// same arena ids. Every use goes through the closure or constructor of the right script.
const SCRIPT_A: &str = "var out = [];
    class P { #s = 'pa'; f = 'fa' + this.#s; static st = 'sa' + JSON.stringify([1]); get g() { return 'ga' + this.#s; } async am() { await 0; return 'ama' + this.#s; } static async sm() { await 0; return 'sma'; } }
    function mkp() { return new P(); }";
const SCRIPT_B: &str = "class Q { #s = 'pb'; f = 'fb' + this.#s; static st = 'sb' + JSON.stringify([2]); get g() { return 'gb' + this.#s; } async am() { await 0; return 'amb' + this.#s; } static async sm() { await 0; return 'smb'; } }
    class R extends P { r = 'rb' + this.f; constructor() { super(); this.c = 'cb'; } async am() { return (await super.am()) + 'R'; } static async sm() { return (await super.sm()) + 'R'; } }
    function mkq() { return new Q(); }";

#[test]
fn colliding_ids_across_scripts_in_calls_constructors_getters_and_async_methods() {
    big(|| {
        let mut it = realm(lim());
        assert_eq!(out(&mut it, SCRIPT_A), "undefined");
        assert_eq!(out(&mut it, SCRIPT_B), "undefined");
        // a third script constructs both, and a subclass whose parent lives in another script
        assert_eq!(out(&mut it, "var p = new P(), q = new Q(), r = new R(); [p.f, q.f, r.f, r.r, r.c, p.g, q.g, r.g, P.st, Q.st].join()"), "fapa,fbpb,fapa,rbfapa,cb,gapa,gbpb,gapa,sa[1],sb[2]");
        // async methods from three scripts, queued from a script and from a job
        out(&mut it, "P.sm().then(function (v) { out.push(v); }); Q.sm().then(function (v) { out.push(v); }); R.sm().then(function (v) { out.push(v); });
                      p.am().then(function (v) { out.push(v); }); q.am().then(function (v) { out.push(v); }); r.am().then(function (v) { out.push(v); });
                      Promise.resolve().then(function () { out.push(mkp().g + mkq().g + new R().f); });");
        it.drain_microtasks().unwrap();
        assert_eq!(out(&mut it, "out.join()"), "gapagbpbfapa,sma,smb,smaR,amapa,ambpb,amapaR".replace("sma,smb,smaR,amapa,ambpb,amapaR", "sma,smb,amapa,ambpb,smaR,amapaR"));
        assert_eq!(it.cur, 0);
        // the host calls closures of every script, after other scripts ran
        let f = it.run_script("(function () { return mkq().g + mkp().g + new R().c; })").unwrap();
        out(&mut it, "var unrelated = { a: [1, 2, 3], b: function () { return 1; }, c: class { d = 1; } };");
        let r = it.call(f, Value::Undefined, &[]).unwrap();
        assert_eq!(it.display(r), "gbpbgapacb");
        assert_eq!(it.cur, 0);
        it.collect(&[]).unwrap();
        assert_eq!(out(&mut it, "mkq().g + mkp().g + new R().r"), "gbpbgaparbfapa");
    });
}

#[test]
fn class_code_restores_the_callers_script_on_throw_limit_and_resource_error() {
    big(|| {
        let mut it = realm(lim());
        out(&mut it, "class T { x = (function () { throw new Error('field'); })(); } class S { x = 1; static s = 2; } function spin() { while (true) {} }
                      class L { x = spin(); } class G { get g() { throw new Error('getter'); } } class C { constructor() { throw new Error('ctor'); } }
                      class D extends C { y = 1; }");
        out(&mut it, "class Z { z = 1; } function after() { return 'after' + new Z().z + new S().x; } var m = [];
                      for (var K of [T, C, D]) { try { new K(); } catch (e) { m.push(e.message); } } try { new G().g; } catch (e) { m.push(e.message); }");
        assert_eq!(it.cur, 0);
        assert_eq!(out(&mut it, "m.join() + after()"), "field,ctor,ctor,getterafter11");
        assert_eq!(out(&mut it, "new L()"), "limit:Steps");
        assert_eq!(it.cur, 0);
        assert_eq!(out(&mut it, "after()"), "after11");
        // call-depth limit inside a field initializer chain that crosses scripts
        out(&mut it, "class Rec { x = new Rec(); }");
        out(&mut it, "function deep() { return new Rec(); }");
        assert_eq!(out(&mut it, "deep()"), "limit:CallDepth");
        assert_eq!(it.cur, 0);
        assert_eq!(out(&mut it, "after()"), "after11");
        // an environment cap hit while a constructor of another script runs
        let mut small = realm(EvalLimits { max_envs: 40, ..lim() });
        out(&mut small, "class W { a = 1; constructor() { this.b = 2; } } var keep = [];");
        let r = out(&mut small, "function mk(i) { return function () { return i; }; } for (var i = 0; i < 1000; i++) keep.push(mk(i)); 0");
        assert_eq!(r, "limit:Envs");
        assert_eq!(small.cur, 0);
        assert_eq!(out(&mut small, "typeof W"), "function");
    });
}

// ------------------------------------------------------------------ gates

#[test]
fn async_class_methods_run_and_unsupported_positions_are_found_before_any_effect() {
    big(|| {
        let mut it = realm(lim());
        assert_eq!(out(&mut it, "class A { async m() { await 1; } static async n() {} }; typeof A.prototype.m"), "function");
        for (src, what) in [
            ("class A { m() { return 1; } } class B extends A { async m() { return super[await 1](); } }", "await in a super property key"),
            ("class A {} class B extends A { async m() { return async () => super[await 1]; } }", "await in a super property key"),
            ("class A { async m() { return class { [await 1]() {} }; } }", "await in this position"),
            ("class A { async m() { return class extends (await Object) {}; } }", "await in this position"),
            ("class A { #x = 1; async m(o) { return o?.#x + await 1; } async n(o) { return (await o)?.#x; } }", "await inside an optional chain"),
            ("class A { #x = 1; async m() { this.#x++; return await this.#x; } async n() { return (await this).#x++; } }", "await inside ++ or --"),
        ] {
            let mut it = realm(lim());
            let r = out(&mut it, &format!("var effect = 1; {src}"));
            assert_eq!(r, format!("unsupported:{what}"), "{src}");
            assert_eq!(out(&mut it, "typeof effect"), "undefined", "{src}");
            assert_eq!(it.async_stats().marks, 0, "{src}");
        }
    });
}

#[test]
fn subclassing_promise_and_host_constructors_is_refused_not_mis_branded() {
    big(|| {
        let mut it = realm(lim());
        // defining the class is fine. Constructing it needs `new.target` in the parent, which Promise and a host constructor do not take.
        assert_eq!(out(&mut it, "class P extends Promise {} var made = 0; function mk() { var p = new P(function () {}); made = 1; return p; } typeof P"), "function");
        assert_eq!(out(&mut it, "mk()"), "unsupported:subclassing Promise (new.target is not passed to the constructor)");
        assert_eq!(out(&mut it, "made + ',' + (Promise.resolve(1) instanceof Promise)"), "0,true");
        assert_eq!(out(&mut it, "class Q extends Promise { constructor(f) { super(f); } } try { new Q(function () {}); } catch (e) { 'caught' }"), "unsupported:subclassing Promise (new.target is not passed to the constructor)");
        assert_eq!(it.cur, 0);
        // a plain `new Promise` and an async method still work afterwards
        assert_eq!(out(&mut it, "var ok = []; new Promise(function (r) { r(1); }).then(function (v) { ok.push(v); }); class A { async m() { await 0; ok.push('m'); } } new A().m(); 0"), "0");
        it.drain_microtasks().unwrap();
        assert_eq!(out(&mut it, "ok.join()"), "1,m");
    });
}

// ------------------------------------------------------------------ collection

#[test]
fn private_state_reachable_only_through_a_suspended_activation_survives_and_then_goes() {
    big(|| {
        let mut it = realm(lim());
        out(&mut it, "var never = new Promise(function () {}); var seen = [];
                      class A { #a = ['k' + 1]; #b = { n: 5 }; async m() { await never; seen.push(this.#a[0], this.#b.n); } }
                      (function () { new A().m(); })(); 0");
        assert_eq!(it.async_stats().activations, 1);
        assert_eq!(it.private_slots(), 2);
        for _ in 0..2 {
            it.collect(&[]).unwrap();
        }
        // the instance is held by `this` in the activation's environment, so its slots stay
        assert_eq!(it.private_slots(), 2);
        assert_eq!(it.async_stats().activations, 1);
        // the rest of the heap is still consistent: a fresh instance does not see the old one's slots
        assert_eq!(out(&mut it, "var fresh = new A(); fresh.constructor === A"), "true");
        assert_eq!(it.private_slots(), 4);
        // the awaited promise is only reachable from the global: drop it and the activation, instance and slots go
        out(&mut it, "never = null; fresh = null; 0");
        it.collect(&[]).unwrap();
        assert_eq!(it.async_stats().activations, 0);
        assert_eq!((it.async_stats().frames, it.async_stats().slots), (0, 0));
        assert_eq!(it.private_slots(), 0);
        assert_eq!(out(&mut it, "seen.length"), "0");
    });
}

#[test]
fn class_and_private_cycles_through_activations_and_promises_are_collected() {
    big(|| {
        let mut it = realm(lim());
        out(&mut it, "var res; class N { #peer; #p = new Promise(function (r) { res = r; }); link(o) { this.#peer = o; } async run() { await this.#p; return this.#peer; } static mk() { var a = new N(), b = new N(); a.link(b); b.link(a); a.run(); b.run(); return 1; } }
                      for (var i = 0; i < 50; i++) N.mk(); 0");
        let (acts, slots) = (it.async_stats().activations, it.private_slots());
        assert_eq!((acts, slots), (100, 200));
        // `res` keeps only the last promise: b's. Its activation reaches b, b reaches a, and a's promise holds a's
        // activation. The other 49 pairs are garbage cycles.
        let freed = it.collect(&[]).unwrap();
        assert!(freed.objects > 100, "{freed:?}");
        assert_eq!(it.async_stats().activations, 2, "{:?}", it.async_stats());
        assert_eq!(it.private_slots(), 4);
        // settle the survivor: the activation resumes, completes, and its state is released
        let res = global(&mut it, "res");
        it.call(res, Value::Undefined, &[Value::Number(1.0)]).unwrap();
        it.drain_microtasks().unwrap();
        out(&mut it, "res = null; 0");
        it.collect(&[]).unwrap();
        assert_eq!((it.async_stats().activations, it.async_stats().frames, it.async_stats().slots), (0, 0, 0));
        assert_eq!(it.private_slots(), 0);
        assert_eq!(it.live_classes(), 1);
    });
}

#[test]
fn a_reused_object_slot_never_inherits_a_brand_or_an_activation() {
    big(|| {
        let mut it = realm(lim());
        out(&mut it, "class B { #x = 1; static has(o) { return #x in o; } async m() { await 0; if (B.has(this)) cnt++; } }
                      var cnt = 0; for (var i = 0; i < 100; i++) new B().m(); 0");
        it.drain_microtasks().unwrap();
        assert_eq!(out(&mut it, "cnt"), "100");
        it.collect(&[]).unwrap();
        assert_eq!(it.private_slots(), 0);
        assert_eq!(it.async_stats().activations, 0);
        // many new objects land on the freed slots: none has the brand
        assert_eq!(out(&mut it, "var plain = []; for (var i = 0; i < 300; i++) plain.push({}); var hits = 0; for (var j = 0; j < 300; j++) if (B.has(plain[j])) hits++; hits"), "0");
        assert_eq!(out(&mut it, "var b2 = new B(); b2.m(); B.has(b2) + ',' + B.has({})"), "true,false");
        it.drain_microtasks().unwrap();
        assert_eq!(out(&mut it, "cnt"), "101");
    });
}

// ------------------------------------------------------------------ checkpoint contract

#[test]
fn a_limit_in_a_resumed_class_method_poisons_later_drains_and_runs_no_more_js() {
    big(|| {
        let mut it = realm(EvalLimits { max_steps: 200_000, ..lim() });
        out(&mut it, "var n = 0; var tail = 0; class A { #c = 0; async m() { await 0; this.#c++; n++; while (true) {} } async k() { await 0; tail++; } }
                      new A().m(); new A().k(); 0");
        let first = it.drain_microtasks();
        assert!(matches!(first, Err(Error::Limit(LimitKind::Steps))), "{first:?}");
        // the poisoned checkpoint repeats the error and the second job never runs
        assert!(matches!(it.drain_microtasks(), Err(Error::Limit(LimitKind::Steps))));
        assert_eq!(out(&mut it, "n + ',' + tail"), "1,0");
        assert_eq!(it.cur, 0);
        // the second job never ran: its activation is still suspended behind its queued job
        assert_eq!(it.async_stats().activations, 1);
        // a limit is never a rejection
        assert_eq!(it.unhandled_rejections().len(), 0);
    });
}

#[test]
fn a_throw_in_a_resumed_class_method_rejects_and_the_jobs_per_drain_cap_is_resumable() {
    big(|| {
        let mut it = realm(lim());
        it.set_promise_limits(PromiseLimits { max_jobs_per_drain: 2, ..Default::default() });
        out(&mut it, "var log = []; class A { #v; constructor(v) { this.#v = v; } async m() { await 0; log.push(this.#v); if (this.#v == 1) throw new Error('t'); await 0; log.push(this.#v + 10); } }
                      var ps = [new A(1).m(), new A(2).m(), new A(3).m()]; 0");
        let r = it.drain_microtasks();
        assert!(matches!(r, Err(Error::Limit(LimitKind::DrainJobs))), "{r:?}");
        assert_eq!(out(&mut it, "log.join()"), "1,2");
        // refused before a job started: the rest is still queued and runs now, in order
        loop {
            match it.drain_microtasks() {
                Ok(_) => break,
                Err(Error::Limit(LimitKind::DrainJobs)) => {}
                Err(e) => panic!("{e:?}"),
            }
        }
        assert_eq!(out(&mut it, "log.join()"), "1,2,3,12,13");
        assert_eq!(out(&mut it, "ps[0] instanceof Promise"), "true");
        assert_eq!(it.unhandled_rejections().len(), 1);
    });
}
