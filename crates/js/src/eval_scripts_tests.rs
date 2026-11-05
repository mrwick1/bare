//! Shared-realm multi-script tests. Each test runs on a big stack. A realm keeps one global object, one
//! global lexical environment and one heap while scripts are added; a closure keeps the code of the script it came from.

use super::*;
use crate::heap::Heap;

fn big<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(64 << 20).spawn(f).unwrap().join().unwrap()
}

fn realm(limits: EvalLimits) -> Interp<'static> {
    Interp::realm(Heap::default(), limits).expect("realm")
}

fn show(it: &mut Interp, r: Result<Value, Error>) -> String {
    match r {
        Ok(v) => it.display(v),
        Err(Error::Throw(v)) if matches!(v, Value::Object(_)) => format!("throw:{}:{}", it.get_str(v, "name"), it.get_str(v, "message")),
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

fn limits() -> EvalLimits {
    EvalLimits { max_steps: 200_000, ..Default::default() }
}

#[test]
fn second_script_sees_vars_functions_and_lexicals() {
    big(|| {
        let mut it = realm(limits());
        assert_eq!(out(&mut it, "var a = 1; function f() { return a + 1 } let l = 10; const k = 5; 'ok'"), "ok");
        assert_eq!(out(&mut it, "a = 5; f() + l + k"), "21");
        // B changes a lexical A made, A's closure sees it
        assert_eq!(out(&mut it, "l = 100; f() + l"), "106");
    });
}

#[test]
fn new_then_run_script_keeps_the_first_program() {
    big(|| {
        let prog = crate::parse::parse_script("var a = 2; function f() { return a * 3 }", Default::default()).unwrap();
        let mut it = Interp::new(&prog, Heap::default(), limits()).unwrap();
        it.run().unwrap();
        assert_eq!(out(&mut it, "f() + a"), "8");
    });
}

#[test]
fn colliding_ids_keep_each_closure_on_its_own_code() {
    big(|| {
        let mut it = realm(limits());
        let a = "var o = { get x() { return 'ga' + n }, m() { return [1, 2].length } }; var n = 1;
                 function mk() { return function () { return 'A' + n } } var c = mk();";
        let b = "var p = { get x() { return 'gb' }, m() { return 'bm' } }; function g() { return 'B' }
                 o.x + o.m() + c() + g() + p.x + p.m()";
        assert_eq!(out(&mut it, a), "undefined");
        assert_eq!(out(&mut it, b), "ga12A1Bgbbm");
        // run again after more scripts: still the right code
        assert_eq!(out(&mut it, "var q = { m() { return 'cm' } }; o.m() + q.m() + p.m() + c()"), "2cmbmA1");
    });
}

#[test]
fn host_calls_callbacks_registered_by_a_later_script() {
    big(|| {
        let mut it = realm(limits());
        out(&mut it, "var q = []; function reg(f) { q.push(f) }");
        out(&mut it, "var tag = 'B'; reg(function (x) { return tag + x }); reg(function () { return typeof reg })");
        let f0 = it.run_script("q[0]").unwrap();
        let f1 = it.run_script("q[1]").unwrap();
        assert_eq!(out(&mut it, "1 + 1"), "2");
        let x = Value::String(it.heap.intern_str("x").unwrap());
        let r = it.call(f0, Value::Undefined, &[x]).unwrap();
        assert_eq!(it.display(r), "Bx");
        let r = it.call(f1, Value::Undefined, &[]).unwrap();
        assert_eq!(it.display(r), "function");
        // host call, then a script: state is intact
        assert_eq!(out(&mut it, "tag + q.length"), "B2");
    });
}

#[test]
fn throw_and_limits_restore_the_caller_code() {
    big(|| {
        let mut it = realm(limits());
        out(&mut it, "function boom() { throw new Error('a-fail') } function okA() { return 'A-ok' }
                      function spin() { while (true) {} } function rec() { return rec() }
                      function bad() { return new String('x') }");
        assert_eq!(out(&mut it, "function okB() { return 'B-ok' } var r; try { boom() } catch (e) { r = e.message } r + okB() + okA()"), "a-failB-okA-ok");
        assert_eq!(out(&mut it, "function k() { return 'k' } var s = k(); spin()"), "limit:Steps");
        assert_eq!(out(&mut it, "k() + okB() + okA() + s"), "kB-okA-okk");
        assert_eq!(out(&mut it, "function z() { return rec() } z()"), "limit:CallDepth");
        assert_eq!(out(&mut it, "z.name + k() + okA()"), "zkA-ok");
        assert_eq!(out(&mut it, "bad()"), "unsupported:wrapper object (new String/Number/Boolean)");
        assert_eq!(out(&mut it, "okB() + k()"), "B-okk");
        assert_eq!(out(&mut it, "try { boom() } catch (e) { e.message } "), "a-fail");
        // a host call that throws, then a script
        let f = it.run_script("boom").unwrap();
        assert!(matches!(it.call(f, Value::Undefined, &[]), Err(Error::Throw(_))));
        assert_eq!(out(&mut it, "okB() + okA()"), "B-okA-ok");
    });
}

#[test]
fn calls_cross_scripts_many_times() {
    big(|| {
        let mut it = realm(limits());
        out(&mut it, "function fa(n) { return n == 0 ? 0 : 1 + gb(n - 1) }");
        out(&mut it, "function gb(n) { return n == 0 ? 0 : 1 + fa(n - 1) }");
        assert_eq!(out(&mut it, "fa(60)"), "60");
        assert_eq!(out(&mut it, "var t = 0; for (var i = 0; i < 300; i++) t += fa(3); t"), "900");
    });
}

#[test]
fn global_redeclaration_rules() {
    big(|| {
        let mut it = realm(limits());
        out(&mut it, "let x = 1; var v = 2; function fn1() { return 'old' } const c = 3; let u;");
        let dup = "Identifier 'x' has already been declared";
        assert_eq!(out(&mut it, "let x = 5"), format!("throw:SyntaxError:{dup}"));
        assert_eq!(out(&mut it, "var x = 5"), format!("throw:SyntaxError:{dup}"));
        assert_eq!(out(&mut it, "function x() {}"), format!("throw:SyntaxError:{dup}"));
        assert_eq!(out(&mut it, "let v = 5"), "throw:SyntaxError:Identifier 'v' has already been declared");
        assert_eq!(out(&mut it, "const c = 9"), "throw:SyntaxError:Identifier 'c' has already been declared");
        assert_eq!(out(&mut it, "let fn1 = 1"), "throw:SyntaxError:Identifier 'fn1' has already been declared");
        // a var may be redeclared and a function may replace it. The old closure keeps old code
        let old = it.run_script("fn1").unwrap();
        assert_eq!(out(&mut it, "var v; function fn1() { return 'new' } v + fn1()"), "2new");
        let r = it.call(old, Value::Undefined, &[]).unwrap();
        assert_eq!(it.display(r), "old");
        // a failed script defines nothing, not even its hoisted names
        assert_eq!(out(&mut it, "var zz = 1; function hz() {} let x2 = 1; let x;"), format!("throw:SyntaxError:{dup}"));
        assert_eq!(out(&mut it, "typeof zz + typeof hz + typeof x2"), "undefinedundefinedundefined");
        // a lexical may shadow a configurable builtin
        assert_eq!(out(&mut it, "let Object = 7; Object"), "7");
        assert_eq!(out(&mut it, "let undefined2; typeof undefined2"), "undefined");
        // a lexical that threw in its initializer stays in its TDZ and stays declared
        assert_eq!(out(&mut it, "function bad() { throw new Error('init') } let t = bad();"), "throw:Error:init");
        assert_eq!(out(&mut it, "t"), "throw:ReferenceError:Cannot access 't' before initialization");
        assert_eq!(out(&mut it, "let t = 2"), "throw:SyntaxError:Identifier 't' has already been declared");
        // a non-configurable global property blocks a lexical of the same name
        assert_eq!(out(&mut it, "let NaN = 1"), "throw:SyntaxError:Identifier 'NaN' has already been declared");
    });
}

#[test]
fn syntax_error_and_unsupported_run_nothing() {
    big(|| {
        let mut it = realm(limits());
        let before = it.stats();
        assert!(out(&mut it, "var q = 1; var = ;").starts_with("throw:SyntaxError:"));
        assert_eq!(out(&mut it, "var zz = 1; async function f() { for (var k in await 1) {} }"), "unsupported:await inside for-in");
        let after = it.stats();
        assert_eq!(out(&mut it, "typeof zz + typeof q"), "undefinedundefined");
        assert_eq!((before.scripts, before.script_source, before.script_nodes), (after.scripts, after.script_source, after.script_nodes));
    });
}

#[test]
fn gc_between_scripts_and_after_clearing_globals() {
    big(|| {
        let mut it = realm(limits());
        out(&mut it, "var big = []; for (var i = 0; i < 200; i++) big.push({ i: i });
                      function sum() { var s = 0; for (var i = 0; i < big.length; i++) s += big[i].i; return s }
                      function lit() { return 'hello' + { key: 'v' }.key }
                      lit(); sum();");
        out(&mut it, "var other = 'b';");
        it.collect(&[]).unwrap();
        // allocate many strings so a wrongly freed cached literal would be reused
        out(&mut it, "var junk = []; for (var i = 0; i < 300; i++) junk.push('j' + i); 0");
        assert_eq!(out(&mut it, "lit() + sum()"), "hellov19900");
        let live = it.stats();
        assert!(live.functions > 3);
        out(&mut it, "big = undefined; sum = undefined; lit = undefined; junk = undefined;");
        let c = it.collect(&[]).unwrap();
        assert!(c.objects >= 200, "{c:?}");
        let after = it.stats();
        assert!(after.functions < live.functions);
        // the code stays until the realm ends, and the realm still runs
        assert_eq!(after.scripts, live.scripts + 1);
        assert_eq!(out(&mut it, "other + 1"), "b1");
    });
}

#[test]
fn script_survives_dropped_host_source() {
    big(|| {
        let mut it = realm(limits());
        let mut src = String::from("var s = 'kept'; function g() { return s + '!' }");
        out(&mut it, &src);
        src.clear();
        src.push_str(&"x".repeat(4096));
        drop(src);
        assert_eq!(out(&mut it, "g()"), "kept!");
    });
}

#[test]
fn script_count_source_and_node_limits() {
    big(|| {
        // count: the empty first slot counts
        let mut it = realm(EvalLimits { max_scripts: 3, ..limits() });
        assert_eq!(out(&mut it, "var a = 1"), "undefined");
        assert_eq!(out(&mut it, "var b = 2"), "undefined");
        assert_eq!(out(&mut it, "var c = 3"), "limit:Scripts");
        assert_eq!(it.stats().scripts, 3);
        assert_eq!(out(&mut it, "typeof a + typeof c"), "limit:Scripts");

        let mut it = realm(EvalLimits { max_script_source: 100, ..limits() });
        let s60 = format!("var a = 1; /*{}*/", "x".repeat(40));
        assert_eq!(out(&mut it, &s60), "undefined");
        assert_eq!(out(&mut it, &s60), "limit:ScriptSource");
        assert_eq!(it.stats().script_source, s60.len());
        assert_eq!(out(&mut it, "a"), "1");

        let mut it = realm(EvalLimits { max_script_nodes: 30, ..limits() });
        assert_eq!(out(&mut it, "var a = 1"), "undefined");
        let big_src = "var b = [1,2,3,4,5,6,7,8,9,10,11,12,13,14,15,16,17,18,19,20,21,22,23,24,25,26,27,28,29,30];";
        assert_eq!(out(&mut it, big_src), "limit:ScriptNodes");
        assert!(it.stats().script_nodes <= 30);
        assert_eq!(out(&mut it, "typeof b"), "undefined");
    });
}

#[test]
fn run_script_refuses_while_a_call_is_active() {
    big(|| {
        let mut it = realm(limits());
        // a host cannot reenter from native code here; check the direct guard
        it.depth = 1;
        assert_eq!(out(&mut it, "1"), "unsupported:run_script during a call");
        it.depth = 0;
        assert_eq!(out(&mut it, "1"), "1");
    });
}

#[test]
fn construct_checks_the_callee_script_not_the_current_one() {
    big(|| {
        let mut it = realm(limits());
        out(&mut it, "function Ctor() { this.v = 'a' }");
        // same function index in this script, but an arrow: not a constructor
        out(&mut it, "var arrow = () => 1; var o = new Ctor(); var bound = Ctor.bind(null);");
        assert_eq!(out(&mut it, "o.v + new bound().v"), "aa");
        assert_eq!(out(&mut it, "try { new arrow() } catch (e) { e.name }"), "TypeError");
    });
}

#[test]
fn promise_jobs_keep_their_script_across_realm_checkpoints() {
    big(|| {
        let mut it = realm(limits());
        assert_eq!(out(&mut it, "var out=[]; Promise.resolve(1).then(function(v){ out.push('A'+v); });"), "object");
        assert_eq!(out(&mut it, "Promise.resolve(2).then(function(v){ out.push('B'+v); });"), "object");
        it.collect(&[]).unwrap();
        assert_eq!(it.drain_microtasks().unwrap(), 2);
        assert_eq!(out(&mut it, "out.join('|')"), "A1|B2");
    });
}

#[test]
fn configurable_global_accessors_become_function_bindings() {
    big(|| {
        let mut it = realm(limits());
        out(&mut it, "var hits=0; Object.defineProperty(globalThis,'f',{get:function(){ return 1; },set:function(){ hits++; },configurable:true});");
        assert_eq!(out(&mut it, "function f(){ return 9; } f()+':'+hits;"), "9:0");
        assert_eq!(out(&mut it, "Object.getOwnPropertyDescriptor(globalThis,'f').writable"), "true");
    });
}

#[test]
fn runtime_throws_keep_code_for_already_created_closures() {
    big(|| {
        let mut it = realm(limits());
        assert_eq!(out(&mut it, "var survive=function(){ return 'kept'; }; throw 1;"), "throw-value:1");
        it.collect(&[]).unwrap();
        assert_eq!(out(&mut it, "survive()"), "kept");
    });
}

#[test]
fn per_script_lexer_source_cap_is_a_limit() {
    big(|| {
        let mut it = realm(EvalLimits { max_script_source: crate::lex::MAX_SOURCE_LEN * 2, ..limits() });
        let source = " ".repeat(crate::lex::MAX_SOURCE_LEN + 1);
        assert_eq!(out(&mut it, &source), "limit:ScriptSource");
        assert_eq!(it.stats().scripts, 1);
    });
}
