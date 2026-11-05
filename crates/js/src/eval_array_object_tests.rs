//! Tests of the Array/Object/Math methods (`eval_array_object.rs`). The corpus is checked against what Node gave
//! (`arrobj-cases.expected`, made by `tools/js/eval_oracle.mjs`; `tools/js/arrobj_check.sh` re-runs Node).
//! The tests below cover what Node cannot show: gates, limits, GC, symbol keys and the host API.

use super::*;
use crate::heap::{Heap, HeapLimits};
use crate::parse::{parse_script, Limits};

fn big<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(64 << 20).spawn(f).unwrap().join().unwrap()
}

fn esc(s: &str) -> String {
    s.replace('\\', "\\\\").replace('\n', "\\n").replace('\r', "\\r").replace('\t', "\\t")
}

fn show(it: &Interp, v: Value) -> String {
    match v {
        Value::String(_) => format!("string:{}", esc(&it.display(v))),
        Value::Number(_) => format!("number:{}", it.display(v)),
        Value::Bool(_) => format!("boolean:{}", it.display(v)),
        Value::Undefined => "undefined".into(),
        Value::Null => "null".into(),
        Value::Symbol(_) => "symbol".into(),
        Value::Object(_) => it.display(v),
    }
}

/// Run one script with these limits. `unsupported:WHAT`, `limit:KIND`, `throw:NAME`, or the shown result.
fn outcome(src: &str, limits: EvalLimits, heap: Heap) -> String {
    let prog = parse_script(src, Limits::default()).unwrap_or_else(|e| panic!("{src:?}: {e}"));
    let mut it = Interp::new(&prog, heap, limits).expect("setup");
    match it.run() {
        Ok(v) => show(&it, v),
        Err(Error::Throw(v)) => format!("throw:{}", if matches!(v, Value::Object(_)) { it.get_str(v, "name") } else { show(&it, v) }),
        Err(Error::Limit(l)) => format!("limit:{l:?}"),
        Err(Error::Unsupported(u)) => format!("unsupported:{}", u.what),
        Err(e) => format!("other:{e:?}"),
    }
}

fn run(src: &str) -> String {
    big({
        let src = src.to_string();
        move || outcome(&src, EvalLimits { max_steps: 2_000_000, ..Default::default() }, Heap::default())
    })
}

#[test]
fn corpus_matches_node() {
    let (cases, expected) = (include_str!("../../../tools/fixtures/js/arrobj-cases.txt"), include_str!("../../../tools/fixtures/js/arrobj-cases.expected"));
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
        let mut bad = Vec::new();
        for ((name, lines), w) in list.iter().zip(want) {
            let src = lines.join("\n");
            let got = format!("{name}\t{}", outcome_node_format(src.trim_end_matches('\n')));
            if got != w {
                bad.push(format!("node: {w}\nbare: {got}"));
            }
        }
        assert!(bad.is_empty(), "{} differences:\n{}", bad.len(), bad.join("\n"));
    });
}

/// The corpus format: a limit prints `limit`, a throw `throw:NAME:MESSAGE`.
fn outcome_node_format(src: &str) -> String {
    let prog = parse_script(src, Limits::default()).unwrap_or_else(|e| panic!("{src:?}: {e}"));
    let mut it = Interp::new(&prog, Heap::default(), EvalLimits { max_steps: 500_000, ..Default::default() }).expect("setup");
    match it.run() {
        Ok(v) => show(&it, v),
        Err(Error::Throw(v)) if matches!(v, Value::Object(_)) => format!("throw:{}:{}", esc(&it.get_str(v, "name")), esc(&it.get_str(v, "message"))),
        Err(Error::Throw(v)) => format!("throw-value:{}", show(&it, v)),
        Err(Error::Limit(_)) => "limit".into(),
        Err(e) => format!("other:{e:?}"),
    }
}

#[test]
fn gates_are_explicit_not_silent() {
    let gate = |src: &str, what: &str| assert_eq!(run(src), format!("unsupported:{what}"), "{src}");
    gate("var a=[1]; a.constructor={[Symbol.species]: function F(){}}; a.map(function(x){return x})", "Array species constructor (subclass or custom constructor)");
    gate("var a=[1]; a.constructor={[Symbol.species]: function F(){}}; a.filter(function(x){return x})", "Array species constructor (subclass or custom constructor)");
    gate("var a=[1]; a.constructor={[Symbol.species]: function F(){}}; a.slice()", "Array species constructor (subclass or custom constructor)");
    gate("var a=[1]; a.constructor={[Symbol.species]: function F(){}}; a.splice(0)", "Array species constructor (subclass or custom constructor)");
    gate("var a=[1]; a.constructor={[Symbol.species]: function F(){}}; a.concat()", "Array species constructor (subclass or custom constructor)");
    gate("Array.prototype.concat.call('abc')", "Array.prototype.concat on a primitive receiver (wrapper object)");
    gate("Object.assign(1, {a:1})", "Object.assign with a primitive target (wrapper object)");
    gate("Object.assign('s', {a:1})", "Object.assign with a primitive target (wrapper object)");
    gate("function F(){} Array.from.call(F, [1])", "Array.from with a non-Array this");
    gate("function F(){} Array.of.call(F, 1)", "Array.of with a non-Array this");
    // The things that are not gates: an `Array` constructor, a non-callable `constructor`, `undefined`.
    assert_eq!(run("var a=[1,2]; a.constructor={}; a.map(function(x){return x}).length"), "number:2");
    assert_eq!(run("var a=[1,2]; a.constructor=undefined; a.slice().length"), "number:2");
    assert_eq!(run("var a=[1,2]; a.constructor=null; try{a.slice()}catch(e){e.name}"), "string:TypeError");
}

#[test]
fn sparse_and_huge_ranges_are_limits_before_any_work() {
    // The step budget is 2M here. None of these may loop 2^32 times, grow memory, or be a JS exception.
    let lim = |src: &str, want: &str| assert_eq!(run(src), want, "{src}");
    lim("var a=[]; a[4294967294]=1; a.slice(0)", "limit:Steps");
    lim("var a=[]; a.length=4294967295; a.map(function(){})", "limit:Steps");
    lim("var a=[]; a.length=4294967295; a.filter(function(){})", "limit:Steps");
    lim("var a=[]; a.length=4294967295; a.sort()", "limit:Steps");
    lim("var a=[]; a.length=4294967295; a.shift()", "limit:Steps");
    lim("var a=[]; a.length=4294967295; a.unshift(1)", "limit:Steps");
    lim("var a=[]; a.length=4294967295; a.splice(0,0,1)", "limit:Steps");
    lim("var a=[]; a.length=4294967295; a.concat([1])", "limit:Steps");
    lim("var a=[]; a.length=4294967295; a.indexOf(1)", "limit:Steps"); // early-exit searches walk and tick
    lim("var a=[]; a.length=4294967295; a.includes(1)", "limit:Steps");
    lim("Array.from({length:4294967295})", "limit:Steps");
    lim("Array.prototype.every.call({length:4294967295},function(){return true})", "limit:Steps");
    // a limit cannot be caught
    lim("try{ var a=[]; a.length=4294967295; a.map(function(){}) }catch(e){ 'caught' }", "limit:Steps");
    // a result longer than 2^32-1 is a RangeError before the elements are read
    assert_eq!(run("var a=[]; a.length=4294967295; try{[1].concat(a)}catch(e){e.name}"), "string:RangeError");
    assert_eq!(run("Array.prototype.shift.call({length:4294967295})"), "limit:Steps");
}

#[test]
fn work_is_charged_for_hole_traversals_and_no_budget_is_free() {
    // 100k holes cost steps: a 50k budget must stop a full pass over them.
    let lim = EvalLimits { max_steps: 50_000, ..Default::default() };
    let r = big(move || outcome("var a=[]; a.length=100000; a.forEach(function(){}); 1", lim, Heap::default()));
    assert_eq!(r, "limit:Steps");
    let r = big(move || outcome("var a=[]; a.length=100000; a.reduce(function(x){return x},0)", lim, Heap::default()));
    assert_eq!(r, "limit:Steps");
    let r = big(move || outcome("var a=[]; a.length=100000; a.sort()", lim, Heap::default()));
    assert_eq!(r, "limit:Steps");
    let r = big(move || outcome("var a=[]; a.length=100000; a.lastIndexOf ? 0 : a.includes(1)", lim, Heap::default()));
    assert_eq!(r, "limit:Steps");
}

#[test]
fn temporary_storage_and_outputs_are_capped_by_the_heap_limit() {
    big(|| {
        // 1 MiB of heap: a sort buffer, an entries list or a map result of 100k items cannot fit.
        let mk = || Heap::new(HeapLimits { max_bytes: 1 << 20, ..Default::default() }).unwrap();
        let lim = EvalLimits { max_steps: 50_000_000, ..Default::default() };
        let fill = "var a=[]; for (var i=0;i<20000;i++) a.push(i); ";
        let src = format!("{fill} a.sort(); 1");
        // 20000 elements are about 320 KiB in the heap; the sort temp (32 bytes each) is under a quarter of 1 MiB? No: 640 KB > 256 KB.
        assert_eq!(outcome(&src, lim, mk()), "limit:Heap(Bytes)");
        let src = format!("{fill} Object.entries(a).length");
        assert_eq!(outcome(&src, lim, mk()), "limit:Heap(Bytes)");
        let src = format!("{fill} a.map(function(x){{return [x]}}).length");
        assert_eq!(outcome(&src, lim, mk()), "limit:Heap(Bytes)");
        // The same operations on a small array work under the same heap.
        assert_eq!(outcome("var a=[3,1,2]; a.sort().join()", lim, mk()), "string:1,2,3");
    });
}

#[test]
fn recursion_through_native_callbacks_hits_the_call_guard() {
    let r = run("function f(){ return [1].map(f) } f()");
    assert_eq!(r, "limit:CallDepth");
    let r = run("function g(){ return Array.from([1], g) } g()");
    assert_eq!(r, "limit:CallDepth");
    let r = run("var a=[2,1]; function h(){ return a.sort(h) } h()");
    assert_eq!(r, "limit:CallDepth");
    // the comparator that throws: a JS error, and no comparator effect after it
    assert_eq!(run("var n=0; try{[5,4,3,2,1].sort(function(){n++; throw new Error('x')})}catch(e){} n"), "number:1");
}

#[test]
fn sorted_flat_failures_leave_the_receiver_alone() {
    assert_eq!(run("var a=[3,2,1]; try{a.sort(function(){throw 1})}catch(e){} a.join()"), "string:3,2,1");
    assert_eq!(run("var a=[3,2,1]; try{a.sort(function(x,y){ return {valueOf:function(){throw 2}} })}catch(e){} a.join()"), "string:3,2,1");
}

#[test]
fn promise_jobs_see_native_callback_errors_as_rejections() {
    big(|| {
        let src = "var out = 'none'; Promise.resolve(1).then(function(){ return [1,2].map(function(){ throw new RangeError('boom') }) }).catch(function(e){ out = e.name });";
        let prog = parse_script(src, Limits::default()).unwrap();
        let mut it = Interp::new(&prog, Heap::default(), EvalLimits::default()).unwrap();
        it.run().unwrap();
        it.drain_microtasks().unwrap();
        let k = it.heap.key_from_str("out").unwrap();
        let Get::Value(v) = it.heap.get(it.global_object(), k).unwrap() else { panic!() };
        assert_eq!(it.display(v), "RangeError");
    });
}

#[test]
fn assign_copies_symbol_keys_with_get_and_set() {
    big(|| {
        let prog = parse_script("var log=[]; var t = Object.assign({}, src); t", Limits::default()).unwrap();
        let mut heap = Heap::default();
        let sym = heap.new_symbol(None).unwrap();
        let src_obj = heap.new_object(None).unwrap();
        // a data symbol property (enumerable), a non-enumerable one, and a string key
        let enumerable = crate::heap::PropertyDescriptor::data(Value::Number(7.0));
        heap.define_own_property(src_obj, PropertyKey::Symbol(sym), &enumerable).unwrap();
        let hidden = heap.new_symbol(None).unwrap();
        let d = crate::heap::PropertyDescriptor { value: Some(Value::Number(9.0)), writable: Some(true), enumerable: Some(false), configurable: Some(true), ..Default::default() };
        heap.define_own_property(src_obj, PropertyKey::Symbol(hidden), &d).unwrap();
        let k_str = heap.key_from_str("s").unwrap();
        heap.define_own_property(src_obj, k_str, &crate::heap::PropertyDescriptor::data(Value::Number(1.0))).unwrap();
        let mut it = Interp::new(&prog, heap, EvalLimits::default()).unwrap();
        let k_src = it.heap.key_from_str("src").unwrap();
        it.def(it.global_object(), k_src, Value::Object(src_obj), true, true, true).unwrap();
        let Value::Object(t) = it.run().unwrap() else { panic!() };
        let get = |it: &Interp, k: PropertyKey| match it.heap.get_own_property(t, k).unwrap() {
            Some(Property::Data { value: Value::Number(n), .. }) => Some(n),
            _ => None,
        };
        assert_eq!(get(&it, PropertyKey::Symbol(sym)), Some(7.0));
        assert_eq!(get(&it, PropertyKey::Symbol(hidden)), None);
        assert_eq!(get(&it, k_str), Some(1.0));
        // keys/values/entries skip symbols
        let prog2 = parse_script("Object.keys(src).length + ',' + Object.values(src).length + ',' + Object.entries(src).length", Limits::default()).unwrap();
        let mut heap2 = Heap::default();
        let s2 = heap2.new_symbol(None).unwrap();
        let o2 = heap2.new_object(None).unwrap();
        heap2.define_own_property(o2, PropertyKey::Symbol(s2), &crate::heap::PropertyDescriptor::data(Value::Number(1.0))).unwrap();
        let mut it2 = Interp::new(&prog2, heap2, EvalLimits::default()).unwrap();
        let k = it2.heap.key_from_str("src").unwrap();
        it2.def(it2.global_object(), k, Value::Object(o2), true, true, true).unwrap();
        let r = it2.run().unwrap();
        assert_eq!(it2.display(r), "0,0,0");
    });
}

#[test]
fn gc_keeps_methods_alive_and_reuses_slots_safely() {
    big(|| {
        let src = "function work(n){ var out=[]; for (var i=0;i<n;i++) out.push({k:i, v:[i,i+1]});
            var m = out.map(function(o){ return o.v.slice().concat([o.k]) }).filter(function(a){ return a[0]%2==0 });
            m.sort(function(x,y){ return y[2]-x[2] }); m.splice(0,1); m.unshift([0]);
            var e = Object.entries(Object.assign({}, out[0], {z:1})); 
            return m.length + ':' + e.length + ':' + Object.values(Object.fromEntries(e)).length + ':' + Array.from({length:2}, function(_,i){return i}).join() + ':' + Math.max(1,2); }";
        let prog = parse_script(src, Limits::default()).unwrap();
        let mut it = Interp::new(&prog, Heap::default(), EvalLimits::default()).unwrap();
        it.run().unwrap();
        let work = global_get(&mut it, "work");
        let r0 = it.call(work, Value::Undefined, &[Value::Number(50.0)]).unwrap();
        let want = it.display(r0);
        for round in 0..20 {
            // each round frees the temporary objects, so their slots are reused by the next round
            it.collect(&[]).unwrap();
            let r = it.call(work, Value::Undefined, &[Value::Number(50.0)]).unwrap();
            assert_eq!(it.display(r), want, "round {round}");
        }
        // the intrinsic Array constructor survives a collection that holds nothing else (species check keeps working)
        it.collect(&[]).unwrap();
        let f = global_get(&mut it, "work");
        assert!(matches!(it.call(f, Value::Undefined, &[Value::Number(3.0)]), Ok(Value::String(_))));
        // after collection the heap holds no more than a baseline: no per-call metadata accumulates
        it.collect(&[]).unwrap();
        let before = it.heap.stats();
        for _ in 0..5 {
            it.call(f, Value::Undefined, &[Value::Number(50.0)]).unwrap();
            it.collect(&[]).unwrap();
        }
        let after = it.heap.stats();
        assert_eq!(before.bytes, after.bytes);
    });
}

fn global_get(it: &mut Interp, name: &str) -> Value {
    let k = it.heap.key_from_str(name).unwrap();
    match it.heap.get(it.global_object(), k).unwrap() {
        Get::Value(v) => v,
        _ => panic!("accessor"),
    }
}

#[test]
fn removed_globals_do_not_break_the_species_check() {
    // The species check compares with the intrinsic, not with the global `Array`.
    assert_eq!(run("var A=Array; Array=1; var r=[1,2].map(function(x){return x}); r.length+','+(r.constructor===A)"), "string:2,true");
    assert_eq!(run("var A=Array; delete globalThis.Array; [1,2].slice().length"), "number:2");
}

// ---------------------------------------------------------------- Array.prototype.fill

#[test]
fn fill_receiver_gates_and_errors() {
    assert_eq!(run("Array.prototype.fill.call(1, 0)"), "unsupported:Array.prototype.fill on a primitive receiver (wrapper object)");
    assert_eq!(run("Array.prototype.fill.call('ab', 'x')"), "unsupported:Array.prototype.fill on a primitive receiver (wrapper object)");
    assert_eq!(run("Array.prototype.fill.call(undefined, 0)"), "throw:TypeError");
}

#[test]
fn fill_reserves_its_steps_before_the_first_write() {
    // The setter on index 5 counts writes. Under 500 steps, a fill of 1000 holes must fail before any write.
    big(|| {
        let setup = "var n = 0; Object.defineProperty(Array.prototype, '5', {set: function(){ n++ }, configurable: true}); ";
        let lim = EvalLimits { max_steps: 500, ..Default::default() };
        let small = format!("{setup} Array(100).fill(0); 1");
        let prog = parse_script(&small, Limits::default()).unwrap();
        let mut it = Interp::new(&prog, Heap::default(), lim).unwrap();
        assert!(it.run().is_ok());
        let n = global_get(&mut it, "n");
        assert_eq!(it.display(n), "1", "the small fill reaches the setter");

        let big_src = format!("{setup} Array(1000).fill(0); 1");
        let prog = parse_script(&big_src, Limits::default()).unwrap();
        let mut it = Interp::new(&prog, Heap::default(), lim).unwrap();
        assert!(matches!(it.run(), Err(Error::Limit(LimitKind::Steps))));
        let n = global_get(&mut it, "n");
        assert_eq!(it.display(n), "0", "the big fill never writes");
    });
}

#[test]
fn fill_huge_ranges_are_steps_limits_and_sparse_writes_are_cheap() {
    assert_eq!(run("Array(4294967295).fill(0)"), "limit:Steps");
    assert_eq!(run("var a=[]; a.length=4294967295; a.fill(0, 0, 4294967295)"), "limit:Steps");
    assert_eq!(run("var a=[]; a.length=4294967295; a.fill(1, 4294967290); a[4294967294]"), "number:1");
    assert_eq!(run("var a=[]; a.length=4294967295; a.fill(1, 4294967290); a.length"), "number:4294967295");
}

#[test]
fn fill_heap_cap_stops_growth_and_is_not_catchable() {
    big(|| {
        let mk = || Heap::new(HeapLimits { max_bytes: 1 << 20, ..Default::default() }).unwrap();
        let lim = EvalLimits { max_steps: 50_000_000, ..Default::default() };
        let prog = parse_script("Array(200000).fill(0); 1", Limits::default()).unwrap();
        let mut it = Interp::new(&prog, mk(), lim).unwrap();
        assert!(matches!(it.run(), Err(Error::Limit(LimitKind::Heap(Limit::Bytes)))));
        assert!(it.heap.stats().bytes <= 1 << 20, "the cap holds after the error");
        assert_eq!(outcome("var r='caught'; try{ Array(200000).fill(0) }catch(e){ r='caught' } r", lim, mk()), "limit:Heap(Bytes)");
    });
}

#[test]
fn fill_a_throwing_setter_stops_the_walk() {
    assert_eq!(run("var n=0; Object.defineProperty(Array.prototype,'1',{set:function(){n++; throw new Error('x')},configurable:true}); try{Array(4).fill(0)}catch(e){} n"), "number:1");
}

#[test]
fn fill_recursion_through_a_setter_hits_the_call_guard() {
    assert_eq!(run("Object.defineProperty(Array.prototype,'0',{set:function(v){ Array(1).fill(v) },configurable:true}); Array(1).fill(1)"), "limit:CallDepth");
}
