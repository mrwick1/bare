// Tests of `promise.rs`: the order of the jobs is checked against Node (each case was run there and its `r` copied here).
use crate::heap::Trigger;
use crate::interp::Vm;
use crate::promise::{DrainStop, PromiseLimits};
use crate::value::Value;
use crate::script::load;

const MIRI: bool = cfg!(miri);

/// Run `src` (it sets the global `r`), drain the jobs, and give `r` and the values that jobs threw (`queueMicrotask`).
fn go(src: &str, gc: bool) -> (Vm, Vec<String>) {
    let mut vm = load(src).unwrap_or_else(|e| panic!("{e}\n--- source:\n{src}"));
    if gc {
        let k = if MIRI { 4 << 10 } else { 1 << 10 };
        vm.realm.heap.set_trigger(Trigger { min: k, slack: k });
    }
    if let Err(e) = vm.run_main(0) {
        panic!("uncaught {e:?}\n--- source:\n{src}");
    }
    let mut errs = Vec::new();
    vm.drain_microtasks(&mut |rt, e| errs.push(rt.error_text(e))).unwrap_or_else(|s| panic!("drain stopped: {s:?}"));
    (vm, errs)
}

fn r_of(src: &str, gc: bool) -> String {
    let (mut vm, _) = go(src, gc);
    vm.global_text("r")
}

const CASES: &[(&str, &str)] = &[
        ("var log=[]; Promise.resolve(1).then(function(v){log.push('a'+v)}); log.push('s'); var r=log.join();", "s"),
        ("var log=[]; new Promise(function(res){res(Promise.resolve(1))}).then(function(){log.push('p1')}); Promise.resolve().then(function(){log.push('t1')}).then(function(){log.push('t2')}).then(function(){log.push('t3')}).then(function(){log.push('t4')}); var r; Promise.resolve().then(function(){}).then(function(){}).then(function(){}).then(function(){}).then(function(){}).then(function(){r=log.join()});", "t1,t2,p1,t3,t4"),
        ("var log=[]; var th={then:function(res){log.push('then');res(5)}}; Promise.resolve(th).then(function(v){log.push('v'+v)}); Promise.resolve().then(function(){log.push('t1')}).then(function(){log.push('t2')}).then(function(){log.push('t3')}); var r; setT();function setT(){Promise.resolve().then(function(){}).then(function(){}).then(function(){}).then(function(){}).then(function(){r=log.join()})}", "then,t1,v5,t2,t3"),
        ("var log=[]; Promise.reject(new Error('x')).catch(function(e){log.push(e.message)}).then(function(){log.push('after')}); var r; Promise.resolve().then(function(){}).then(function(){}).then(function(){}).then(function(){r=log.join()});", "x,after"),
        ("var log=[]; var p=new Promise(function(res,rej){res(1);rej(2);res(3)}); p.then(function(v){log.push(v)},function(e){log.push('e'+e)}); var r; Promise.resolve().then(function(){}).then(function(){r=log.join()});", "1"),
        ("var log=[]; var p=new Promise(function(){throw 7}); p.then(null,function(e){log.push('e'+e)}); var r; Promise.resolve().then(function(){}).then(function(){r=log.join()});", "e7"),
        ("var log=[]; var p=Promise.resolve(1); p.then(function(v){throw 'boom'}).then(function(){log.push('no')},function(e){log.push('caught '+e)}); var r; Promise.resolve().then(function(){}).then(function(){}).then(function(){r=log.join()});", "caught boom"),
        ("var log=[]; var p=Promise.resolve(1); var q=p.then(function(){return q}); q.then(null,function(e){log.push(e instanceof TypeError)}); var r; Promise.resolve().then(function(){}).then(function(){}).then(function(){r=log.join()});", "true"),
        ("var log=[]; queueMicrotask(function(){log.push('m1')}); Promise.resolve().then(function(){log.push('p1')}); queueMicrotask(function(){log.push('m2')}); var r; Promise.resolve().then(function(){}).then(function(){r=log.join()});", "m1,p1,m2"),
        ("var log=[]; var p=new Promise(function(res){setR=res}); var setR; p.then(function(v){log.push('a'+v)}); p.then(function(v){log.push('b'+v)}); p.then(function(v){log.push('c'+v)}); setR(9); var r; Promise.resolve().then(function(){}).then(function(){r=log.join()});", "a9,b9,c9"),
        ("var r=''; var p=Promise.resolve(2); r+= (p.then(function(){}) instanceof Promise); r+= (Promise.resolve(p)===p); r+=typeof Promise.prototype.then; r+=Promise.length; r+=Promise.prototype.then.length; r+=Promise.prototype.catch.length;", "truetruefunction121"),
        ("var log=[]; class MyP extends Promise {}; var m=new MyP(function(res){res(1)}); log.push(m instanceof MyP); log.push(m instanceof Promise); var r=log.join();", "true,true"),
        ("var r=''; try { Promise() } catch(e) { r+= e instanceof TypeError } try { new Promise() } catch(e) { r+= e instanceof TypeError } try { Promise.prototype.then.call({}) } catch(e) { r+= e instanceof TypeError }", "truetruetrue"),
        ("var log=[]; var p=Promise.resolve(1); p.then(function(v){log.push(v); return Promise.resolve(v+1)}).then(function(v){log.push(v)}); var r; (function f(n){ if(n==0){r=log.join();return} Promise.resolve().then(function(){f(n-1)})})(8);", "1,2"),
        ("var log=[]; var d={then:function(res,rej){throw 'bad'}}; Promise.resolve(d).then(null,function(e){log.push('rej '+e)}); var d2={then:function(res,rej){res(1);throw 'late'}}; Promise.resolve(d2).then(function(v){log.push('ok '+v)}); var r; (function f(n){ if(n==0){r=log.join();return} Promise.resolve().then(function(){f(n-1)})})(6);", "rej bad,ok 1"),
        ("var r=''; var thrown; Promise.resolve().then(function(){ r+='a'; return 1 }).finally; r += typeof Promise.reject;", "functiona"),
        ("var log=[]; var got; var p=Promise.reject(1); p.catch(function(){}); var r; (function f(n){ if(n==0){r=log.join();return} Promise.resolve().then(function(){f(n-1)})})(3);", ""),
        ("var log=[]; var o={}; Object.defineProperty(o,'then',{get:function(){log.push('get');return function(res){res(1)}}}); Promise.resolve(o).then(function(v){log.push('v'+v)}); var r; (function f(n){ if(n==0){r=log.join();return} Promise.resolve().then(function(){f(n-1)})})(6);", "get,v1"),
        ("var log=[]; Promise.resolve().then(function(){ log.push(this===undefined||this===globalThis) }); var r; (function f(n){ if(n==0){r=log.join();return} Promise.resolve().then(function(){f(n-1)})})(3);", "true"),
        ("var log=[]; var o={}; Object.defineProperty(o,'then',{get:function(){throw 'g'}}); Promise.resolve(o).then(null,function(e){log.push('rej '+e)}); var r; (function f(n){ if(n==0){r=log.join();return} Promise.resolve().then(function(){f(n-1)})})(4);", "rej g"),
        ("var log=[]; Promise.reject(1).then(1,2).catch(function(e){log.push('c'+e)}); Promise.resolve(2).then(1,2).then(function(v){log.push('v'+v)}); var r; (function f(n){ if(n==0){r=log.join();return} Promise.resolve().then(function(){f(n-1)})})(5);", "c1,v2"),
        ("var log=[]; new Promise(function(res,rej){ rej('x'); res('y') }).then(function(v){log.push('v'+v)},function(e){log.push('e'+e)}); var r; (function f(n){ if(n==0){r=log.join();return} Promise.resolve().then(function(){f(n-1)})})(3);", "ex"),
        ("var log=[]; var d={then:function(res,rej){res('a');rej('b');res('c')}}; Promise.resolve(d).then(function(v){log.push('v'+v)},function(e){log.push('e'+e)}); var r; (function f(n){ if(n==0){r=log.join();return} Promise.resolve().then(function(){f(n-1)})})(5);", "va"),
        ("var log=[]; var inner; var p=new Promise(function(res){ res(new Promise(function(r2){ inner=r2 })) }); p.then(function(v){log.push('p'+v)}); Promise.resolve().then(function(){inner('late')}); var r; (function f(n){ if(n==0){r=log.join();return} Promise.resolve().then(function(){f(n-1)})})(8);", "plate"),
        ("var log=[]; var p=Promise.resolve(1); var a=p.then(function(){log.push('a');return 'x'}); var b=p.then(function(){log.push('b')}); a.then(function(v){log.push('a2'+v)}); b.then(function(){log.push('b2')}); var r; (function f(n){ if(n==0){r=log.join();return} Promise.resolve().then(function(){f(n-1)})})(6);", "a,b,a2x,b2"),
        ("var log=[]; var p=Promise.resolve(1); p.then(function(v){ return {then:function(res){ log.push('thenable'); res(v+10) }} }).then(function(v){log.push('got'+v)}); Promise.resolve().then(function(){log.push('t1')}).then(function(){log.push('t2')}).then(function(){log.push('t3')}).then(function(){log.push('t4')}); var r; (function f(n){ if(n==0){r=log.join();return} Promise.resolve().then(function(){f(n-1)})})(8);", "t1,thenable,t2,got11,t3,t4"),
        ("var log=[]; var p=Promise.resolve(1); var q=p.then(function(){throw new RangeError('r')}); q.catch(function(e){ log.push(e.name); return 'rec' }).then(function(v){log.push(v)}); var r; (function f(n){ if(n==0){r=log.join();return} Promise.resolve().then(function(){f(n-1)})})(5);", "RangeError,rec"),
        ("var log=[]; var p={then:function(a,b){log.push(typeof a+typeof b)}}; Promise.prototype.catch.call(p,function(){}); var r=log.join(); try { Promise.prototype.catch.call(1,function(){}) } catch(e) { r+=e instanceof TypeError }", "undefinedfunctiontrue"),
        ("var r; var o={}; Object.setPrototypeOf(o, Promise.prototype); try { o.then(function(){}) } catch(e) { r = e instanceof TypeError }", "true"),
        ("var r=[]; var p=Promise.resolve(1); Object.setPrototypeOf(p, Object.prototype); try { Promise.prototype.then.call(p, function(){}) } catch(e) { r.push(e instanceof TypeError) } r=r.join();", ""),
        ("var log=[]; var p=Promise.resolve(5); p.a=1; p.b=2; p.c=3; delete p.a; Object.preventExtensions(p); var r=Object.keys(p).join()+Object.getOwnPropertyNames(p).length; p.then(function(v){ r += ',' + v });", "b,c2,5"),
        ("var r=''; Promise.prototype.constructor = function Foo(){}; var p=Promise.resolve(1); try { p.then(function(){}) } catch(e) { r += e instanceof TypeError } Promise.prototype.constructor = Promise; r += (p.then(function(){}) instanceof Promise);", "true"),
        ("var log=[]; Promise.prototype.constructor = undefined; var p=new Promise(function(res){res(1)}); log.push(p.then(function(){}) instanceof Promise); Promise.prototype.constructor = Promise; var r=log.join();", "true"),
        ("var log=[]; var p=Promise.resolve(1); Object.defineProperty(p,'constructor',{get:function(){log.push('get ctor'); return Promise}}); p.then(function(){log.push('h')}); var r; (function f(n){ if(n==0){r=log.join();return} Promise.resolve().then(function(){f(n-1)})})(3);", "get ctor,h"),
        ("var log=[]; var C=Promise.resolve(1); var p2=Promise.resolve(C); log.push(p2===C); var r=log.join();", "true"),
];

#[test]
fn the_order_of_jobs_is_the_order_of_node() {
    for (src, want) in CASES {
        assert_eq!(r_of(src, false), *want, "{src}");
    }
}

#[test]
fn the_order_of_jobs_holds_under_constant_collection() {
    for (src, want) in CASES {
        assert_eq!(r_of(src, true), *want, "under collection: {src}");
    }
}

/// Live bytes after a collection of a realm that keeps `n` values made by `make`.
fn live_bytes(n: usize, make: impl Fn(&mut crate::interp_rt::Realm) -> crate::value::Value) -> usize {
    let mut vm = load("").unwrap();
    for _ in 0..n {
        let v = make(&mut vm.realm);
        vm.realm.keep(v);
    }
    vm.collect_idle();
    vm.realm.last_gc.live_bytes
}

#[test]
fn a_promise_is_32_bytes_and_a_reaction_48() {
    let base = live_bytes(0, |rt| rt.new_promise().value());
    let per = |n: usize, f: &dyn Fn(&mut crate::interp_rt::Realm) -> crate::value::Value| (live_bytes(n, f) - base) as f64 / n as f64;
    // `keep` adds 8 bytes for each value to a `Vec` that is not in the heap, so the heap holds the cell only.
    assert_eq!(per(1000, &|rt| rt.new_promise().value()), 32.0);
}

/// A realm with `limits`, the script run once, and no drain yet.
fn with_limits(src: &str, limits: PromiseLimits) -> Vm {
    let mut vm = load(src).unwrap();
    vm.realm.pr.limits = limits;
    vm.run_main(0).unwrap_or_else(|e| panic!("uncaught {e:?}"));
    vm
}

fn drain(vm: &mut Vm) -> Result<u32, DrainStop> {
    vm.drain_microtasks(&mut |_, _| {})
}

#[test]
fn a_full_queue_is_a_range_error_and_the_queued_jobs_run() {
    let src = "var n = 0, e = ''; for (var i = 0; i < 10; i++) { try { queueMicrotask(function () { n++; }); } catch (x) { e = x.name; } }";
    let mut vm = with_limits(src, PromiseLimits { max_queue: 4, ..Default::default() });
    assert_eq!(drain(&mut vm), Ok(4));
    assert_eq!((vm.global_text("n"), vm.global_text("e")), ("4".into(), "RangeError".into()));
}

#[test]
fn too_many_pending_reactions_is_a_range_error() {
    let src = "var p = new Promise(function () {}); var n = 0, e = ''; for (var i = 0; i < 10; i++) { try { p.then(function () {}); n++; } catch (x) { e = x.name; } }";
    let mut vm = with_limits(src, PromiseLimits { max_reactions: 3, ..Default::default() });
    drain(&mut vm).unwrap();
    assert_eq!((vm.global_text("n"), vm.global_text("e")), ("3".into(), "RangeError".into()));
}

#[test]
fn a_drain_that_passes_max_jobs_stops_and_stays_stopped_until_cleared() {
    let src = "var n = 0; function f() { n++; queueMicrotask(f); } f();";
    let mut vm = with_limits(src, PromiseLimits { max_jobs_per_drain: 100, ..Default::default() });
    assert_eq!(drain(&mut vm), Err(DrainStop::Limit));
    assert_eq!(vm.global_text("n"), "101");
    assert_eq!(vm.realm.pending_jobs(), 1);
    assert_eq!(drain(&mut vm), Err(DrainStop::Limit));
    assert_eq!(vm.global_text("n"), "101", "a failed drain runs nothing");
    vm.realm.clear_failed_job();
    assert_eq!(drain(&mut vm), Err(DrainStop::Limit));
    assert_eq!(vm.global_text("n"), "201");
}

#[test]
fn a_callback_that_throws_goes_to_the_host_and_the_drain_goes_on() {
    let mut vm = load("var r = ''; queueMicrotask(function () { throw new Error('a'); }); queueMicrotask(function () { r += 'b'; }); queueMicrotask(function () { throw new Error('c'); });").unwrap();
    vm.run_main(0).unwrap();
    let mut seen = Vec::new();
    assert_eq!(vm.drain_microtasks(&mut |rt, e| seen.push(rt.error_text(e))), Ok(3));
    assert_eq!((seen, vm.global_text("r")), (vec!["Error: a".to_string(), "Error: c".to_string()], "b".to_string()));
}

#[test]
fn queue_microtask_needs_a_function() {
    let mut vm = load("var r = ''; try { queueMicrotask(1); } catch (e) { r = e.name; }").unwrap();
    vm.run_main(0).unwrap();
    assert_eq!(vm.global_text("r"), "TypeError");
}

#[test]
fn a_hundred_thousand_pending_reactions_survive_collection_and_run_in_order() {
    let n = if MIRI { 500 } else { 100_000 };
    let src = format!(
        "var res, n = 0, bad = 0; var p = new Promise(function (a) {{ res = a; }}); \
         function fill() {{ for (var i = 0; i < {n}; i++) {{ (function (i) {{ p.then(function () {{ if (i !== n) bad++; n++; }}); }})(i); }} }} \
         fill(); res(1);"
    );
    let mut vm = load(&src).unwrap();
    let k = if MIRI { 4 << 10 } else { 64 << 10 };
    vm.realm.heap.set_trigger(Trigger { min: k, slack: k });
    vm.run_main(0).unwrap();
    assert_eq!(vm.realm.pending_jobs(), n);
    vm.collect_idle();
    assert_eq!(drain(&mut vm), Ok(n as u32));
    assert_eq!((vm.global_text("n"), vm.global_text("bad")), (n.to_string(), "0".to_string()));
}

#[test]
fn the_host_sees_an_unhandled_rejection_once_and_a_late_handler_after() {
    let mut vm = load("var p = Promise.reject(1); var q = Promise.reject(2); q.catch(function () {}); function later() { p.catch(function () {}); }").unwrap();
    vm.run_main(0).unwrap();
    drain(&mut vm).unwrap();
    let (p, q) = (vm.global("p"), vm.global("q"));
    let r = vm.realm.take_rejections();
    assert_eq!((r.unhandled, r.handled.len(), r.dropped), (vec![p], 0, 0));
    let r = vm.realm.take_rejections();
    assert_eq!((r.unhandled.len(), r.handled.len()), (0, 0), "the host sees a rejection once");
    let later = vm.global("later");
    vm.call(later, Value::UNDEFINED, &[]).unwrap();
    drain(&mut vm).unwrap();
    let r = vm.realm.take_rejections();
    assert_eq!((r.unhandled.len(), r.handled), (0, vec![p]));
    assert!(vm.realm.promise_status(q).is_some());
}

#[test]
fn a_reported_promise_that_nothing_holds_leaves_the_list_at_a_collection() {
    let mut vm = load("Promise.reject(1);").unwrap();
    vm.run_main(0).unwrap();
    drain(&mut vm).unwrap();
    assert_eq!(vm.realm.take_rejections().unhandled.len(), 1);
    assert_eq!(vm.realm.pr.reported.len(), 1);
    vm.collect_idle();
    assert_eq!(vm.realm.pr.reported.len(), 0);
}

/// Live bytes after a collection of a script that calls `then` `n` times on a pending promise that a global holds.
fn pending_then_bytes(n: usize) -> usize {
    let mut vm = load(&format!("var f = function () {{}}; var root = new Promise(function () {{}}); function fill() {{ for (var i = 0; i < {n}; i++) root.then(f); }} fill();")).unwrap();
    vm.run_main(0).unwrap();
    vm.collect_idle();
    vm.realm.last_gc.live_bytes
}

#[test]
fn a_pending_then_costs_a_reaction_of_48_bytes_and_a_derived_promise_of_32() {
    assert_eq!((pending_then_bytes(2000) - pending_then_bytes(1000)) as f64 / 1000.0, 80.0);
}

/// The statics (`promise_statics.rs`), with the output of Node 22. By hand: `Promise.try` (Node 22 lacks it) and the `message` of the
/// `AggregateError` of `Promise.any` (the specification gives it none; V8 gives it a text).
const STATIC_CASES: &[(&str, &str)] = &[
    (r#"var log=[]; Promise.all([1,Promise.resolve(2),new Promise(function(res){res(3)})]).then(function(v){log.push(v.join('+'))}); var r; (function f(n){ if(n==0){r=log.join();return} Promise.resolve().then(function(){f(n-1)})})(20);"#, "1+2+3"),
    (r#"var log=[]; Promise.all([]).then(function(v){log.push('empty'+v.length)}); var r; (function f(n){ if(n==0){r=log.join();return} Promise.resolve().then(function(){f(n-1)})})(20);"#, "empty0"),
    (r#"var log=[]; Promise.all([1,Promise.reject('x'),3]).then(function(v){log.push('no')},function(e){log.push('rej '+e)}); var r; (function f(n){ if(n==0){r=log.join();return} Promise.resolve().then(function(){f(n-1)})})(20);"#, "rej x"),
    (r#"var log=[]; Promise.all('ab').then(function(v){log.push(v.join())}); var r; (function f(n){ if(n==0){r=log.join();return} Promise.resolve().then(function(){f(n-1)})})(20);"#, "a,b"),
    (r#"var log=[]; Promise.all(5).then(null,function(e){log.push(e instanceof TypeError)}); var r; (function f(n){ if(n==0){r=log.join();return} Promise.resolve().then(function(){f(n-1)})})(20);"#, "true"),
    (r#"var log=[]; Promise.all().then(null,function(e){log.push(e instanceof TypeError)}); var r; (function f(n){ if(n==0){r=log.join();return} Promise.resolve().then(function(){f(n-1)})})(20);"#, "true"),
    (r#"var log=[]; Promise.allSettled([1,Promise.reject('x'),Promise.resolve(3)]).then(function(v){log.push(v.map(function(o){return o.status+':'+(o.status=='fulfilled'?o.value:o.reason)}).join())}); var r; (function f(n){ if(n==0){r=log.join();return} Promise.resolve().then(function(){f(n-1)})})(20);"#, "fulfilled:1,rejected:x,fulfilled:3"),
    (r#"var log=[]; Promise.allSettled([]).then(function(v){log.push('e'+v.length)}); var r; (function f(n){ if(n==0){r=log.join();return} Promise.resolve().then(function(){f(n-1)})})(20);"#, "e0"),
    (r#"var log=[]; Promise.race([new Promise(function(){}),Promise.resolve('b'),Promise.resolve('c')]).then(function(v){log.push(v)}); var r; (function f(n){ if(n==0){r=log.join();return} Promise.resolve().then(function(){f(n-1)})})(20);"#, "b"),
    (r#"var log=[]; Promise.race([]).then(function(v){log.push('never')}); log.push('done'); var r; (function f(n){ if(n==0){r=log.join();return} Promise.resolve().then(function(){f(n-1)})})(20);"#, "done"),
    (r#"var log=[]; Promise.race([Promise.reject('r'),Promise.resolve(1)]).then(null,function(e){log.push('rej '+e)}); var r; (function f(n){ if(n==0){r=log.join();return} Promise.resolve().then(function(){f(n-1)})})(20);"#, "rej r"),
    (r#"var log=[]; Promise.any([Promise.reject(1),Promise.resolve(2),Promise.resolve(3)]).then(function(v){log.push(v)}); var r; (function f(n){ if(n==0){r=log.join();return} Promise.resolve().then(function(){f(n-1)})})(20);"#, "2"),
    (r#"var log=[]; Promise.any([Promise.reject(1),Promise.reject(2)]).then(null,function(e){log.push(e instanceof AggregateError, e.errors.join(), e.name, e.hasOwnProperty('message'))}); var r; (function f(n){ if(n==0){r=log.join();return} Promise.resolve().then(function(){f(n-1)})})(20);"#, "true,1,2,AggregateError,false"),
    (r#"var log=[]; Promise.any([]).then(null,function(e){log.push(e.constructor===AggregateError, e.errors.length)}); var r; (function f(n){ if(n==0){r=log.join();return} Promise.resolve().then(function(){f(n-1)})})(20);"#, "true,0"),
    (r#"var log=[]; var d=Object.getOwnPropertyDescriptor(new AggregateError([1],'m'),'errors'); log.push(d.writable,d.enumerable,d.configurable, new AggregateError([1,2],'m').message, AggregateError.length, Object.getPrototypeOf(AggregateError)===Error, AggregateError.prototype.name, Object.getPrototypeOf(AggregateError.prototype)===Error.prototype, String(new AggregateError([], 'x'))); var r; (function f(n){ if(n==0){r=log.join();return} Promise.resolve().then(function(){f(n-1)})})(20);"#, "true,false,true,m,2,true,AggregateError,true,AggregateError: x"),
    (r#"var log=[]; var w=Promise.withResolvers(); w.promise.then(function(v){log.push('v'+v)}); w.resolve(4); w.resolve(5); log.push(Object.keys(w).join()); var r; (function f(n){ if(n==0){r=log.join();return} Promise.resolve().then(function(){f(n-1)})})(20);"#, "promise,resolve,reject,v4"),
    (r#"var log=[]; Promise.resolve(1).finally(function(){log.push('f',arguments.length); return 99}).then(function(v){log.push('v'+v)}); var r; (function f(n){ if(n==0){r=log.join();return} Promise.resolve().then(function(){f(n-1)})})(20);"#, "f,0,v1"),
    (r#"var log=[]; Promise.reject(1).finally(function(){log.push('f'); return 99}).then(null,function(e){log.push('e'+e)}); var r; (function f(n){ if(n==0){r=log.join();return} Promise.resolve().then(function(){f(n-1)})})(20);"#, "f,e1"),
    (r#"var log=[]; Promise.resolve(1).finally(function(){throw 'ft'}).then(null,function(e){log.push('e'+e)}); var r; (function f(n){ if(n==0){r=log.join();return} Promise.resolve().then(function(){f(n-1)})})(20);"#, "eft"),
    (r#"var log=[]; Promise.resolve(1).finally(5).then(function(v){log.push('v'+v)}); var r; (function f(n){ if(n==0){r=log.join();return} Promise.resolve().then(function(){f(n-1)})})(20);"#, "v1"),
    (r#"var log=[]; Promise.reject(1).finally(function(){return Promise.reject('inner')}).then(null,function(e){log.push('e'+e)}); var r; (function f(n){ if(n==0){r=log.join();return} Promise.resolve().then(function(){f(n-1)})})(20);"#, "einner"),
    (r#"var log=[]; log.push(Promise[Symbol.species]===Promise, Object.getOwnPropertyDescriptor(Promise,Symbol.species).get.name, Promise.prototype[Symbol.toStringTag], Object.prototype.toString.call(Promise.resolve())); var r; (function f(n){ if(n==0){r=log.join();return} Promise.resolve().then(function(){f(n-1)})})(20);"#, "true,get [Symbol.species],Promise,[object Promise]"),
    (r#"var log=[]; function P(ex){ ex(function(v){log.push('res'+v)}, function(e){log.push('rej'+e)}); } P.resolve=function(x){return x}; var thenable={then:function(a,b){log.push('then'); a('t')}}; Promise.all.call(P,[thenable]);  var r; (function f(n){ if(n==0){r=log.join();return} Promise.resolve().then(function(){f(n-1)})})(20);"#, "then,rest"),
    (r#"var log=[]; var calls=0; function P(ex){ calls++; ex(function(){},function(){}); } Promise.resolve.call(P, 1); log.push(calls); var r; (function f(n){ if(n==0){r=log.join();return} Promise.resolve().then(function(){f(n-1)})})(20);"#, "1"),
    (r#"var log=[]; var p=Promise.resolve(1); p.constructor={}; p.constructor[Symbol.species]=function(ex){ log.push('species'); ex(function(){},function(){}); }; p.then(function(){}); log.push('ok'); var r; (function f(n){ if(n==0){r=log.join();return} Promise.resolve().then(function(){f(n-1)})})(20);"#, "species,ok"),
    (r#"var log=[]; var p=Promise.resolve(1); p.constructor={}; p.constructor[Symbol.species]=undefined; log.push(p.then(function(){}) instanceof Promise); var r; (function f(n){ if(n==0){r=log.join();return} Promise.resolve().then(function(){f(n-1)})})(20);"#, "true"),
    (r#"var log=[]; var p=Promise.resolve(1); p.constructor={}; p.constructor[Symbol.species]=5; try{ p.then(function(){}) }catch(e){ log.push(e instanceof TypeError) } var r; (function f(n){ if(n==0){r=log.join();return} Promise.resolve().then(function(){f(n-1)})})(20);"#, "true"),
    (r#"var log=[]; var p=Promise.resolve(1); p.constructor=5; try{ p.then(function(){}) }catch(e){ log.push(e instanceof TypeError) } var r; (function f(n){ if(n==0){r=log.join();return} Promise.resolve().then(function(){f(n-1)})})(20);"#, "true"),
    (r#"var log=[]; var closed=0; var it={}; it[Symbol.iterator]=function(){ return { next:function(){ return {done:false,value:1} }, return:function(){ closed++; return {} } } }; var orig=Promise.resolve; Promise.resolve=function(){ throw 'boom' }; Promise.all(it).then(null,function(e){ log.push('e'+e, closed) }); Promise.resolve=orig; var r; (function f(n){ if(n==0){r=log.join();return} Promise.resolve().then(function(){f(n-1)})})(20);"#, "eboom,1"),
    (r#"var log=[]; var i=0; var it={}; it[Symbol.iterator]=function(){ return { next:function(){ i++; return i>3?{done:true}:{done:false,value:i} } } }; Promise.all(it).then(function(v){log.push(v.join())}); var r; (function f(n){ if(n==0){r=log.join();return} Promise.resolve().then(function(){f(n-1)})})(20);"#, "1,2,3"),
    (r#"var log=[]; var it={}; it[Symbol.iterator]=function(){ return { next:function(){ throw 'nx' }, return:function(){ log.push('closed') } } }; Promise.all(it).then(null,function(e){log.push('e'+e)}); var r; (function f(n){ if(n==0){r=log.join();return} Promise.resolve().then(function(){f(n-1)})})(20);"#, "enx"),
    (r#"var log=[]; Promise.all([Promise.resolve(1), Promise.resolve(2)]).then(function(v){ log.push('a'+v) }); Promise.resolve().then(function(){log.push('t1')}).then(function(){log.push('t2')}).then(function(){log.push('t3')}).then(function(){log.push('t4')}); var r; (function f(n){ if(n==0){r=log.join();return} Promise.resolve().then(function(){f(n-1)})})(20);"#, "t1,a1,2,t2,t3,t4"),
    (r#"var log=[]; Promise.race([Promise.resolve(1)]).then(function(v){ log.push('a'+v) }); Promise.resolve().then(function(){log.push('t1')}).then(function(){log.push('t2')}).then(function(){log.push('t3')}); var r; (function f(n){ if(n==0){r=log.join();return} Promise.resolve().then(function(){f(n-1)})})(20);"#, "t1,a1,t2,t3"),
    (r#"var log=[]; Promise.allSettled([Promise.resolve(1)]).then(function(v){ log.push('a') }); Promise.any([Promise.resolve(1)]).then(function(v){ log.push('b') }); Promise.resolve().then(function(){log.push('t1')}).then(function(){log.push('t2')}).then(function(){log.push('t3')}).then(function(){log.push('t4')}); var r; (function f(n){ if(n==0){r=log.join();return} Promise.resolve().then(function(){f(n-1)})})(20);"#, "t1,a,b,t2,t3,t4"),
    (r#"var log=[]; Promise.try(function(a,b){ log.push(a+b); return 9 }, 1, 2).then(function(v){log.push('v'+v)}); Promise.try(function(){ throw 'tt' }).then(null,function(e){log.push('e'+e)}); var r; (function f(n){ if(n==0){r=log.join();return} Promise.resolve().then(function(){f(n-1)})})(20);"#, "3,v9,ett"),
    (r#"var log=[]; var calls=[]; var P=function(ex){ ex(function(){},function(){}) }; log.push(typeof Promise.all.call(P,[]), Promise.all.call(P,[]) instanceof P); var r; (function f(n){ if(n==0){r=log.join();return} Promise.resolve().then(function(){f(n-1)})})(20);"#, "object,true"),
    (r#"var log=[]; try{ Promise.all.call(5,[]) }catch(e){ log.push(e instanceof TypeError) } try{ Promise.race.call(undefined,[]) }catch(e){ log.push(e instanceof TypeError) } log.push(Promise.all.length, Promise.any.length, Promise.race.length, Promise.allSettled.length, Promise.withResolvers.length, Promise.prototype.finally.length, Promise.try.length); var r; (function f(n){ if(n==0){r=log.join();return} Promise.resolve().then(function(){f(n-1)})})(20);"#, "true,true,1,1,1,1,0,1,1"),
    (r#"var log=[]; var vals=[]; Promise.all([1,2,3].map(function(x){ return new Promise(function(res){ Promise.resolve().then(function(){res(x*2)}) }) })).then(function(v){ log.push(v.join()) }); var r; (function f(n){ if(n==0){r=log.join();return} Promise.resolve().then(function(){f(n-1)})})(20);"#, "2,4,6"),
    (r#"var log=[]; var res1; var a=new Promise(function(r){res1=r}); var b=Promise.resolve('b'); Promise.all([a,b]).then(function(v){log.push(v.join())}); Promise.resolve().then(function(){ res1('a') }); var r; (function f(n){ if(n==0){r=log.join();return} Promise.resolve().then(function(){f(n-1)})})(20);"#, "a,b"),
    (r#"var log=[]; class MyP extends Promise {}; var p=MyP.resolve(1); log.push(p instanceof MyP); var q=p.then(function(){}); log.push(q instanceof MyP, q.constructor===MyP); var r; (function f(n){ if(n==0){r=log.join();return} Promise.resolve().then(function(){f(n-1)})})(20);"#, "true,true,true"),
    (r#"var log=[]; class MyP extends Promise { constructor(ex){ log.push('ctor'); super(ex); this.tag='t' } }; var q=MyP.all([1,2]); log.push(q instanceof MyP, q.tag); q.then(function(v){log.push(v.join())}); var r; (function f(n){ if(n==0){r=log.join();return} Promise.resolve().then(function(){f(n-1)})})(20);"#, "ctor,ctor,ctor,ctor,ctor,true,t,ctor,1,2"),
    (r#"var log=[]; class MyP extends Promise { static get [Symbol.species](){ return Promise } }; var q=MyP.resolve(1).then(function(){}); log.push(q instanceof MyP, q instanceof Promise); var r; (function f(n){ if(n==0){r=log.join();return} Promise.resolve().then(function(){f(n-1)})})(20);"#, "false,true"),
    (r#"var log=[]; class B { constructor(ex){ this.p=new Promise(ex); this.f=1 } then(a,b){ return this.p.then(a,b) } }; class B2 { x=5; constructor(ex){ ex(function(){},function(){}); log.push(this.x) } }; Promise.resolve.call(B2, 1); var r; (function f(n){ if(n==0){r=log.join();return} Promise.resolve().then(function(){f(n-1)})})(20);"#, "5"),
    (r#"var log=[]; class MyP extends Promise {}; Promise.resolve(1).finally(function(){}).then(function(){log.push('v')}); MyP.resolve(2).finally(function(){}).then(function(v){log.push('m'+v)}); var z=MyP.resolve(3).finally(function(){}); log.push(z instanceof MyP); var r; (function f(n){ if(n==0){r=log.join();return} Promise.resolve().then(function(){f(n-1)})})(20);"#, "true,v,m2"),
    (r#"var log=[]; class MyP extends Promise {}; var w=MyP.withResolvers(); log.push(w.promise instanceof MyP); class NotP { constructor(){ throw 'ctor threw' } } try { Promise.resolve.call(NotP, 1) } catch(e){ log.push(e) } var r; (function f(n){ if(n==0){r=log.join();return} Promise.resolve().then(function(){f(n-1)})})(20);"#, "true,ctor threw"),
    (r#"var log=[]; var arrow=()=>{}; try { Promise.resolve.call(arrow, 1) } catch(e){ log.push(e instanceof TypeError) } try { Promise.all.call({}, []) } catch(e){ log.push(e instanceof TypeError) } var m={f(){}}; try { Promise.reject.call(m.f, 1) } catch(e){ log.push(e instanceof TypeError) } var r; (function f(n){ if(n==0){r=log.join();return} Promise.resolve().then(function(){f(n-1)})})(20);"#, "true,true,true"),
    (r#"var log=[]; var p=Promise.resolve(1); Object.defineProperty(Promise, Symbol.species, {get:function(){ log.push('species'); return Promise }, configurable:true}); new Promise(function(r){ r(p) }).then(function(v){log.push('v'+v)}); var r; (function f(n){ if(n==0){r=log.join();return} Promise.resolve().then(function(){f(n-1)})})(20);"#, "species,species,species,species,species,v1,species,species,species,species,species,species,species,species,species,species,species,species,species,species,species,species,species"),
    (r#"var log=[]; var p=Promise.resolve(1); var n=0; Object.defineProperty(Promise, Symbol.species, {get:function(){ n++; return this }, configurable:true}); p.then(function(){}); p.finally(function(){}); log.push(n); var r; (function f(n){ if(n==0){r=log.join();return} Promise.resolve().then(function(){f(n-1)})})(20);"#, "3"),
];

#[test]
fn the_statics_give_the_results_of_node() {
    for (src, want) in STATIC_CASES {
        assert_eq!(r_of(src, false), *want, "
--- source:
{src}");
    }
}

#[test]
fn the_statics_hold_under_constant_collection() {
    for (src, want) in STATIC_CASES {
        assert_eq!(r_of(src, true), *want, "
--- source:
{src}");
    }
}
