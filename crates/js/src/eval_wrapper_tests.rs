//! Primitive wrapper object tests (`new String`, `new Number`, `new Boolean`, `Object(prim)`). What Node can show is in
//! `tools/fixtures/js/wrapper-cases.txt` (`tools/js/wrapper_check.sh`). The tests below cover what Node cannot show: GC,
//! the heap and step limits, and the cost of the paths that must not change (primitive method calls, plain objects).

use super::intl_tests::{big, out, realm};
use super::*;

#[test]
fn new_string_number_boolean_make_wrapper_objects() {
    big(|| {
        let mut it = realm();
        assert_eq!(out(&mut it, "typeof new String('a') + typeof new Number(1) + typeof new Boolean(false)"), "string:objectobjectobject");
        assert_eq!(out(&mut it, "new String('abc').length + new Number(5).valueOf() + (new Boolean(false) ? 1 : 0)"), "number:9");
        assert_eq!(out(&mut it, "new Number(1) == 1 && new Number(1) !== 1 && new String('a') == 'a' && new Boolean(false) == false"), "boolean:true");
        assert_eq!(out(&mut it, "Object(Symbol('d')).description"), "string:d");
    });
}

#[test]
fn primitive_method_calls_and_plain_objects_allocate_no_wrapper() {
    big(|| {
        let mut it = realm();
        out(&mut it, "var s = 'abc', n = 1.5, b = true, y = Symbol('q'), o = {}, acc = 0;");
        let before = (it.heap.stats().total_object_allocs, it.heap.has_exotics());
        out(&mut it, "for (var i = 0; i < 100; i++) { acc += s.length + s[1].length + n.toFixed(1).length + s.charAt(1).length + s.toUpperCase().length + n.toString().length + b.toString().length + y.toString().length + y.description.length + (n.valueOf() | 0) + o.toString().length; } acc");
        let after = (it.heap.stats().total_object_allocs, it.heap.has_exotics());
        assert_eq!(before, after, "no object was allocated for a primitive receiver");
        // a method that is a native keeps the primitive `this`: no wrapper
        assert_eq!(out(&mut it, "var r = Number.prototype.valueOf.call(5) + String.prototype.valueOf.call('x'); r"), "string:5x");
        assert_eq!(it.heap.stats().total_object_allocs, after.0 + 0, "no object for a native called on a primitive");
        // a strict function keeps the primitive `this`
        out(&mut it, "function strict() { 'use strict'; return this; } function sloppy() { return this; } strict.call(0); sloppy.call({});");
        let base = it.heap.stats().total_object_allocs;
        out(&mut it, "for (var i = 0; i < 50; i++) strict.call(i);");
        assert_eq!(it.heap.stats().total_object_allocs, base, "strict `this` is not boxed");
        // a sloppy function boxes: one wrapper for each call
        out(&mut it, "for (var i = 0; i < 50; i++) sloppy.call(i);");
        assert_eq!(it.heap.stats().total_object_allocs, base + 50);
    });
}

#[test]
fn dead_wrappers_are_collected_and_live_ones_keep_their_primitive() {
    big(|| {
        let mut it = realm();
        out(&mut it, "var keepS = new String('kept' + 'string'), keepY = Object(Symbol('kept symbol')), keepN = new Number(42);");
        out(&mut it, "for (var i = 0; i < 500; i++) { new String('dead' + i); new Number(i); new Boolean(i & 1); Object(Symbol('d' + i)); } 0;");
        let busy = it.heap.stats();
        it.collect(&[]).unwrap();
        let after = it.heap.stats();
        assert!(after.live_objects + 2000 <= busy.live_objects + 3, "{} -> {}", busy.live_objects, after.live_objects);
        assert!(after.live_symbols < busy.live_symbols);
        assert!(after.live_exotics <= 3 + 1, "only the three kept wrappers stay (and no other exotic): {}", after.live_exotics);
        // the kept string, symbol and number are still there after a collect that reached them only through the wrappers
        assert_eq!(out(&mut it, "keepS.valueOf() + keepS.length + keepY.description + keepY.valueOf().toString() + keepN"), "string:keptstring10kept symbolSymbol(kept symbol)42");
        out(&mut it, "keepS = keepY = keepN = null;");
        it.collect(&[]).unwrap();
        assert_eq!(it.heap.stats().live_exotics, 0);
    });
}

#[test]
fn a_wrapper_costs_one_object_and_one_table_entry() {
    big(|| {
        let mut it = realm();
        let before = it.heap.stats();
        out(&mut it, "var keep = []; for (var i = 0; i < 1000; i++) keep.push(new Number(i));");
        let after = it.heap.stats();
        // 1000 wrappers + the array. A wrapper has no string, no property and no element vector.
        assert_eq!(after.live_objects - before.live_objects, 1001);
        assert_eq!(after.live_exotics, 1000);
        assert!(after.live_strings <= before.live_strings + 3, "no string for a Number wrapper: {} -> {}", before.live_strings, after.live_strings);
        let per = (after.bytes - before.bytes) / 1000;
        assert!(per <= 120, "accounted bytes for each wrapper: {per}");
    });
}

#[test]
fn the_heap_cap_stops_wrapper_creation_and_cannot_be_caught() {
    big(|| {
        let intrinsic = Interp::realm(Heap::default(), EvalLimits::default()).unwrap().heap.stats().live_objects;
        let heap = Heap::new(HeapLimits { max_objects: intrinsic + 30, ..Default::default() }).unwrap();
        let mut it = Interp::realm(heap, EvalLimits::default()).unwrap();
        let r = it.run_script("var keep = []; try { for (;;) keep.push(new String('x')); } catch (e) { 'caught' }");
        assert!(matches!(r, Err(Error::Limit(LimitKind::Heap(Limit::Objects)))), "{r:?}");
    });
}

#[test]
fn a_string_wrapper_key_list_is_charged_before_it_is_built() {
    big(|| {
        let mut it = realm();
        out(&mut it, "var big = new String('x'.repeat(100000));");
        it.limits.max_steps = 5000;
        assert_eq!(out(&mut it, "Object.keys(big).length"), "limit:Steps");
    });
}

#[test]
fn subclass_of_a_wrapper_keeps_its_prototype_and_the_rules() {
    big(|| {
        let mut it = realm();
        out(&mut it, "class N extends Number { get dbl() { return this * 2; } } var n = new N(4);");
        assert_eq!(out(&mut it, "n.dbl + ':' + (n instanceof N) + ':' + typeof n + ':' + Object.prototype.toString.call(n)"), "string:8:true:object:[object Number]");
        out(&mut it, "class S extends String {} var s = new S('xy');");
        assert_eq!(out(&mut it, "s.length + Object.keys(s).join() + (s instanceof S) + JSON.stringify(s)"), "string:20,1true\"xy\"");
    });
}

#[test]
fn json_stringify_of_a_wrapper_calls_the_conversion_once_and_in_order() {
    big(|| {
        let mut it = realm();
        out(&mut it, "var log = []; var n = new Number(1); n.valueOf = function () { log.push('v'); return 7; }; n.toString = function () { log.push('t'); return 'x'; };");
        assert_eq!(out(&mut it, "JSON.stringify([n, n]) + log.join()"), "string:[7,7]v,v");
    });
}
