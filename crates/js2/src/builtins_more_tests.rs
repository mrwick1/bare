// Tests of the built-ins that the test262 round of String, Array, Number, Math and JSON added or fixed (`bi_json.rs`, `bi_string_more.rs`,
// `bi_number.rs`, `bi_array.rs`, `bi_string.rs`, `prelude.js`). One case for each cause. Each runs normally and under constant collection.
use crate::heap::Trigger;
use crate::script::{load, run};

/// Run `src`, then read the global `result` as text.
fn ev(src: &str) -> String {
    match run(src) {
        Ok(mut vm) => vm.global_text("result"),
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
    vm.global_text("result")
}

fn check(cases: &[(&str, &str)]) {
    for (src, want) in cases {
        let src = format!("var result = (function () {{ {src} }})();");
        assert_eq!(ev(&src), *want, "{src}");
        if !cfg!(miri) {
            assert_eq!(ev_gc(&src), *want, "under collection: {src}");
        }
    }
}

#[test]
fn json_parse_values_and_structure() {
    check(&[
        ("var o = JSON.parse('{\"a\":[1,2.5e2,-0,true,null],\"b\":{\"c\":\"x\"}}'); return o.a.length + ',' + o.a[1] + ',' + (1 / o.a[2]) + ',' + o.a[3] + ',' + o.a[4] + ',' + o.b.c;", "5,250,-Infinity,true,null,x"),
        ("return JSON.parse(' \\t\\n\\r[ ] ').length + ',' + typeof JSON.parse('{}') + ',' + JSON.parse('\"s\"') + ',' + JSON.parse('12') + ',' + JSON.parse('null') + ',' + JSON.parse(true);", "0,object,s,12,null,true"),
        // A repeated key keeps the last value; `__proto__` is an own property; an index key is an index.
        ("var o = JSON.parse('{\"a\":1,\"a\":2,\"__proto__\":5,\"3\":6}'); return o.a + ',' + o.__proto__ + ',' + Object.keys(o).join() + ',' + (Object.getPrototypeOf(o) === Object.prototype);", "2,5,3,a,__proto__,true"),
        ("var s = JSON.parse('\"\\\\u0041\\\\n\\\\/\\\\\\\\\\\\ud800\"'); return s.length + ',' + s.charCodeAt(0) + ',' + s.charCodeAt(1) + ',' + s.charAt(2) + ',' + s.charAt(3) + ',' + s.charCodeAt(4).toString(16);", "5,65,10,/,\\,d800"),
        ("return JSON.parse('1E2') + ',' + JSON.parse('-1.5e-1') + ',' + JSON.parse('0') + ',' + JSON.parse('123456789012345678901234567890');", "100,-0.15,0,1.2345678901234568e+29"),
    ]);
}

#[test]
fn json_parse_errors_are_syntax_errors() {
    check(&[
        ("var bad = ['', '{', '[1,]', '{\"a\":1,}', '01', '1.', '.5', '+1', '-', '\"\\\\x\"', '\"\\u0001\"', \"'a'\", 'nul', 'tru e', '[1 2]', '{\"a\" 1}', '{a:1}', '1 2', '\"\\\\u12G4\"', '\\u00a01', 'undefined', 'NaN'];\
          var n = 0; for (var i = 0; i < bad.length; i++) { try { JSON.parse(bad[i]); } catch (e) { if (e instanceof SyntaxError) n++; } } return n + ',' + bad.length;", "22,22"),
        ("try { JSON.parse('[1,]'); } catch (e) { return e.name + ':' + e.message; }", "SyntaxError:Unexpected token ] in JSON at position 3"),
        ("try { JSON.parse(''); } catch (e) { return e.name + ':' + e.message; }", "SyntaxError:Unexpected end of JSON input"),
        ("var s = ''; for (var i = 0; i < 1100; i++) s += '['; try { JSON.parse(s); } catch (e) { return e.name; }", "RangeError"),
    ]);
}

#[test]
fn json_stringify_values_gaps_and_keys() {
    check(&[
        ("return JSON.stringify({a: 1, b: [1, 'x', null, undefined, function () {}], c: undefined, d: function () {}, e: NaN, f: -Infinity, g: true, h: -0});", "{\"a\":1,\"b\":[1,\"x\",null,null,null],\"e\":null,\"f\":null,\"g\":true,\"h\":0}"),
        ("return JSON.stringify(undefined) + ',' + JSON.stringify(function () {}) + ',' + JSON.stringify(null) + ',' + JSON.stringify(3) + ',' + JSON.stringify('a'); ", "undefined,undefined,null,3,\"a\""),
        ("return JSON.stringify({a: [], b: {}, c: [{}]}, null, 2);", "{\n  \"a\": [],\n  \"b\": {},\n  \"c\": [\n    {}\n  ]\n}"),
        ("return JSON.stringify([1, [2]], null, '--') + JSON.stringify([1], null, 20).length + JSON.stringify([1], null, 'abcdefghijklmnop').length + JSON.stringify([1], null, 0);", "[\n--1,\n--[\n----2\n--]\n]1515[1]"),
        ("return JSON.stringify({b: 1, a: 2, c: 3}, ['c', 'a', 'c', 7]) + JSON.stringify({1: 1, 7: 2}, [7]) + JSON.stringify([{a: 1, b: 2}], ['b']);", "{\"c\":3,\"a\":2}{\"7\":2}[{\"b\":2}]"),
        ("return JSON.stringify({k: 'a\"b\\\\c\\n\\u0007\\u2028\\ud800x\\udc00'});", "{\"k\":\"a\\\"b\\\\c\\n\\u0007\u{2028}\\ud800x\\udc00\"}"),
        ("return JSON.stringify('\\ud83d\\ude00') === '\"\\ud83d\\ude00\"';", "true"),
        ("var o = {}; o.self = [o]; try { JSON.stringify(o); } catch (e) { return e.name; }", "TypeError"),
        ("var o = {}; o.a = {b: 1}; o.c = o.a; return JSON.stringify(o);", "{\"a\":{\"b\":1},\"c\":{\"b\":1}}"),
        ("var a = []; for (var i = 0; i < 1100; i++) a = [a]; try { JSON.stringify(a); } catch (e) { return e.name; }", "RangeError"),
        ("return JSON.stringify(JSON.parse('{\"x\":[1,{\"y\":\"z\"}]}'));", "{\"x\":[1,{\"y\":\"z\"}]}"),
    ]);
}

#[test]
fn json_stringify_calls_to_json_with_the_key() {
    check(&[
        ("return JSON.stringify({toJSON: function (k) { return [1, k]; }, a: {toJSON: function (k) { return 'k' + k; }}});", "[1,\"\"]"),
        ("return JSON.stringify([{toJSON: function (k) { return 'k' + k; }}, {b: {toJSON: function (k) { return 'k' + k; }}}]);", "[\"k0\",{\"b\":\"kb\"}]"),
        ("return JSON.stringify({toJSON: 5, a: 1});", "{\"toJSON\":5,\"a\":1}"),
    ]);
}

#[test]
fn json_ignores_a_non_callable_to_json() {
    check(&[
        ("return JSON.stringify({toJSON: 5, a: 1});", "{\"toJSON\":5,\"a\":1}"),
    ]);
}

#[test]
fn json_has_the_attributes_of_a_built_in() {
    check(&[
        ("var d = Object.getOwnPropertyDescriptor(this, 'JSON'); return d.writable + ',' + d.enumerable + ',' + d.configurable + ',' + JSON.parse.length + ',' + JSON.stringify.length + ',' + JSON.parse.name + ',' + JSON.stringify.name;", "true,false,true,2,3,parse,stringify"),
    ]);
}

#[test]
fn math_has_the_hyperbolic_and_the_other_missing_functions() {
    check(&[
        ("return [Math.sinh(0), Math.cosh(0), Math.tanh(0), Math.asinh(0), Math.acosh(1), Math.atanh(0), Math.expm1(0), Math.log1p(0)].join();", "0,1,0,0,0,0,0,0"),
        ("return [Math.atanh(1), Math.atanh(-1), Math.acosh(0.5), Math.acosh(Infinity), Math.tanh(Infinity), Math.tanh(-Infinity), Math.sinh(-Infinity), Math.log1p(-1), Math.log1p(-2), Math.expm1(-Infinity), Math.cosh(-Infinity)].join();", "Infinity,-Infinity,NaN,Infinity,1,-1,-Infinity,-Infinity,NaN,-1,Infinity"),
        ("return [1 / Math.sinh(-0), 1 / Math.asinh(-0), 1 / Math.tanh(-0), 1 / Math.expm1(-0), 1 / Math.log1p(-0), 1 / Math.atanh(-0)].join();", "-Infinity,-Infinity,-Infinity,-Infinity,-Infinity,-Infinity"),
        ("return [Math.clz32(0), Math.clz32(1), Math.clz32(-1), Math.clz32(0.5), Math.clz32(4294967295), Math.clz32(4294967296), Math.clz32(NaN), Math.clz32(Infinity), Math.clz32('8'), Math.clz32()].join();", "32,31,0,32,0,32,32,32,28,32"),
        ("return [Math.sinh.length, Math.clz32.length, Math.hypot.length, Math.acosh.name].join();", "1,1,2,acosh"),
    ]);
}

#[test]
fn math_hypot_converts_every_argument_and_scales() {
    check(&[
        // The second argument converts through a native `valueOf` that throws; an infinity before it must not skip it.
        ("try { Math.hypot(Infinity, {valueOf: String.prototype.valueOf}); return 'no'; } catch (e) { return e.name; }", "TypeError"),
        ("return [Math.hypot(), Math.hypot(3, 4), Math.hypot(-3), Math.hypot(NaN, Infinity), Math.hypot(NaN, 1), Math.hypot(0, -0), Math.hypot(1e200, 1e200) > 1e200, Math.hypot(1e-200, 1e-200) > 0, Math.hypot(3, 4, 12)].join();", "0,5,3,Infinity,NaN,0,true,true,13"),
    ]);
}

#[test]
fn number_to_exponential_to_precision_and_to_locale_string() {
    check(&[
        ("return [(123.456).toExponential(2), (0).toExponential(), (0).toExponential(2), (-1234.5678).toExponential(), (1e21).toExponential(3), (0.00001).toExponential(), (25).toExponential(0), (12345).toExponential(3), (99.99).toExponential(1)].join();", "1.23e+2,0e+0,0.00e+0,-1.2345678e+3,1.000e+21,1e-5,3e+1,1.235e+4,1.0e+2"),
        ("return [(NaN).toExponential(5), Infinity.toExponential(), (-Infinity).toExponential(200)].join();", "NaN,Infinity,-Infinity"),
        ("var n = 0; for (var i = 0; i < 2; i++) { try { (1).toExponential(i ? -1 : 101); } catch (e) { if (e instanceof RangeError) n++; } } return n;", "2"),
        ("return [(123.456).toPrecision(4), (0.000123).toPrecision(2), (1e21).toPrecision(3), (123456).toPrecision(2), (0).toPrecision(3), (0).toPrecision(1), (1.25).toPrecision(2), (-0.0000001).toPrecision(3), (99.99).toPrecision(3), (123.456).toPrecision(), (1e-7).toPrecision(1), (0.000001).toPrecision(2)].join();", "123.5,0.00012,1.00e+21,1.2e+5,0.00,0,1.3,-1.00e-7,100,123.456,1e-7,0.0000010"),
        ("return [(NaN).toPrecision(0), Infinity.toPrecision(1000), (1).toPrecision(undefined)].join();", "NaN,Infinity,1"),
        ("var n = 0; for (var i = 0; i < 2; i++) { try { (1).toPrecision(i ? 101 : 0); } catch (e) { if (e instanceof RangeError) n++; } } return n;", "2"),
        ("return [(1234567.891).toLocaleString(), (0).toLocaleString(), (-1234.5).toLocaleString(), (0.0004).toLocaleString(), (999.9995).toLocaleString(), NaN.toLocaleString(), Infinity.toLocaleString(), (-0.0001).toLocaleString()].join('|');", "1,234,567.891|0|-1,234.5|0|1,000|NaN|∞|-0"),
        ("return [Number.parseInt === parseInt, Number.parseFloat === parseFloat, Number.prototype.toString.length, Number.prototype.toLocaleString.length].join();", "true,true,1,0"),
    ]);
}

#[test]
fn string_raw_from_code_point_and_the_well_formed_methods() {
    check(&[
        ("return String.raw`a\\n${1}b${2}` + ',' + String.raw({raw: ['x', 'y', 'z']}, 1) + ',' + String.raw({raw: {length: 2, 0: 'p', 1: 'q'}}, 'S', 'T') + ',' + String.raw({raw: []}) + ',' + String.raw({raw: 'abc'}, 1, 2, 3);", "a\\n1b2,x1yz,pSq,,a1b2c"),
        ("var n = 0; try { String.raw(); } catch (e) { if (e instanceof TypeError) n++; } try { String.raw({}); } catch (e) { if (e instanceof TypeError) n++; } return n;", "2"),
        ("return String.fromCodePoint(65, 0x1F600, 0xFFFF).length + ',' + String.fromCodePoint(0x1F600).charCodeAt(0).toString(16) + ',' + String.fromCodePoint(0x1F600).charCodeAt(1).toString(16) + ',' + String.fromCodePoint().length;", "4,d83d,de00,0"),
        ("var n = 0; var bad = [-1, 1.5, 0x110000, NaN, Infinity, 'x']; for (var i = 0; i < bad.length; i++) { try { String.fromCodePoint(bad[i]); } catch (e) { if (e instanceof RangeError) n++; } } return n;", "6"),
        ("return ['ab'.isWellFormed(), 'a\\ud800'.isWellFormed(), '\\ud83d\\ude00'.isWellFormed(), '\\ude00\\ud83d'.isWellFormed()].join() + ',' + 'a\\ud800b'.toWellFormed().charCodeAt(1).toString(16) + ',' + 'ok'.toWellFormed();", "true,false,true,false,fffd,ok"),
    ]);
}

#[test]
fn string_normalize_locale_case_and_the_methods_that_need_a_regexp() {
    check(&[
        ("var s = '\\u1E9B\\u0323'; return [s.normalize('NFC') === '\\u1E9B\\u0323', s.normalize('NFD') === '\\u017F\\u0323\\u0307', s.normalize() === s.normalize('NFC'), 'e\\u0301'.normalize().length, '\\u00e9'.normalize('NFD').length, 'abc'.normalize() === 'abc'].join();", "true,true,true,1,2,true"),
        ("var n = 0; var bad = ['nfc', 'NFX', '', null, 1]; for (var i = 0; i < bad.length; i++) { try { 'a'.normalize(bad[i]); } catch (e) { if (e instanceof RangeError) n++; } } return n;", "5"),
        ("return 'a\\ud800\\u0301'.normalize('NFC').length + ',' + 'a\\u0301\\ud800'.normalize('NFC').length;", "3,2"),
        ("return 'AbC'.toLocaleLowerCase() + 'AbC'.toLocaleUpperCase() + 'ß'.toLocaleUpperCase() + 'a\\ud800'.toLocaleUpperCase().charCodeAt(1).toString(16) + 'ΑΣ'.toLocaleLowerCase();", "abcABCSSd800ας"),
        ("var n = 0; var f = ['match', 'matchAll', 'search']; for (var i = 0; i < f.length; i++) { try { String.prototype[f[i]].call(undefined, 'a'); } catch (e) { if (e instanceof TypeError) n++; } try { 'a'[f[i]]('a'); } catch (e) { if (e instanceof TypeError) n++; } } return n + ',' + String.prototype.match.length + String.prototype.search.length + String.prototype.matchAll.length;", "3,111"),
        ("return ['o\\u0308'.localeCompare('\\u00f6'), 'a'.localeCompare('b'), 'b'.localeCompare('a'), 'a'.localeCompare('a')].join();", "0,-1,1,0"),
        ("return ['abc'.charAt(), 'abc'.charAt(NaN), 'abc'.charAt(undefined), 'abc'.charAt(-0.99999), 'abc'.charAt(1.9), 'abc'.charAt(-1) === '', 'abc'.charAt(3) === ''].join();", "a,a,a,a,b,true,true"),
    ]);
}

#[test]
fn array_keys_values_and_entries() {
    check(&[
        ("var it = ['a', 'b'].entries(); var x = it.next(), y = it.next(), z = it.next(); return x.value[0] + x.value[1] + x.done + y.value + y.done + z.value + z.done;", "0afalse1,bfalseundefinedtrue"),
        ("var k = [7, 8].keys(), v = [7, 8].values(); return [k.next().value, k.next().value, k.next().done, v.next().value, v.next().value, v.next().done].join();", "0,1,true,7,8,true"),
        // After the end an iterator stays done, even when the array grows; the length is read at each step.
        ("var a = [1]; var it = a.values(); it.next(); it.next(); a.push(2); return it.next().done + ',' + [1].values().next.length;", "true,0"),
        ("var a = [1]; var it = a.values(); it.next(); a.push(2); return it.next().value;", "2"),
        ("var it = {length: 2, 0: 'x', 1: 'y'}; var i = Array.prototype.values.call(it); return i.next().value + i.next().value + i.next().done;", "xytrue"),
        ("var it = [1].keys(); return Object.keys(it).length + ',' + Object.getOwnPropertyNames(it).length + ',' + typeof it.next + ',' + JSON.stringify(it) + ',' + (Object.getPrototypeOf(it) === Object.getPrototypeOf([].values()));", "0,0,function,{},true"),
        ("var n = 0; var next = [].keys().next; var bad = [undefined, null, 1, {}, [], 'x']; for (var i = 0; i < bad.length; i++) { try { next.call(bad[i]); } catch (e) { if (e instanceof TypeError) n++; } } try { Array.prototype.keys.call(undefined); } catch (e) { if (e instanceof TypeError) n++; } return n;", "7"),
        ("var d = Object.getOwnPropertyDescriptor(Array.prototype, 'entries'); return d.writable + ',' + d.enumerable + ',' + d.configurable + ',' + Array.prototype.keys.length + ',' + Array.prototype.values.name;", "true,false,true,0,values"),
    ]);
}

#[test]
fn array_species_constructor_must_be_undefined_or_an_object() {
    check(&[
        ("var out = []; var fs = ['map', 'filter', 'flat', 'flatMap', 'slice', 'splice', 'concat']; for (var i = 0; i < fs.length; i++) { var a = [1]; a.constructor = null; try { a[fs[i]](function (x) { return x; }); out.push('no'); } catch (e) { out.push(e.name); } } return out.join();", "TypeError,TypeError,TypeError,TypeError,TypeError,TypeError,TypeError"),
        ("var out = []; var fs = ['map', 'filter', 'slice', 'concat']; var bad = [1, 'x', true]; for (var j = 0; j < bad.length; j++) for (var i = 0; i < fs.length; i++) { var a = [1]; a.constructor = bad[j]; try { a[fs[i]](function (x) { return x; }); out.push('no'); } catch (e) { out.push(e.name); } } return out.length + ',' + out.filter(function (n) { return n === 'TypeError'; }).length;", "12,12"),
        ("var a = [1, 2]; a.constructor = undefined; var b = [3]; b.constructor = {}; return a.map(function (x) { return x * 2; }).join() + '|' + b.filter(function () { return true; }).join() + '|' + b.slice().join() + '|' + [1].concat([2]).join() + '|' + [[1]].flat().join();", "2,4|3|3|1,2|1"),
        // A non-array `this` skips the check.
        ("var o = {length: 1, 0: 5, constructor: null}; return Array.prototype.map.call(o, function (x) { return x + 1; }).join();", "6"),
    ]);
}

#[test]
fn the_function_constructor_builds_a_global_function() {
    check(&[
        ("return new Function('a', 'b', 'return a + b')(2, 3) + ',' + Function('return 7')();", "5,7"),
        ("return Function('a, b', 'c', 'return a + b + c')(1, 2, 3) + ',' + Function('a', 'b = a', 'return b')(4);", "6,4"),
        ("var f = Function('a', 'return a'); return f.name + ',' + f.length + ',' + (typeof f) + ',' + (Object.getPrototypeOf(f) === Function.prototype);", "anonymous,1,function,true"),
        ("return Function().name + ',' + Function('').length + ',' + Function()();", "anonymous,0,undefined"),
        // The scope is the global one, and the function has no binding for its own name.
        ("var x = 'local'; globalThis.gx = 'global'; return Function('return typeof x + gx')() + ',' + Function('return typeof anonymous')();", "undefinedglobal,undefined"),
        // The arguments are converted to text in order, the body last.
        ("var log = []; var o = function (n, v) { return {toString: function () { log.push(n); return v; }}; }; Function(o('p', 'a'), o('q', 'b'), o('body', 'return a'))(1); return log.join();", "p,q,body"),
        ("var o = {toString: function () { throw new RangeError('r'); }}; try { Function(o, 'return 1'); } catch (e) { return e.name; }", "RangeError"),
        // A comment or a brace in a part cannot reach across.
        ("var out = []; var bad = [['/*', '*/){'], ['a){ return 1; }; (function (b', 'return 2'], ['', '}); (function () {'], ['a //', 'return a'], ['', '}\\nvar x = function () {']]; for (var i = 0; i < bad.length; i++) { try { var f = Function(bad[i][0], bad[i][1]); out.push(i === 3 ? 'ok' : 'no'); } catch (e) { out.push(e.name); } } return out.join();", "SyntaxError,SyntaxError,SyntaxError,ok,SyntaxError"),
        ("try { Function('return ]'); } catch (e) { return e.name; }", "SyntaxError"),
        ("try { Function('a', 'a', '\"use strict\";'); } catch (e) { return e.name; }", "SyntaxError"),
        ("var f = Function('return this'); return (f() === globalThis) + ',' + (new Function('this.z = 1; return this') instanceof Function);", "true,true"),
        ("var f = new Function('a', 'this.v = a'); var o = new f(9); return o.v + ',' + (o instanceof f);", "9,true"),
    ]);
}

#[test]
fn primitive_prototypes_have_their_own_wrapped_values() {
    check(&[
        ("return Number.prototype.valueOf() + ':' + Number.prototype.toString(2) + ':' + Object.prototype.toString.call(Number.prototype);", "0:0:[object Number]"),
        ("return Boolean.prototype.valueOf() + ':' + String.prototype.valueOf() + ':' + String.prototype.length;", "false::0"),
        ("try { return Object.create(Number.prototype).valueOf(); } catch (e) { return e.name; }", "TypeError"),
    ]);
}

#[test]
fn bind_constructs_with_the_bound_arguments_and_reads_metadata_at_bind_time() {
    check(&[
        ("function A(x, y, z) { this.v = [x, y, z, new.target === A]; } var B = A.bind(null, 1); var C = B.bind(null, 2); return [new C(3).v.join(), new C() instanceof B, C.length, C.name].join('|');", "1,2,3,true|true|1|bound bound A"),
        ("function A() { this.target = new.target; } function B() {} var C = A.bind(null); return Reflect.construct(C, [], B).target === B;", "true"),
        ("var f = () => {}; try { new (f.bind(null))(); } catch (e) { return e.name; }", "TypeError"),
        ("function f(x) {} Object.defineProperty(f, 'name', {get() { throw 'name'; }}); try { f.bind(); } catch (e) { return e; }", "name"),
        ("function f(x) {} var b = f.bind(); Object.defineProperty(f, 'name', {value: 'later'}); Object.defineProperty(f, 'length', {value: 8}); return b.name + ':' + b.length;", "bound f:1"),
        ("function f(v) { return this.offset + v; } Object.defineProperty(f, 'length', {get() { throw 'length'; }}); Object.defineProperty(f, 'name', {get() { throw 'name'; }}); return [3].map(f, {offset: 4})[0];", "7"),
    ]);
}

#[test]
fn json_callbacks_see_the_holder_and_visit_properties_in_order() {
    check(&[
        (r#"var seen = []; var o = JSON.parse('{"a":[1,2],"b":3}', function (k,v) { seen.push(k); if (k === '0') { this[1] = 7; return undefined; } return v; }); return seen.join('|') + ':' + JSON.stringify(o);"#, "0|1|a|b|:{\"a\":[null,7],\"b\":3}"),
        (r#"var seen = []; var o = { a: {toJSON() { return 5; }}, b: 2 }; var s = JSON.stringify(o, function(k,v) { seen.push(this[k] === o.a ? k + '=' + v : k); if (k === 'a') delete this.b; return v; }); return seen.join('|') + ':' + s;"#, "|a=5|b:{\"a\":5}"),
        (r#"return JSON.stringify([new Number(2), new Boolean(false), new String('x')]);"#, "[2,false,\"x\"]"),
        (r#"try { JSON.parse('[', function() { throw 'wrong'; }); } catch (e) { return e.name; }"#, "SyntaxError"),
    ]);
}

#[test]
fn prelude_builtins_have_no_construct_slot_before_or_after_call() {
    check(&[
        ("var f = Array.prototype.map; function tryNew() { try { Reflect.construct(Object, [], f); return 'yes'; } catch (e) { return e.name; } } var before = tryNew(); [].map(function () {}); return before + ':' + tryNew() + ':' + Object.hasOwn(f, 'prototype');", "TypeError:TypeError:false"),
        ("var f = Array.from; try { new f(); } catch (e) { return e.name; }", "TypeError"),
    ]);
}
