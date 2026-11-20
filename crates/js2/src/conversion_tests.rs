// Conversions that call script code (`reenter.rs`) and wrapper objects (`bi_wrap.rs`). Each case runs normally and under constant
// collection, as in `script_tests.rs`. A conversion that runs script code holds the collector (see `reenter.rs`), so the second run
// also covers the collection that follows it. Under Miri a group runs only its first cases (each run compiles the prelude too).
use crate::heap::Trigger;
use crate::interp::Exit;
use crate::script::{load, run};

const MIRI: bool = cfg!(miri);

fn ev(src: &str, name: &str) -> String {
    match run(src) {
        Ok(mut vm) => vm.global_text(name),
        Err(e) => panic!("{e}\n--- source:\n{src}"),
    }
}

fn ev_gc(src: &str, name: &str) -> String {
    let mut vm = load(src).unwrap_or_else(|e| panic!("{e}\n--- source:\n{src}"));
    let k = if MIRI { 4 << 10 } else { 1 << 10 };
    vm.realm.heap.set_trigger(Trigger { min: k, slack: k });
    if let Err(e) = vm.run_main(0) {
        panic!("uncaught {e:?}\n--- source:\n{src}");
    }
    vm.global_text(name)
}

/// `(source, global, expected text)`: normally and under constant collection.
fn check(cases: &[(&str, &str, &str)]) {
    for (src, name, want) in if MIRI { &cases[..cases.len().min(3)] } else { cases } {
        assert_eq!(ev(src, name), *want, "{src}");
        if !MIRI {
            assert_eq!(ev_gc(src, name), *want, "under collection: {src}");
        }
    }
}

#[test]
fn a_script_value_of_and_to_string_run_in_every_conversion() {
    check(&[
        ("var o = {valueOf: function () { return 41; }}; var r = (o + 1) + ',' + (o - 1) + ',' + (o * 2) + ',' + (o < 50) + ',' + (o == 41) + ',' + -o + ',' + (o | 0) + ',' + Number(o) + ',' + Math.max(o, 3);", "r", "42,40,82,true,true,-41,41,41,41"),
        ("var s = {toString: function () { return 'str'; }}; var r = (s + '!') + ',' + String(s) + ',' + `${s}` + ',' + [s, s].join('-') + ',' + ('str' == s) + ',' + ('a' + s);", "r", "str!,str,str,str-str,true,astr"),
        // `valueOf` wins for the hint default and number, `toString` for the hint string.
        ("var o = {valueOf: function () { return 'v'; }, toString: function () { return 't'; }}; var r = (o + '') + ',' + String(o) + ',' + `${o}` + ',' + (o == 'v');", "r", "v,t,t,true"),
        // A `valueOf` that returns an object is skipped; when both give objects, the conversion throws a TypeError.
        ("var o = {valueOf: function () { return {}; }, toString: function () { return 'ts'; }}; var r = o + '|';", "r", "ts|"),
        ("var o = {valueOf: function () { return {}; }, toString: function () { return {}; }}; try { o + 1; } catch (e) { var r = e.name; }", "r", "TypeError"),
        // The property key of `o[k]` is the string of the primitive of `k`.
        ("var k = {toString: function () { return 'key'; }}; var o = {}; o[k] = 5; o[{valueOf: function () { return 7; }}] = 'v'; var r = o.key + ',' + o[k] + ',' + Object.keys(o).join('/') + ',' + (k in o) + ',' + o.hasOwnProperty(k);", "r", "5,5,key/[object Object],true,true"),
        // Arguments of built-ins: `substring`, `slice`, `indexOf` and the like convert them in order.
        ("var n = {valueOf: function () { return 2; }}; var r = 'abcdef'.substring(n, 4) + ',' + 'abcdef'.slice(n) + ',' + [1, 2, 3, 4].indexOf(3, n) + ',' + 'x'.repeat(n) + ',' + (1.2345).toFixed(n) + ',' + parseInt({toString: function () { return '12px'; }});", "r", "cd,cdef,2,xx,1.23,12"),
        ("var log = ''; var a = {valueOf: function () { log += 'a'; return 1; }}, b = {valueOf: function () { log += 'b'; return 2; }}; var r = (a + b) + log + (b < a) + log;", "r", "3abfalseabba"),
        ("var o = {valueOf: function () { return this.n; }, n: 9}; var r = o + o;", "r", "18"),
        // An accessor can give the method.
        ("var o = {get valueOf() { return function () { return 7; }; }}; var r = o * 2;", "r", "14"),
        // A bound function and a native as `valueOf`.
        ("var o = {valueOf: (function () { return this.k; }).bind({k: 5})}; var r = o + 1;", "r", "6"),
        ("var o = {valueOf: Math.random.bind(Math)}; var r = typeof (o + 1);", "r", "number"),
        ("var d = new Number(3); var r = d + 1;", "r", "4"),
        // The array method `join`, a native, converts each element with a script `toString`.
        ("var r = [{toString: function () { return 'a'; }}, [{toString: function () { return 'b'; }}], null, 1].join('+');", "r", "a+b++1"),
    ]);
}

#[test]
fn a_throw_from_a_conversion_is_catchable_and_ends_the_operation() {
    check(&[
        ("var log = ''; var t = {valueOf: function () { throw new RangeError('boom'); }}, u = {valueOf: function () { log += 'u'; return 1; }}; try { t + u; } catch (e) { var r = e.name + ':' + e.message + ':' + log; }", "r", "RangeError:boom:"),
        ("var t = {toString: function () { throw 'plain'; }}; try { String(t); } catch (e) { var r = e; }", "r", "plain"),
        // The exception leaves the nested run, and the next conversion works.
        ("var t = {valueOf: function () { throw 1; }}; var n = 0; for (var i = 0; i < 5; i++) { try { t + 1; } catch (e) { n += e; } } var r = n + ({valueOf: function () { return 1; }} + 1);", "r", "7"),
        // A conversion in the middle of a built-in: the error comes out of the built-in.
        ("var t = {valueOf: function () { throw new TypeError('x'); }}; try { 'abc'.substring(t); } catch (e) { var r = e.name; }", "r", "TypeError"),
        // A `try` inside the `valueOf` catches its own errors.
        ("var o = {valueOf: function () { try { null.x; } catch (e) { return 5; } }}; var r = o + 1;", "r", "6"),
        // `finally` of the code that was converting runs as usual.
        ("var log = ''; function f() { try { return {valueOf: function () { throw 1; }} + 1; } finally { log += 'f'; } } try { f(); } catch (e) { log += e; } var r = log;", "r", "f1"),
    ]);
}

#[test]
fn a_conversion_that_calls_itself_ends_with_a_range_error() {
    // The nested calls stop at a fixed depth. The frames of the interpreter are big in a debug build, so the test has its own stack.
    std::thread::Builder::new()
        .stack_size(256 << 20)
        .spawn(|| {
            check(&[
                ("var o = {valueOf: function () { return this + 1; }}; try { o + 1; } catch (e) { var r = e instanceof RangeError; }", "r", "true"),
                ("var o = {toString: function () { return '' + this; }}; try { String(o); } catch (e) { var r = e instanceof RangeError; }", "r", "true"),
                // A tree of nodes whose `toString` joins the children: ordinary nesting, 30 levels.
                ("function N(d) { this.kids = d ? [new N(d - 1)] : []; this.d = d; } N.prototype.toString = function () { return '(' + this.kids.join(',') + ')'; }; var r = String(new N(30)).length;", "r", "62"),
            ]);
        })
        .expect("thread")
        .join()
        .expect("the checks pass");
}

#[test]
fn the_budget_stops_a_nested_call_and_no_handler_runs() {
    let src = "function probe(x) { return x + 1; } var caught = false; var o = {valueOf: function () { for (;;) {} }}; try { o + 1; } catch (e) { caught = true; } finally { caught = 'finally'; }";
    let mut vm = load(src).expect("loads");
    vm.fuel = 10_000;
    assert_eq!(vm.run_main(0), Err(Exit::Fuel));
    assert_eq!(vm.global_text("caught"), "false");
    // The realm is usable afterwards, with a new budget.
    vm.fuel = 1_000;
    let f = vm.global("probe");
    let r = vm.call(f, crate::value::Value::UNDEFINED, &[crate::value::Value::int(3)]);
    assert_eq!(r.map(|v| vm.display(v, false)), Ok("4".to_string()));
    // The budget that a nested call spends is the budget of the caller.
    let src = "var n = 0; var o = {valueOf: function () { for (var i = 0; i < 400; i++) n++; return 1; }}; for (var k = 0; k < 100; k++) o + 1; var done = n;";
    let mut vm = load(src).expect("loads");
    vm.fuel = 1_000_000;
    assert!(vm.run_main(0).is_ok());
    assert_eq!(vm.global_text("done"), "40000");
    assert!(vm.fuel <= 1_000_000 - 40_000, "the nested back edges are counted: {}", vm.fuel);
}

#[test]
fn a_nested_call_that_allocates_keeps_the_values_of_the_native() {
    // The native holds the converted strings while the next `valueOf` allocates a lot (the collector is held); then the heap is
    // collected at the next op that allocates. Under constant collection every op collects.
    check(&[
        (
            "var big = {valueOf: function () { var t = []; for (var i = 0; i < 3000; i++) t.push({i: i, s: 'k' + i}); return t.length; }}; \
             var r = ('a' + big + 'b' + big + 'c') + [big, big].join('|') + Math.max(big, big);",
            "r",
            "a3000b3000c[object Object]|[object Object]3000",
        ),
        (
            "var calls = 0; var o = {toString: function () { calls++; return 'x'.repeat(100) + calls; }}; var a = []; for (var i = 0; i < 50; i++) a.push(String(o)); var r = a[0].length + ',' + a[49].length + ',' + calls;",
            "r",
            "101,102,50",
        ),
    ]);
}

#[test]
fn getters_and_setters_run_when_a_built_in_reads_or_writes_a_property() {
    check(&[
        ("var src = {get a() { return 1; }, b: 2}; var t = Object.assign({}, src); var r = t.a + ',' + t.b + ',' + Object.values(src).join();", "r", "1,2,1,2"),
        ("var log = ''; var t = {set a(v) { log += 'set' + v; }}; Object.assign(t, {a: 5}); var r = log;", "r", "set5"),
    ]);
}

#[test]
fn wrapper_objects() {
    check(&[
        ("var s = new String('abc'), n = new Number(5), b = new Boolean(false); var r = typeof s + typeof n + typeof b + ',' + (s instanceof String) + (n instanceof Number) + (b instanceof Boolean) + ',' + (s == 'abc') + (n == 5) + (b == false) + ',' + (s === 'abc');", "r", "objectobjectobject,truetruetrue,truetruetrue,false"),
        ("var r = (new Number(5) + 1) + ',' + (new String('a') + 'b') + ',' + (new Boolean(true) + 1) + ',' + (new Number(2) * new Number(3)) + ',' + (new Number(2) < new Number(3));", "r", "6,ab,2,6,true"),
        // A `Boolean` wrapper is truthy, whatever it holds.
        ("var r = (new Boolean(false) ? 'T' : 'F') + (!new Number(0)) + (!new String(''));", "r", "Tfalsefalse"),
        ("var s = new String('héllo'); var r = s.length + ',' + s[1] + ',' + s[9] + ',' + s.toUpperCase() + ',' + s.indexOf('l') + ',' + s.charAt(0) + ',' + s.valueOf() + ',' + typeof s.valueOf() + ',' + s.toString();", "r", "5,é,undefined,HÉLLO,2,h,héllo,string,héllo"),
        ("var n = new Number(255); var r = n.toString(16) + ',' + n.toFixed(1) + ',' + n.valueOf() + ',' + typeof n.valueOf() + ',' + new Number() + ',' + new Number('7') + ',' + new Number(undefined);", "r", "ff,255.0,255,number,0,7,NaN"),
        ("var b = new Boolean(true); var r = b.toString() + ',' + b.valueOf() + ',' + new Boolean() + ',' + new Boolean('') + ',' + new Boolean('x');", "r", "true,true,false,false,true"),
        // The conversion functions called as functions give primitives, and `Object` boxes.
        ("var r = typeof String(1) + typeof Number('1') + typeof Boolean(1) + ',' + typeof Object(1) + typeof Object('s') + typeof Object(true) + ',' + (Object(null) instanceof Object) + ',' + (Object(undefined) instanceof Object);", "r", "stringnumberboolean,objectobjectobject,true,true"),
        ("var s = new String('x'); var r = (Object(s) === s) + ',' + (new Object(s) === s) + ',' + (new Object(1) instanceof Number) + ',' + Object.prototype.toString.call(new Number(1)) + Object.prototype.toString.call(new Boolean(1)) + Object.prototype.toString.call(s);", "r", "true,true,true,[object Number][object Boolean][object String]"),
        // Own properties: the units and `length` of a `String` wrapper, and the properties that a script adds.
        ("var s = new String('ab'); s.x = 1; var r = Object.keys(s).join() + '|' + Object.getOwnPropertyNames(s).join() + '|' + s.hasOwnProperty('1') + s.hasOwnProperty('2') + s.hasOwnProperty('length') + s.propertyIsEnumerable('0') + s.propertyIsEnumerable('length');", "r", "0,1,x|0,1,length,x|truefalsetruetruefalse"),
        ("var s = new String('ab'); var d = Object.getOwnPropertyDescriptor(s, '1'); var l = Object.getOwnPropertyDescriptor(s, 'length'); var r = d.value + d.enumerable + d.writable + ',' + l.value + l.enumerable + l.writable;", "r", "btruefalse,2falsefalse"),
        ("var w = new Number(1); w.a = 2; w.b = 3; delete w.a; var r = w.b + ',' + w.a + ',' + (w + 1) + ',' + Object.keys(w).join();", "r", "3,undefined,2,b"),
        // A class that extends a wrapper type.
        ("class N2 extends Number { twice() { return this * 2; } } var n = new N2(21); var r = n.twice() + ',' + (n instanceof N2) + (n instanceof Number) + ',' + typeof n + ',' + n.toFixed(1);", "r", "42,truetrue,object,21.0"),
        ("class S2 extends String { shout() { return this.toUpperCase() + '!'; } } var s = new S2('hi'); var r = s.shout() + ',' + s.length + ',' + s[0];", "r", "HI!,2,h"),
        // The wrapper survives a property add (the shape changes) and a prototype change.
        ("var n = new Number(8); n.p = 1; Object.setPrototypeOf(n, {valueOf: function () { return 100; }}); var r = n + 1;", "r", "101"),
        ("var n = new Number(8); Number.prototype.valueOf.call(n); var r = Number.prototype.valueOf.call(n) + Number.prototype.toString.call(n); try { Number.prototype.valueOf.call({}); } catch (e) { r += e.name; }", "r", "88TypeError"),
        ("var r = String.prototype.valueOf.call(new String('q')) + Boolean.prototype.toString.call(new Boolean(true)); try { Boolean.prototype.valueOf.call('x'); } catch (e) { r += e.name; }", "r", "qtrueTypeError"),
    ]);
}

#[test]
fn a_wrapper_in_a_loop_under_constant_collection() {
    let n = if MIRI { 12 } else { 300 };
    let src = &format!("var t = 0; for (var i = 0; i < {n}; i++) {{ var w = new Number(i); w.k = i; var s = new String('v' + i); t += w + s.length + (w.k - i); }} var r = t;");
    let want = (0..n).map(|i: i32| i + (format!("v{i}").len() as i32)).sum::<i32>().to_string();
    assert_eq!(ev(src, "r"), want);
    assert_eq!(ev_gc(src, "r"), want);
}

#[test]
fn a_conversion_that_recurses_deep_grows_its_own_stack_and_the_outer_loop_goes_on() {
    // The nested run has a stack of its own: the outer frame (its registers) is still valid after a `valueOf` that grows a stack.
    let depth: i32 = if MIRI { 30 } else { 3000 };
    check(&[(
        &format!(
            "function down(n) {{ return n == 0 ? 0 : 1 + down(n - 1); }} var o = {{valueOf: function () {{ return down({depth}); }}}};
             var a = 1, b = 2, c = [3];
             var r = 0;
             for (var i = 0; i < 3; i++) {{ var x = a + b; r += (o + x) + c[0]; c[0] = c[0] + 1; }}
             r = r + ',' + a + b + c[0];"
        ),
        "r",
        &format!("{},126", (0..3).map(|i| depth + 3 + 3 + i).sum::<i32>()),
    )]);
}

#[test]
fn a_prelude_function_that_an_outer_run_compiled_works_in_a_nested_call() {
    // `map` waits as a stub until its first call (`Op::Lazy`). The outer run compiles it; a `valueOf` that calls it later works.
    check(&[(
        "[1].map(function (x) { return x; }); var o = {valueOf: function () { return [1, 2, 3].map(function (x) { return x * 2; }).length; }}; var r = o + 1;",
        "r",
        "4",
    )]);
}

#[test]
fn a_prelude_function_that_waits_is_compiled_at_its_first_call_inside_a_nested_call() {
    // The first call of `map`, `sort`, `reduce`, `Array.from` is inside a `valueOf`, a getter, a setter and a `toString` that a native runs. Each
    // compiles its stub in the nested run. The outer loop goes on, and it calls the same functions (and the helpers that they call, which are
    // new codes that its feedback vector did not have) afterwards.
    check(&[
        ("var o = {valueOf: function () { return [1, 2, 3].map(function (x) { return x * 2; }).length; }}; var r = (o + 1) + ',' + [4, 5].map(function (x) { return x; }).join();", "r", "4,4,5"),
        ("var o = {get g() { return [3, 1, 2].sort().join(); }}; var r = Object.getOwnPropertyDescriptor(o, 'g').get.call(o) + ',' + Object.assign({}, o).g + ',' + [2, 1].sort().join();", "r", "1,2,3,1,2,3,1,2"),
        ("var s; var o = {set v(x) { s = [1, 2, 3].reduce(function (a, b) { return a + b; }, x); }}; Object.assign(o, {v: 10}); var r = s + ',' + [1, 2].reduce(function (a, b) { return a + b; });", "r", "16,3"),
        ("var o = {toString: function () { return Array.from({length: 2, 0: 'a', 1: 'b'}).join('-'); }}; var r = ('x' + o) + ',' + `${o}` + ',' + Array.from([7]).join();", "r", "xa-b,a-b,7"),
    ]);
}

#[test]
fn a_stub_first_called_two_levels_deep_compiles_and_the_levels_above_use_what_it_made() {
    // `valueOf` -> a getter (read by `Object.assign`) -> `valueOf` again -> the first call of `map` and of `sort`. The three machines
    // are nested; the one at the top made no feedback for the codes that the lowest compile added.
    check(&[(
        "var inner = {valueOf: function () { return [2, 1].sort().length + [1].map(function (x) { return x; }).length; }};
         var mid = {get g() { return inner + 1; }};
         var outer = {valueOf: function () { return Object.assign({}, mid).g + [3, 2, 1].sort().join(''); }};
         var r = (outer + '') + ',' + [9, 8].sort().join() + ',' + [5].map(function (x) { return x + 1; }).join() + ',' + (inner + 0);",
        "r",
        "4123,8,9,6,3",
    )]);
}

#[test]
fn a_function_that_waits_and_is_called_in_a_nested_run_that_throws_stays_usable() {
    // The nested run compiles `forEach` and then throws; the catch in the outer run calls it again (it is compiled), and a conversion
    // that never ran it before compiles another one.
    check(&[(
        "var o = {valueOf: function () { [1].forEach(function () {}); throw new Error('x'); }}; var e = 0; try { o + 1; } catch (x) { e = x.message; }
         var n = 0; [1, 2].forEach(function (v) { n += v; }); var p = {valueOf: function () { return [3, 4].find(function (v) { return v > 3; }); }};
         var r = e + n + (p + 0);",
        "r",
        "x34",
    )]);
}
