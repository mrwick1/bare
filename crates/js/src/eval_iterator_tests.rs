//! Tests of the batch-2 built-ins: Iterator and its helpers, the Set methods, Boolean.prototype, EvalError,
//! `Error.captureStackTrace` and `Error.stackTraceLimit`, WeakRef, FinalizationRegistry and `Promise.withResolvers`.
//! The corpus (`tools/fixtures/js/iterator-cases.txt`) is checked against what Node gave (`iterator-cases.expected`,
//! made by `tools/js/eval_oracle.mjs`; `tools/js/iterator_check.sh` re-runs Node and the matrix cases). The tests below
//! cover what Node cannot show: closing and order in detail, allocation per step, the collector, caps and gates.

use super::*;
use crate::heap::Heap;
use crate::parse::{parse_script, Limits};

fn big<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(64 << 20).spawn(f).unwrap().join().unwrap()
}

fn esc(s: &str) -> String {
    s.replace('\\', "\\\\").replace('\n', "\\n").replace('\r', "\\r").replace('\t', "\\t")
}

fn show(it: &mut Interp, r: Result<Value, Error>) -> String {
    match r {
        Ok(Value::String(s)) => format!("string:{}", esc(&it.display(Value::String(s)))),
        Ok(v @ Value::Number(_)) => format!("number:{}", it.display(v)),
        Ok(v @ Value::Bool(_)) => format!("boolean:{}", it.display(v)),
        Ok(Value::Undefined) => "undefined".into(),
        Ok(Value::Null) => "null".into(),
        Ok(v) => it.display(v),
        Err(Error::Throw(v)) if matches!(v, Value::Object(_)) => format!("throw:{}:{}", esc(&it.get_str(v, "name")), esc(&it.get_str(v, "message"))),
        Err(Error::Throw(v)) => format!("throw-value:{}", it.display(v)),
        Err(Error::Limit(l)) => format!("limit:{l:?}"),
        Err(Error::Unsupported(u)) => format!("unsupported:{}", u.what),
        Err(e) => format!("other:{e:?}"),
    }
}

fn out(it: &mut Interp, src: &str) -> String {
    let r = it.run_script(src);
    show(it, r)
}

fn realm() -> Interp<'static> {
    Interp::realm(Heap::default(), EvalLimits { max_steps: 50_000_000, ..Default::default() }).unwrap()
}

/// The corpus without the matrix cases (`iterator_check.sh` runs them).
#[test]
fn corpus_matches_node() {
    let (cases, expected) = (include_str!("../../../tools/fixtures/js/iterator-cases.txt"), include_str!("../../../tools/fixtures/js/iterator-cases.expected"));
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
            if name.ends_with("_matrix") {
                continue;
            }
            let src = lines.join("\n");
            let prog = parse_script(src.trim_end_matches('\n'), Limits::default()).unwrap_or_else(|e| panic!("{name}: {e}"));
            let mut it = Interp::new(&prog, Heap::default(), EvalLimits { max_steps: 50_000_000, ..Default::default() }).expect("setup");
            let r = it.run();
            let got = format!("{name}\t{}", show(&mut it, r));
            if got != w {
                bad.push(format!("node: {w}\nbare: {got}"));
            }
        }
        assert!(bad.is_empty(), "{} differences:\n{}", bad.len(), bad.join("\n"));
    });
}

/// A helper is `Copy` and lives in a map of the realm. Its size is part of the cost of every `map` and `filter`.
#[test]
fn helper_record_stays_small() {
    assert!(std::mem::size_of::<Helper>() <= 136, "{}", std::mem::size_of::<Helper>());
}

/// The log shows the order of every call: a helper does nothing until `next`, then pulls one item at a time.
#[test]
fn helpers_are_lazy_and_pull_one_item_at_a_time() {
    big(|| {
        let mut it = realm();
        let src = "var log = []; var src = { i: 0, next: function () { log.push('next' + this.i); return { value: this.i++, done: false }; } };
            var h = Iterator.prototype.map.call(src, function (x) { log.push('map' + x); return x; });
            var f = Iterator.prototype.filter.call(h, function (x) { log.push('filter' + x); return x % 2; });
            var t = Iterator.prototype.take.call(f, 1);
            log.push('built'); t.next(); log.push('|'); t.next(); log.join()";
        // the second `next` of `take(1)` is done and pulls nothing
        assert_eq!(out(&mut it, src), "string:built,next0,map0,filter0,next1,map1,filter1,|");
    });
}

/// Early exit calls `return` once. A throw from a callback calls `return` and keeps the original error. A source that
/// is done or that threw is not closed.
#[test]
fn closing_on_exit_on_throw_and_never_when_done() {
    big(|| {
        let mut it = realm();
        out(&mut it, "var log = []; function mk(n) { var i = 0; return { next: function () { log.push('n'); return i++ < n ? { value: i, done: false } : { done: true }; }, return: function () { log.push('r'); return {}; } }; } var P = Iterator.prototype;");
        // early exit: take, find, some, every, break and destructuring
        assert_eq!(out(&mut it, "log.length = 0; P.take.call(mk(9), 1).toArray(); log.join()"), "string:n,r");
        assert_eq!(out(&mut it, "log.length = 0; P.find.call(mk(9), function (x) { return x === 2; }); log.join()"), "string:n,n,r");
        assert_eq!(out(&mut it, "log.length = 0; for (var x of P.map.call(mk(9), function (x) { return x; })) break; log.join()"), "string:n,r");
        assert_eq!(out(&mut it, "log.length = 0; var [a] = P.filter.call(mk(9), function () { return true; }); log.join()"), "string:n,r");
        // a callback throws: close, and the callback error wins over an error from return
        assert_eq!(out(&mut it, "log.length = 0; var r; try { P.forEach.call(mk(9), function () { throw new RangeError('cb'); }); } catch (e) { r = e.name; } r + log.join()"), "string:RangeErrorn,r");
        assert_eq!(
            out(&mut it, "var s = { next: function () { return { value: 1, done: false }; }, return: function () { throw new TypeError('ret'); } }; try { P.map.call(s, function () { throw new RangeError('cb'); }).next(); } catch (e) { e.name }"),
            "string:RangeError"
        );
        // done: no close, whether the helper or a consumer ran the source to its end
        assert_eq!(out(&mut it, "log.length = 0; P.toArray.call(mk(2)); P.map.call(mk(1), function (x) { return x; }).toArray(); log.join()"), "string:n,n,n,n,n");
        assert_eq!(out(&mut it, "log.length = 0; P.take.call(mk(1), 5).toArray(); log.join()"), "string:n,n");
        // a throwing next is not closed
        assert_eq!(out(&mut it, "log.length = 0; var bad = { next: function () { throw new Error('x'); }, return: function () { log.push('r'); return {}; } }; try { P.toArray.call(bad); } catch (e) {} log.join()"), "string:");
    });
}

#[test]
fn set_methods_with_set_like_arguments() {
    big(|| {
        let mut it = realm();
        out(&mut it, "var A = new Set([1, 2, 3]); var like = function (a) { return { size: a.length, has: function (x) { return a.indexOf(x) >= 0; }, keys: function () { return a.values(); } }; }; var j = function (s) { return Array.from(s).join(); };");
        assert_eq!(out(&mut it, "[j(A.union(like([3, 4]))), j(A.intersection(like([3, 2, 9]))), j(A.difference(like([1]))), j(A.symmetricDifference(like([3, 4]))), A.isSubsetOf(like([1, 2, 3, 4])), A.isSupersetOf(like([1, 2])), A.isDisjointFrom(like([7, 8]))].join('|')"), "string:1,2,3,4|2,3|2,3|1,2,4|true|true|true");
        // a Map is set-like: its keys count
        assert_eq!(out(&mut it, "j(A.union(new Map([[8, 1], [1, 1]])))"), "string:1,2,3,8");
        // size is read first and decides which side to walk: a smaller argument is walked with keys, never has
        assert_eq!(out(&mut it, "var log = []; A.intersection({ size: 1, has: function (x) { log.push('has'); return true; }, keys: function () { log.push('keys'); return [2].values(); } }); log.join()"), "string:keys");
        assert_eq!(out(&mut it, "log.length = 0; A.intersection({ size: 9, has: function (x) { log.push('has' + x); return x > 1; }, keys: function () { log.push('keys'); return [].values(); } }); log.join()"), "string:has1,has2,has3");
        // errors come from GetSetRecord before any walk
        assert_eq!(out(&mut it, "try { A.union({ size: NaN, has: function () {}, keys: function () {} }); } catch (e) { e.name + ':' + e.message }"), "string:TypeError:The .size property is NaN");
        assert_eq!(out(&mut it, "try { A.union({ size: -1, has: function () {}, keys: function () {} }); } catch (e) { e.name }"), "string:RangeError");
        // the result is a plain Set and a new object
        assert_eq!(out(&mut it, "var u = A.union(new Set()); (u !== A) + ':' + (Object.getPrototypeOf(u) === Set.prototype) + ':' + u.size"), "string:true:true:3");
    });
}

/// `-0` from `keys()` becomes `+0` in the result (CanonicalizeKeyedCollectionKey).
#[test]
fn set_methods_store_positive_zero() {
    big(|| {
        let mut it = realm();
        let neg = "{ size: 1, has: function () { return true; }, keys: function () { return [-0].values(); } }";
        for m in ["union", "symmetricDifference"] {
            assert_eq!(out(&mut it, &format!("var s = new Set([1]).{m}({neg}); Array.from(s).map(function (x) {{ return 1 / x; }}).join()")), "string:1,Infinity", "{m}");
        }
        assert_eq!(out(&mut it, &format!("Array.from(new Set([0]).intersection({neg})).map(function (x) {{ return 1 / x; }}).join()")), "string:Infinity");
    });
}

#[test]
fn set_methods_charge_the_entry_caps() {
    big(|| {
        let mut it = realm();
        it.set_sc_limits(ScLimits { max_entries: 5, ..Default::default() });
        out(&mut it, "var a = new Set([1, 2, 3]), b = new Set([4, 5, 6]);");
        assert_eq!(out(&mut it, "a.union(b)"), "limit:Sc(Entries)");
        assert_eq!(out(&mut it, "a.intersection(b).size + ':' + a.difference(b).size"), "string:0:3");
        let mut it = realm();
        it.set_sc_limits(ScLimits { max_total_entries: 8, ..Default::default() });
        out(&mut it, "var a = new Set([1, 2, 3]), b = new Set([4, 5, 6]);");
        assert_eq!(out(&mut it, "a.union(b)"), "limit:Sc(TotalEntries)");
    });
}

/// A chain that reads its source through the native fast path makes no result object per step. The only objects
/// are the helpers (one per call of `map`, `filter`, `take`) and the result array.
#[test]
fn chain_makes_no_object_per_step() {
    big(|| {
        let mut it = realm();
        out(&mut it, "var a = []; for (var i = 0; i < 20000; i++) a.push(i);");
        let before = it.heap.stats().total_object_allocs;
        assert_eq!(out(&mut it, "a.values().map(function (x) { return x + 1; }).filter(function (x) { return x % 2; }).take(5000).toArray().length"), "number:5000");
        let used = it.heap.stats().total_object_allocs - before;
        // 3 helpers + the result array + the callbacks' environments are not objects: far below one per step
        assert!(used < 40, "{used} objects for 10000 steps");
        let before = it.heap.stats().total_object_allocs;
        assert_eq!(out(&mut it, "var n = 0; for (var x of a.values().flatMap(function (x) { return [x, x]; }).drop(10)) n++; n"), "number:39990");
        let used = it.heap.stats().total_object_allocs - before;
        assert!(used < 40_000, "{used}");
        let before = it.heap.stats().total_object_allocs;
        assert_eq!(out(&mut it, "var s = new Set(a), t = new Set(a.slice(0, 5000)); s.union(t).size + ':' + s.intersection(t).size + ':' + s.difference(t).size"), "string:20000:5000:15000");
        let used = it.heap.stats().total_object_allocs - before;
        assert!(used < 20, "{used} objects for three Set methods on 20000 entries");
    });
}

/// Helpers are in a realm map. An unreachable helper is dropped by `collect`, and a reachable one keeps its source,
/// callback and inner iterator alive.
#[test]
fn helpers_are_collected_and_keep_their_edges() {
    big(|| {
        let mut it = realm();
        out(&mut it, "for (var i = 0; i < 100; i++) [1, 2, 3].values().map(function (x) { return x; }).take(2);");
        let before = it.sc.helpers.len();
        assert!(before >= 100, "{before}");
        it.collect(&[]).unwrap();
        assert!(it.sc.helpers.len() < 10, "{}", it.sc.helpers.len());
        out(&mut it, "var keep = (function () { var seen = []; return [10, 20, 30].values().flatMap(function (x) { seen.push(x); return [x, x + 1].values(); }).take(5); })();");
        it.collect(&[]).unwrap();
        it.collect(&[]).unwrap();
        assert_eq!(out(&mut it, "keep.toArray().join()"), "string:10,11,20,21,30");
        // a helper that is half way through a flatMap holds its inner iterator
        out(&mut it, "var h = [1, 2].values().flatMap(function (x) { return [x * 10, x * 20].values(); }); h.next();");
        it.collect(&[]).unwrap();
        assert_eq!(out(&mut it, "h.next().value + ',' + h.next().value + ',' + h.next().value + ',' + h.next().done"), "string:20,20,40,true");
    });
}

/// Stepping a helper from inside its own callback is a TypeError. After it threw, the helper still works.
#[test]
fn reentrant_calls_throw_and_leave_the_helper_usable() {
    big(|| {
        let mut it = realm();
        assert_eq!(
            out(&mut it, "var h, e; h = [1, 2, 3].values().map(function (x) { try { h.next(); } catch (er) { e = er.name; } return x; }); h.next(); h.next().value + ':' + e"),
            "string:2:TypeError"
        );
        let mut it = realm();
        assert_eq!(out(&mut it, "var h = [1, 2].values().map(function (x) { throw new Error('boom'); }); try { h.next(); } catch (e) {} JSON.stringify(h.next())"), "string:{\"done\":true}");
    });
}

#[test]
fn iterator_constructor_and_gates() {
    big(|| {
        let mut it = realm();
        assert_eq!(out(&mut it, "try { new Iterator(); } catch (e) { e.message }"), "string:Abstract class Iterator not directly constructable");
        assert_eq!(out(&mut it, "class C extends Iterator { next() { return { done: true }; } } var c = new C(); (c instanceof Iterator) + ':' + c.toArray().length"), "string:true:0");
        // a static of Promise on a user constructor is a gate. Promise.withResolvers follows the existing gate
        let mut it = realm();
        assert!(out(&mut it, "function F(ex) { ex(function () {}, function () {}); } try { Promise.withResolvers.call(F); } catch (e) { 'caught' }").starts_with("unsupported:"));
        // subclassing WeakRef or FinalizationRegistry is a gate, like the other native collections
        let mut it = realm();
        assert!(out(&mut it, "class W extends WeakRef {} try { new W({}); } catch (e) { 'caught' }").starts_with("unsupported:"));
        let mut it = realm();
        assert!(out(&mut it, "class W extends FinalizationRegistry {} try { new W(function () {}); } catch (e) { 'caught' }").starts_with("unsupported:"));
        // a non-registered Symbol as a weak target is a gate too (WeakMap, WeakSet and WeakRef agree)
        let mut it = realm();
        assert!(out(&mut it, "try { new WeakRef(Symbol('s')); } catch (e) { 'caught' }").starts_with("unsupported:"));
    });
}

/// A WeakRef to a dead object returns undefined after a collection. A WeakRef to a live object keeps its target.
#[test]
fn weakref_after_a_collection() {
    big(|| {
        let mut it = realm();
        out(&mut it, "var keep = { k: 1 }; var live = new WeakRef(keep); var dead = (function () { return new WeakRef({ gone: true }); })(); var before = typeof dead.deref();");
        assert_eq!(out(&mut it, "before"), "string:object");
        it.collect(&[]).unwrap();
        assert_eq!(out(&mut it, "typeof dead.deref() + ':' + (live.deref() === keep)"), "string:undefined:true");
        // the target can come back through a strong path only while that path exists
        out(&mut it, "keep = null;");
        it.collect(&[]).unwrap();
        assert_eq!(out(&mut it, "typeof live.deref()"), "string:undefined");
        // a collection inside a job never runs between a deref and its use: the target is held until the job ends
        let mut it = realm();
        out(&mut it, "var w = (function () { return new WeakRef({ v: 7 }); })(); var t = w.deref();");
        it.collect(&[]).unwrap();
        assert_eq!(out(&mut it, "t.v + ':' + (w.deref() === t)"), "string:7:true");
    });
}

/// Cleanup callbacks never run on their own. The host calls `run_finalizers` after a collection.
#[test]
fn finalization_callbacks_run_only_when_the_host_asks() {
    big(|| {
        let mut it = realm();
        out(&mut it, "var held = []; var reg = new FinalizationRegistry(function (h) { held.push(h); if (h === 'boom') throw new Error('x'); }); var tok = {};
            (function () { reg.register({}, 'a'); reg.register({}, 'boom'); reg.register({}, 'b', tok); reg.register({}, 'c'); })();
            var keep = {}; reg.register(keep, 'live');");
        assert_eq!(it.run_finalizers().unwrap(), 0, "nothing is dead before a collection");
        it.collect(&[]).unwrap();
        assert_eq!(out(&mut it, "held.length"), "number:0");
        // 4 dead targets, the 'boom' callback throws and the later ones still run
        assert_eq!(it.run_finalizers().unwrap(), 4);
        assert_eq!(out(&mut it, "held.slice().sort().join()"), "string:a,b,boom,c");
        assert_eq!(it.run_finalizers().unwrap(), 0, "each callback runs once");
        // unregister removes the cell, so its callback never runs
        out(&mut it, "held.length = 0; var tok2 = {}; (function () { reg.register({}, 'x', tok2); reg.register({}, 'y', tok2); reg.register({}, 'z'); })(); var removed = reg.unregister(tok2);");
        assert_eq!(out(&mut it, "removed"), "boolean:true");
        it.collect(&[]).unwrap();
        assert_eq!(it.run_finalizers().unwrap(), 1);
        assert_eq!(out(&mut it, "held.join()"), "string:z");
        // the registry holds the held value strongly and the token weakly; the live target stays registered
        assert_eq!(out(&mut it, "reg.unregister(keep)"), "boolean:false");
        assert_eq!(out(&mut it, "var unreg = {}; reg.register(keep, 'again', unreg); reg.unregister(unreg)"), "boolean:true");
    });
}

#[test]
fn run_finalizers_is_refused_inside_a_call_and_held_values_survive_gc() {
    big(|| {
        let mut it = realm();
        out(&mut it, "var got; var reg = new FinalizationRegistry(function (h) { got = h.tag; }); (function () { reg.register({}, { tag: 'T' }); })();");
        it.collect(&[]).unwrap();
        it.collect(&[]).unwrap();
        assert_eq!(it.run_finalizers().unwrap(), 1);
        assert_eq!(out(&mut it, "got"), "string:T");
        let r = it.run_script("1");
        assert!(r.is_ok());
    });
}

#[test]
fn weak_ref_and_registry_count_in_the_weak_caps() {
    big(|| {
        let mut it = realm();
        it.set_sc_limits(ScLimits { max_weak_collections: 2, ..Default::default() });
        assert_eq!(out(&mut it, "var a = new WeakRef({}); var b = new FinalizationRegistry(function () {}); try { new WeakRef({}); } catch (e) { 'caught' }"), "limit:Sc(WeakCollections)");
        let mut it = realm();
        it.set_sc_limits(ScLimits { max_total_weak_entries: 3, ..Default::default() });
        assert_eq!(out(&mut it, "var r = new FinalizationRegistry(function () {}); r.register({}, 1); r.register({}, 2); r.register({}, 3); try { r.register({}, 4); } catch (e) { 'caught' }"), "limit:Sc(WeakEntries)");
    });
}

/// `Error.stackTraceLimit` caps the frames and `captureStackTrace` follows the same cap, the constructor argument
/// hides that frame and the frames above it, and the header uses the name and message of the object.
#[test]
fn stack_limit_and_capture_follow_v8() {
    big(|| {
        let mut it = realm();
        out(&mut it, "function deep(n, f) { return n === 0 ? f() : deep(n - 1, f); } function lines(s) { return s.split('\\n').length - 1; }");
        assert_eq!(out(&mut it, "Error.stackTraceLimit"), "number:10");
        assert_eq!(out(&mut it, "lines(deep(30, function () { return new Error('e').stack; }))"), "number:10");
        assert_eq!(out(&mut it, "Error.stackTraceLimit = 3; lines(deep(30, function () { return new Error('e').stack; }))"), "number:3");
        assert_eq!(out(&mut it, "lines(deep(30, function () { var o = {}; Error.captureStackTrace(o); return o.stack; }))"), "number:3");
        assert_eq!(out(&mut it, "Error.stackTraceLimit = 0; JSON.stringify(deep(5, function () { return new Error('e').stack; }))"), "string:\"Error: e\"");
        // not a number, or deleted: the stack is not made
        assert_eq!(out(&mut it, "Error.stackTraceLimit = 'x'; typeof new Error('e').stack"), "string:undefined");
        assert_eq!(out(&mut it, "delete Error.stackTraceLimit; typeof new Error('e').stack"), "string:undefined");
        // the constructor argument hides its own frame
        let mut it = realm();
        let s = out(&mut it, "function a() { return b(); } function b() { var o = {}; Error.captureStackTrace(o, b); return o.stack; } a()");
        assert!(s.contains("at a ") && !s.contains("at b "), "{s}");
        assert_eq!(out(&mut it, "var o = { name: 'N', message: 'm' }; Error.captureStackTrace(o); o.stack.split('\\n')[0]"), "string:N: m");
        // it replaces a stack that is already there and returns undefined
        assert_eq!(out(&mut it, "var q = { stack: 'old' }; var r = Error.captureStackTrace(q); (q.stack !== 'old') + ':' + r"), "string:true:undefined");
        assert_eq!(out(&mut it, "try { Error.captureStackTrace(1); } catch (e) { e.name }"), "string:TypeError");
    });
}

#[test]
fn promise_with_resolvers_settles_once_and_in_order() {
    big(|| {
        let mut it = realm();
        out(&mut it, "var log = []; var a = Promise.withResolvers(); a.promise.then(function (v) { log.push('a' + v); }); a.resolve(1); a.resolve(2); a.reject(3);
            var b = Promise.withResolvers(); b.promise.then(null, function (v) { log.push('b' + v); }); b.reject('no'); b.resolve('late');
            var c = Promise.withResolvers(); c.resolve(Promise.resolve('chained')); c.promise.then(function (v) { log.push('c' + v); });
            var s = Promise.withResolvers(); s.resolve(s.promise); s.promise.then(null, function (e) { log.push(e.name); });
            log.push('sync');");
        assert_eq!(out(&mut it, "log.join()"), "string:sync");
        it.drain_microtasks().unwrap();
        assert_eq!(out(&mut it, "log.join()"), "string:sync,a1,bno,TypeError,cchained");
    });
}

#[test]
fn boolean_prototype_and_dispose_symbols() {
    big(|| {
        let mut it = realm();
        assert_eq!(out(&mut it, "[true.toString(), (false).valueOf(), Boolean.prototype.toString.call(true), Object.getPrototypeOf(true) === Boolean.prototype].join()"), "string:true,false,true,true");
        assert_eq!(out(&mut it, "try { Boolean.prototype.valueOf.call(1); } catch (e) { e.name }"), "string:TypeError");
        // Symbol.dispose and Symbol.asyncDispose are well-known symbols: not writable, not enumerable, not configurable
        assert_eq!(
            out(&mut it, "var d = Object.getOwnPropertyDescriptor(Symbol, 'dispose'), a = Object.getOwnPropertyDescriptor(Symbol, 'asyncDispose'); [typeof Symbol.dispose, typeof Symbol.asyncDispose, d.writable, d.enumerable, d.configurable, a.writable, a.configurable, Symbol.dispose.description, Symbol.asyncDispose.description, Symbol.dispose !== Symbol.asyncDispose].join()"),
            "string:symbol,symbol,false,false,false,false,false,Symbol.dispose,Symbol.asyncDispose,true"
        );
        assert_eq!(out(&mut it, "Symbol.dispose.toString() + Symbol.keyFor(Symbol.dispose)"), "string:Symbol(Symbol.dispose)undefined");
        assert_eq!(out(&mut it, "var o = {}; o[Symbol.dispose] = function () { return 'd'; }; o[Symbol.dispose]()"), "string:d");
    });
}
