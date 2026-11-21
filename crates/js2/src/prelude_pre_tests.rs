// Tests of the flat prelude (`prelude_pre.rs`, `prelude.js`): a public name makes its function at the first lookup, a helper or a native
// that the functions read is made at its first read, and the results are those of the wrapper that this replaced. Each case runs
// normally and under constant collection.
use crate::heap::Trigger;
use crate::script::{load, run};

fn ev(src: &str) -> String {
    match run(src) {
        Ok(mut vm) => vm.global_text("r"),
        Err(e) => panic!("{e}\n--- source:\n{src}"),
    }
}

fn ev_gc(src: &str) -> String {
    let mut vm = load(src).unwrap_or_else(|e| panic!("{e}\n--- source:\n{src}"));
    let k = if cfg!(miri) { 4 << 10 } else { 1 << 10 };
    vm.realm.heap.set_trigger(Trigger { min: k, slack: k });
    if let Err(e) = vm.run_main(0) {
        panic!("uncaught {e:?}\n--- source:\n{src}");
    }
    vm.global_text("r")
}

fn check(cases: &[(&str, &str)]) {
    for (src, want) in cases {
        assert_eq!(ev(src), *want, "{src}");
        assert_eq!(ev_gc(src), *want, "under collection: {src}");
    }
}

#[test]
fn every_public_prelude_function_has_the_name_length_and_attributes_it_had() {
    let src = "
var want = {
  'Array.prototype': 'keys:0 values:0 entries:0 forEach:1 map:1 filter:1 reduce:1 reduceRight:1 some:1 every:1 find:1 findIndex:1 findLast:1 findLastIndex:1 flat:0 flatMap:1 sort:1 toSorted:1 toLocaleString:0',
  'Array': 'of:0 from:1',
  'String.prototype': 'replace:2 replaceAll:2'
};
var bad = [];
for (var path in want) {
  var o = path === 'Array' ? Array : path === 'String.prototype' ? String.prototype : Array.prototype;
  want[path].split(' ').forEach(function (e) {
    var n = e.split(':')[0], len = +e.split(':')[1], d = Object.getOwnPropertyDescriptor(o, n), f = d && d.value;
    var dn = Object.getOwnPropertyDescriptor(f, 'name'), dl = Object.getOwnPropertyDescriptor(f, 'length');
    if (typeof f !== 'function' || f.name !== n || f.length !== len || d.enumerable || !d.writable || !d.configurable
        || dn.writable || dn.enumerable || !dn.configurable || dl.writable || dl.enumerable || !dl.configurable) bad.push(path + '.' + n);
  });
}
var r = bad.join() + '|' + (Array.prototype[Symbol.iterator] === Array.prototype.values);";
    assert_eq!(ev(src), "|true");
    assert_eq!(ev_gc(src), "|true");
}

#[test]
fn the_prelude_adds_no_global_and_no_property_before_a_lookup() {
    check(&[
        ("var r = typeof __def + typeof __helper + typeof __set + Object.keys(globalThis).join();", "undefinedundefinedundefinedr"),
        ("var r = Object.hasOwn(Array.prototype, 'forEach') + ',' + Object.hasOwn(Object.prototype, '__proto__') + ',' + Object.hasOwn(String.prototype, 'replace');", "true,true,true"),
    ]);
}

#[test]
fn the_helpers_that_natives_call_give_their_results() {
    check(&[
        ("var o = Object.assign({}, { get a() { return 1; } }, { b: 2 }); var r = o.a + ',' + o.b;", "1,2"),
        ("var o = {}; Object.defineProperty(o, 'x', { get: function () { return 7; }, configurable: true }); var r = o.x + ',' + Object.keys(o).length;", "7,0"),
        ("var o = Object.defineProperties({}, { a: { get: function () { return 1; } }, b: { value: 2 } }); var r = o.a + ',' + o.b;", "1,2"),
        ("var s = { get g() { return 3; }, h: 4 }; var o = { ...s }; var r = o.g + ',' + o.h;", "3,4"),
        ("var { g, ...rest } = { get g() { return 3; }, h: 4, i: 5 }; var r = g + ',' + Object.keys(rest).join('');", "3,hi"),
        ("var r = Object.values({ get q() { return 5; }, z: 6 }).join() + ';' + Object.entries({ get q() { return 5; } }).join();", "5,6;q,5"),
        ("var a = { length: 3, 0: 'x', get 1() { return 'y'; }, 2: 'x' }; var r = [Array.prototype.indexOf.call(a, 'y'), Array.prototype.lastIndexOf.call(a, 'x'), Array.prototype.includes.call(a, 'y')].join();", "1,2,true"),
    ]);
}

#[test]
fn a_script_that_replaces_a_native_does_not_change_the_helpers() {
    check(&[
        ("Object.keys = function () { throw new Error('no'); }; Object.defineProperty = function () { throw new Error('no'); }; var r = Object.values({ get q() { return 5; } }).join() + Object.assign({}, { get a() { return 1; } }).a;", "51"),
    ]);
}

#[test]
fn proto_is_an_accessor_with_the_names_and_the_lengths_it_had() {
    check(&[
        ("var d = Object.getOwnPropertyDescriptor(Object.prototype, '__proto__'); var r = [typeof d.get, typeof d.set, d.get.length, d.set.length, d.enumerable, d.configurable, 'value' in d].join();", "function,function,0,1,false,true,false"),
        ("var o = {}; var r = (o.__proto__ === Object.prototype) + ',' + ((5).__proto__ === Number.prototype) + ',' + (Object.create(null).__proto__);", "true,true,undefined"),
        ("var p = { k: 1 }, o = {}; o.__proto__ = p; var r = o.k + ',' + (Object.getPrototypeOf(o) === p);", "1,true"),
        ("var o = {}; o.__proto__ = 5; o.__proto__ = undefined; var r = Object.getPrototypeOf(o) === Object.prototype;", "true"),
        ("var o = {}; o.__proto__ = null; var r = Object.getPrototypeOf(o) === null;", "true"),
        ("var d = Object.getOwnPropertyDescriptor(Object.prototype, '__proto__'); d.set.call(7, {}); var r = 'ok';", "ok"),
        ("var d = Object.getOwnPropertyDescriptor(Object.prototype, '__proto__'); var r = ''; try { d.get.call(undefined); } catch (e) { r += e.name; } try { d.set.call(null, {}); } catch (e) { r += e.name; }", "TypeErrorTypeError"),
        ("var a = {}, b = Object.create(a), r = ''; try { a.__proto__ = b; } catch (e) { r = e.name; }", "TypeError"),
    ]);
}

#[test]
fn the_array_iterator_has_its_prototype_tag_and_next() {
    check(&[
        ("var it = [1, 2].keys(), p = Object.getPrototypeOf(it); var r = [Object.getOwnPropertyNames(p).join(), p[Symbol.toStringTag], p.next.name, p.next.length, Object.getPrototypeOf(p) === Object.getPrototypeOf(Object.getPrototypeOf(''[Symbol.iterator]()))].join();", "next,Array Iterator,next,0,true"),
        ("var r = [...['a', 'b'].entries()].join(';') + '|' + [...['a', 'b'].keys()].join() + '|' + [...['a', 'b'].values()].join();", "0,a;1,b|0,1|a,b"),
        ("var p = Object.getPrototypeOf([].keys()); var r = ''; try { p.next.call({}); } catch (e) { r = e.name; }", "TypeError"),
        ("var a = [1].values(), b = [2].values(); var r = (Object.getPrototypeOf(a) === Object.getPrototypeOf(b)) + ',' + JSON.stringify(a.next()) + JSON.stringify(a.next());", "true,{\"value\":1,\"done\":false}{\"done\":true}"),
    ]);
}

#[test]
fn a_string_iterator_that_a_script_replaced_does_not_break_the_array_iterator() {
    check(&[("String.prototype[Symbol.iterator] = function () { throw new Error('no'); }; var r = [...[1, 2].values()].join();", "1,2")]);
}

#[test]
fn the_functions_of_the_prelude_are_the_same_object_at_each_lookup() {
    check(&[
        ("var r = (Array.prototype.map === Array.prototype.map) + ',' + (Array.prototype[Symbol.iterator] === Array.prototype.values) + ',' + (Object.getOwnPropertyDescriptor(Array.prototype, 'forEach').value === [].forEach);", "true,true,true"),
        ("delete Array.prototype.values; var r = typeof Array.prototype.values + ',' + typeof [][Symbol.iterator];", "undefined,function"),
    ]);
}

#[test]
fn the_calls_of_a_prelude_function_work_before_and_after_its_first_compile() {
    check(&[
        ("var r = ''; for (var i = 0; i < 3; i++) r += [3, 1, 2].map(function (x) { return x + i; }).sort().join('') + ';';", "123;234;345;"),
        ("var r = [1, [2, [3, [4]]]].flat(Infinity).join() + ';' + [1, 2].flatMap(function (x) { return [x, x]; }).join() + ';' + [5, 1, 4].toSorted().join();", "1,2,3,4;1,1,2,2;1,4,5"),
        ("var r = Array.of(1, 2).join() + ';' + Array.from({ length: 2 }, function (x, i) { return i; }).join() + ';' + 'abab'.replaceAll('a', function () { return 'c'; });", "1,2;0,1;cbcb"),
    ]);
}

#[test]
fn the_first_slots_are_the_helpers_in_the_order_of_their_constants() {
    use crate::bi_object::{H_ASSIGN, H_DEFINE, H_DEFINE_ALL, H_PAIRS, H_REST, H_SEARCH, H_SPREAD};
    use crate::prelude_pre::PRE;
    for (h, name) in [(H_DEFINE, "helperDefineProperty"), (H_DEFINE_ALL, "helperDefineProperties"), (H_ASSIGN, "helperAssign"), (H_SEARCH, "helperSearch"), (H_SPREAD, "helperSpread"), (H_REST, "helperRest"), (H_PAIRS, "helperPairs")] {
        assert_eq!(PRE[h].0, name);
    }
}
