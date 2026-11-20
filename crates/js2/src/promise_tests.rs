// Tests of `promise.rs`: the order of the jobs is checked against Node (each case was run there and its `r` copied here).
use crate::heap::Trigger;
use crate::interp::Vm;
use crate::promise::{DrainStop, PromiseLimits};
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
