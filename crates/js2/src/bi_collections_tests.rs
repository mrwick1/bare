// Tests of `Map`, `Set`, `WeakMap` and `WeakSet` (`bi_collections.rs`, `weaktab.rs`): each case is a program, the global to read and the
// expected text. Each runs normally and under constant collection. The weak ones are also checked against the collector's counters.
use crate::heap::Trigger;
use crate::script::{load, run};

fn ev(src: &str, name: &str) -> String {
    match run(src) {
        Ok(mut vm) => vm.global_text(name),
        Err(e) => panic!("{e}\n--- source:\n{src}"),
    }
}

fn ev_gc(src: &str, name: &str) -> String {
    let mut vm = load(src).unwrap_or_else(|e| panic!("{e}\n--- source:\n{src}"));
    let k = if cfg!(miri) { 4 << 10 } else { 1 << 10 };
    vm.realm.heap.set_trigger(Trigger { min: k, slack: k });
    if let Err(e) = vm.run_main(0) {
        panic!("uncaught {e:?}\n--- source:\n{src}");
    }
    vm.global_text(name)
}

fn check(cases: &[(&str, &str, &str)]) {
    for (src, name, want) in cases {
        assert_eq!(ev(src, name), *want, "{src}");
        if !cfg!(miri) {
            assert_eq!(ev_gc(src, name), *want, "under collection: {src}");
        }
    }
}

#[test]
fn a_map_keeps_insertion_order_and_the_last_value_of_a_key() {
    check(&[
        ("var m = new Map(); m.set('a', 1).set('b', 2).set('a', 3); var r = m.size + ',' + m.get('a') + ',' + m.get('b') + ',' + m.get('c') + ',' + m.has('b') + ',' + m.has('c');", "r", "2,3,2,undefined,true,false"),
        ("var m = new Map([[1, 'x'], [2, 'y'], [1, 'z']]); var r = m.size + m.get(1) + m.get(2);", "r", "2zy"),
        ("var m = new Map([[1, 1], [2, 2], [3, 3]]); var r = m.delete(2) + ',' + m.delete(2) + ',' + m.size;", "r", "true,false,2"),
        ("var m = new Map(); var r = m.clear() === undefined; m.set(1, 1); m.clear(); r = r + ',' + m.size + ',' + m.has(1); m.set(5, 5); r += ',' + m.size + ',' + m.get(5);", "r", "true,0,false,1,5"),
    ]);
}

#[test]
fn keys_are_same_value_zero() {
    check(&[
        ("var m = new Map(); m.set(NaN, 'n').set(0, 'z'); var r = m.get(NaN) + ',' + m.get(-0) + ',' + m.get(0) + ',' + m.size;", "r", "n,z,z,2"),
        ("var m = new Map(); m.set(-0, 'z'); var k = m.keys().next().value; var r = (1 / k) + ',' + m.has(+0);", "r", "Infinity,true"),
        ("var m = new Map([[1, 'int'], ['1', 'str'], [true, 'b'], [null, 'nul'], [undefined, 'und']]); var r = m.get(1) + m.get('1') + m.get(true) + m.get(null) + m.get(undefined) + m.get(1.0) + m.size;", "r", "intstrbnulundint5"),
        ("var m = new Map(); var a = 'ab', b = 'a'; b += 'b'; m.set(a, 1); var r = m.get(b) + ',' + m.size + ',' + m.has('a' + 'b') + ',' + m.has('ac');", "r", "1,1,true,false"),
        ("var m = new Map(), o1 = {}, o2 = {}, f = function () {}; m.set(o1, 1).set(o2, 2).set(f, 3); var r = m.get(o1) + ',' + m.get(o2) + ',' + m.get(f) + ',' + m.get({}) + ',' + m.size;", "r", "1,2,3,undefined,3"),
        ("var s = Symbol('k'), t = Symbol('k'), m = new Map([[s, 1]]); var r = m.get(s) + ',' + m.get(t) + ',' + m.has(Symbol.iterator);", "r", "1,undefined,false"),
        ("var m = new Map(); m.set(2147483647, 'a').set(2147483648, 'b').set(0.5, 'c').set(-1, 'd'); var r = m.get(2147483647) + m.get(2147483648) + m.get(0.5) + m.get(-1) + m.get(2147483647 + 1);", "r", "abcdb"),
    ]);
}

#[test]
fn a_set_keeps_one_of_each_in_order() {
    check(&[
        ("var s = new Set([3, 1, 3, 2, 1]); var r = s.size + ',' + s.has(3) + ',' + s.has(4) + ',' + s.add(4).size + ',' + s.delete(1) + ',' + s.delete(1) + ',' + s.size;", "r", "3,true,false,4,true,false,3"),
        ("var s = new Set('hello'); var r = s.size; s.clear(); r += ',' + s.size + ',' + s.has('h');", "r", "4,0,false"),
        ("var s = new Set([NaN, NaN, 0, -0]); var r = s.size + ',' + s.has(NaN) + ',' + s.has(0);", "r", "2,true,true"),
        ("var s = new Set(); var o = {}; s.add(o).add(o).add({}); var r = s.size + ',' + s.has(o);", "r", "2,true"),
        ("var s = new Set(); var big = 100000, r = 0; for (var i = 0; i < big; i++) s.add(i * 3); for (var i = 0; i < big; i++) if (s.has(i * 3)) r++; for (var i = 0; i < big; i += 2) s.delete(i * 3); r = r + ',' + s.size + ',' + s.has(3) + ',' + s.has(6);", "r", "100000,50000,true,false"),
        ("var m = new Map(), n = 20000; for (var i = 0; i < n; i++) m.set('k' + i, i); var t = 0; for (var i = 0; i < n; i++) t += m.get('k' + i); for (var i = 0; i < n; i++) m.delete('k' + i); var r = t + ',' + m.size;", "r", "199990000,0"),
    ]);
}

#[test]
fn iterators_follow_the_protocol() {
    check(&[
        ("var m = new Map([[1, 'a'], [2, 'b']]); var it = m.entries(); var a = it.next(), b = it.next(), c = it.next(), d = it.next(); var r = a.value.join('') + b.value.join('') + c.done + c.value + d.done + (it[Symbol.iterator]() === it);", "r", "1a2btrueundefinedtruetrue"),
        ("var m = new Map([[1, 'a'], [2, 'b']]); var k = m.keys(), v = m.values(); var r = k.next().value + v.next().value + k.next().value + v.next().value + k.next().done;", "r", "1a2btrue"),
        ("var s = new Set(['x', 'y']); var it = s.entries(); var r = it.next().value.join('') + it.next().value.join('') + it.next().done + (s[Symbol.iterator] === s.values) + (s.keys === s.values) + (Set.prototype.keys === Set.prototype.values);", "r", "xxyytruetruetruetrue"),
        ("var m = new Map(); var r = (Map.prototype[Symbol.iterator] === Map.prototype.entries) + ',' + (m[Symbol.iterator]().next().done) + ',' + Object.prototype.toString.call(m.keys()) + ',' + Object.prototype.toString.call(new Set().values());", "r", "true,true,[object Map Iterator],[object Set Iterator]"),
        ("var it = new Map().keys(), p = Object.getPrototypeOf(it); var r = (typeof p.next) + (Object.getPrototypeOf(new Set().values()) !== p) + (p.hasOwnProperty('next')); var e = ''; try { p.next.call({}); } catch (x) { e = x.name; } r += e;", "r", "functiontruetrueTypeError"),
    ]);
}

#[test]
fn an_iterator_survives_changes_to_the_collection() {
    check(&[
        // Delete an entry that has not been seen and add one: the new one is seen, the deleted one is not.
        ("var s = new Set([1, 2, 3]); var it = s.values(), out = [], r; while (!(r = it.next()).done) { out.push(r.value); if (r.value === 1) { s.delete(2); s.add(4); } } var r = out.join();", "r", "1,3,4"),
        // The table is copied (grows) between two `next` calls.
        ("var m = new Map(); for (var i = 0; i < 4; i++) m.set(i, i); var it = m.keys(); it.next(); it.next(); for (var i = 4; i < 200; i++) m.set(i, i); var r = it.next().value + ',' + it.next().value;", "r", "2,3"),
        // Entries below the cursor are deleted, then the table is copied: the cursor must not skip.
        ("var m = new Map(); for (var i = 0; i < 6; i++) m.set(i, i); var it = m.keys(); it.next(); it.next(); it.next(); m.delete(0); m.delete(1); for (var i = 6; i < 100; i++) m.set(i, i); var r = it.next().value + ',' + it.next().value;", "r", "3,4"),
        // Several copies in a row (grow, then shrink by deletes).
        ("var m = new Map(); for (var i = 0; i < 64; i++) m.set(i, i); var it = m.keys(); for (var i = 0; i < 10; i++) it.next(); for (var i = 0; i < 60; i++) m.delete(i); var r = it.next().value + ',' + m.size; m.set(100, 1); var seen = []; var q; while (!(q = it.next()).done) seen.push(q.value); r += ',' + seen.join();", "r", "60,4,61,62,63,100"),
        ("var s = new Set([1, 2]); var it = s.values(); it.next(); s.clear(); s.add(9); s.add(10); var r = it.next().value + ',' + it.next().value + ',' + it.next().done;", "r", "9,10,true"),
        // Once done, an iterator stays done.
        ("var s = new Set([1]); var it = s.values(); it.next(); var d1 = it.next().done; s.add(2); var r = d1 + ',' + it.next().done;", "r", "true,true"),
        ("var s = new Set([1]); var r = []; s.forEach(function (v, k, o) { r.push(v + ':' + k + ':' + (o === s)); if (v < 4) s.add(v + 1); }); var r = r.join();", "r", "1:1:true,2:2:true,3:3:true,4:4:true"),
        ("var m = new Map([[1, 'a'], [2, 'b'], [3, 'c']]); var r = []; m.forEach(function (v, k, o) { r.push(k + v); if (k === 1) m.delete(2); }, null); r = r.join();", "r", "1a,3c"),
        ("var m = new Map([[1, 2]]); var t = {}; var r; m.forEach(function (v, k) { r = (this === t) + ',' + v + k; }, t);", "r", "true,21"),
    ]);
}

#[test]
fn the_constructors_take_iterables_and_check_their_input() {
    check(&[
        ("var r = ''; try { Map(); } catch (e) { r += e.name; } try { Set(); } catch (e) { r += e.name; } try { WeakMap(); } catch (e) { r += e.name; } try { WeakSet(); } catch (e) { r += e.name; }", "r", "TypeErrorTypeErrorTypeErrorTypeError"),
        ("var r = ''; try { new Map(5); } catch (e) { r += e.name; } try { new Map([1]); } catch (e) { r += e.name; } try { new Set(5); } catch (e) { r += e.name; } try { new Set({}); } catch (e) { r += e.name; }", "r", "TypeErrorTypeErrorTypeErrorTypeError"),
        ("var r = new Map(null).size + ',' + new Map(undefined).size + ',' + new Set(null).size + ',' + new Set().size + ',' + new Map([]).size;", "r", "0,0,0,0,0"),
        ("var a = new Map([[1, 'a'], [2, 'b']]); var b = new Map(a); b.set(3, 'c'); var s = new Set(a.keys()), t = new Set(new Set([1, 2, 2])); var r = a.size + ',' + b.size + ',' + s.size + ',' + t.size + ',' + new Map(a.entries()).get(2) + ',' + new Set(a).size;", "r", "2,3,2,2,b,2"),
        ("var it = { [Symbol.iterator]() { var i = 0; return { next() { return i < 3 ? { value: i++, done: false } : { done: true, value: undefined }; } }; } }; var r = new Set(it).size + ',' + new Map({ [Symbol.iterator]() { var n = 0; return { next() { return n++ < 2 ? { value: [n, n * 2], done: false } : { done: true }; } }; } }).get(2);", "r", "3,4"),
        // A bad item closes the iterator.
        ("var closed = 0; var it = { [Symbol.iterator]() { return { next() { return { value: 1, done: false }; }, return() { closed++; return {}; } }; } }; var r = ''; try { new Map(it); } catch (e) { r = e.name + closed; }", "r", "TypeError1"),
        ("var calls = []; class S extends Set { add(v) { calls.push(v); return super.add(v); } } var s = new S([1, 2]); var r = calls.join() + ',' + s.size;", "r", "1,2,2"),
        ("class M extends Map { constructor(x) { super(x); this.extra = 7; } sum() { var t = 0; this.forEach(function (v) { t += v; }); return t; } } var m = new M([[1, 10], [2, 20]]); m.set(3, 30); var r = m.sum() + ',' + m.extra + ',' + (m instanceof Map) + ',' + (m instanceof M) + ',' + Object.keys(m).join();", "r", "60,7,true,true,extra"),
    ]);
}

#[test]
fn the_shape_of_the_built_ins_is_the_standard_one() {
    check(&[
        ("var r = [Map.length, Set.length, WeakMap.length, WeakSet.length, Map.name, Set.name, Map.prototype.constructor === Map, Map.prototype[Symbol.toStringTag], WeakSet.prototype[Symbol.toStringTag]].join();", "r", "0,0,0,0,Map,Set,true,Map,WeakSet"),
        ("var d = Object.getOwnPropertyDescriptor(Map.prototype, 'size'); var r = (typeof d.get) + (d.set === undefined) + d.enumerable + d.configurable + (Map[Symbol.species] === Map) + (Set[Symbol.species] === Set);", "r", "functiontruefalsetruetruetrue"),
        ("var m = new Map(); m.x = 1; var r = Object.getOwnPropertyNames(m).join() + '|' + Object.keys(new Set()).length + '|' + JSON.stringify(new Map([[1, 2]])) + '|' + Object.prototype.toString.call(new Map()) + Object.prototype.toString.call(new WeakMap());", "r", "x|0|{}|[object Map][object WeakMap]"),
        ("var r = ''; try { Map.prototype.get.call(new Set(), 1); } catch (e) { r += e.name; } try { Set.prototype.add.call({}, 1); } catch (e) { r += e.name; } try { Object.getOwnPropertyDescriptor(Map.prototype, 'size').get.call(new Set()); } catch (e) { r += e.name; } try { WeakMap.prototype.get.call(new Map(), {}); } catch (e) { r += e.name; } try { new Map().forEach(1); } catch (e) { r += e.name; }", "r", "TypeErrorTypeErrorTypeErrorTypeErrorTypeError"),
        ("var m = new Map(); var f = m.set; var r = ''; try { f(1, 2); } catch (e) { r = e.name; } r += (m.set(1, 2) === m) + ',' + (new Set().add(1) instanceof Set);", "r", "TypeErrortrue,true"),
        ("var r = [Map.prototype.get.name, Map.prototype.get.length, Map.prototype.set.length, Set.prototype.forEach.length, Map.prototype.entries.name, Set.prototype.values.name, Map.prototype.delete.name].join();", "r", "get,1,2,1,entries,values,delete"),
    ]);
}

#[test]
fn a_weak_map_and_a_weak_set_hold_objects_only() {
    check(&[
        ("var wm = new WeakMap(), k = {}, f = function () {}; wm.set(k, 1).set(f, 2); var r = wm.get(k) + ',' + wm.get(f) + ',' + wm.get({}) + ',' + wm.has(k) + ',' + wm.has({}) + ',' + wm.delete(k) + ',' + wm.delete(k) + ',' + wm.has(k) + ',' + wm.get(k);", "r", "1,2,undefined,true,false,true,false,false,undefined"),
        ("var wm = new WeakMap(), r = ''; try { wm.set(1, 1); } catch (e) { r += e.name; } try { wm.set('s', 1); } catch (e) { r += e.name; } try { wm.set(undefined, 1); } catch (e) { r += e.name; } r += wm.get(1) + ',' + wm.has('s') + ',' + wm.delete(null);", "r", "TypeErrorTypeErrorTypeErrorundefined,false,false"),
        ("var ws = new WeakSet(), o = {}; ws.add(o).add(o); var r = ws.has(o) + ',' + ws.has({}) + ',' + ws.delete(o) + ',' + ws.delete(o) + ',' + ws.has(o) + ',' + ws.has(1); try { ws.add(1); } catch (e) { r += e.name; }", "r", "true,false,true,false,false,falseTypeError"),
        ("var k1 = {}, k2 = {}; var wm = new WeakMap([[k1, 'a'], [k2, 'b']]); var ws = new WeakSet([k1, k2]); var r = wm.get(k2) + ',' + ws.has(k1) + ',' + Object.prototype.toString.call(wm);", "r", "b,true,[object WeakMap]"),
        ("var wm = new WeakMap(), ks = []; for (var i = 0; i < 5000; i++) { ks.push({}); wm.set(ks[i], i); } var t = 0; for (var i = 0; i < 5000; i++) t += wm.get(ks[i]); for (var i = 0; i < 5000; i += 2) wm.delete(ks[i]); var r = t + ',' + wm.has(ks[0]) + ',' + wm.has(ks[1]) + ',' + wm.get(ks[4999]);", "r", "12497500,false,true,4999"),
        ("var wm = new WeakMap(), k = {}; wm.set(k, 1); wm.set(k, 2); var r = wm.get(k); class W extends WeakSet { } var w = new W([k]); r += ',' + w.has(k) + ',' + (w instanceof WeakSet);", "r", "2,true,true"),
    ]);
}

/// Run `src` and collect once; the counters of that collection.
fn collect_after(src: &str) -> crate::gc::Stats {
    let mut vm = run(src).unwrap_or_else(|e| panic!("{e}\n--- source:\n{src}"));
    vm.collect_idle();
    vm.realm.last_gc
}

#[test]
fn a_dead_key_drops_its_entry() {
    let st = collect_after("var wm = new WeakMap(); var keep = {}; wm.set(keep, 'kept'); (function () { for (var i = 0; i < 50; i++) wm.set({}, { n: i }); })();");
    assert_eq!(st.cleared, 50, "{st:?}");
    // Nothing is dropped while the keys are alive.
    let st = collect_after("var wm = new WeakMap(), ks = []; for (var i = 0; i < 50; i++) { ks[i] = {}; wm.set(ks[i], { n: i }); }");
    assert_eq!(st.cleared, 0, "{st:?}");
}

#[test]
fn a_chain_and_a_cycle_through_weak_maps_follow_their_keys() {
    // k1 -> k2 -> k3 -> v: the values wait for their keys and are released in any marking order, so nothing is dropped while k1 lives.
    let src = "var a = new WeakMap(), b = new WeakMap(), c = new WeakMap(); var k1 = {}; (function () { var k2 = {}, k3 = {}; a.set(k1, k2); b.set(k2, k3); c.set(k3, { alive: 1 }); })();";
    let st = collect_after(src);
    assert_eq!(st.cleared, 0, "{st:?}");
    assert!(st.released >= 2, "the chain was released through its keys: {st:?}");
    // Drop k1 and the three entries go.
    let st = collect_after(&format!("{src} k1 = null;"));
    assert_eq!(st.cleared, 3, "{st:?}");
    // A key that is its own value, and keys that name each other, with nothing else naming them: all dead.
    let st = collect_after("var wm = new WeakMap(); (function () { var a = {}, b = {}; wm.set(a, a); wm.set(b, { other: a }); wm.set(a, { back: b }); })();");
    assert_eq!(st.cleared, 2, "{st:?}");
}

#[test]
fn a_weak_set_drops_dead_members_and_goes_on_working() {
    let mk = "var ws = new WeakSet(), keep = []; for (var i = 0; i < 300; i++) { var o = {}; ws.add(o); if (i % 3 === 0) keep.push(o); } o = null;";
    let st = collect_after(mk);
    assert_eq!(st.cleared, 200, "{st:?}");
    // After the churn the dead slots are tombstones: lookups and new members still work, and a rehash drops them.
    check(&[(&format!("{mk} var n = 0; for (var i = 0; i < 3000; i++) ws.add({{}}); for (var i = 0; i < keep.length; i++) if (ws.has(keep[i])) n++; var r = n + ',' + keep.length;"), "r", "100,100")]);
}

#[test]
fn weak_maps_work_under_constant_collection() {
    check(&[
        ("var wm = new WeakMap(), keep = []; for (var i = 0; i < 400; i++) { var k = {}; wm.set(k, { v: i, s: 'v' + i }); if (i % 4 === 0) keep.push(k); } var t = 0; for (var i = 0; i < keep.length; i++) t += wm.get(keep[i]).v; var r = t + ',' + wm.get(keep[99]).s;", "r", "19800,v396"),
        ("var a = new WeakMap(), b = new WeakMap(), root = {}, cur = root; for (var i = 0; i < 100; i++) { var next = {}; a.set(cur, next); b.set(next, [i]); cur = next; } var n = 0, p = root; while (a.has(p)) { p = a.get(p); n += b.get(p)[0]; } var r = n;", "r", "4950"),
        // A value that only a live key reaches survives the churn.
        ("var wm = new WeakMap(), k = {}; (function () { wm.set(k, { deep: { x: 42 } }); })(); var junk; for (var i = 0; i < 3000; i++) junk = { a: i, b: [i] }; var r = wm.get(k).deep.x;", "r", "42"),
    ]);
}
