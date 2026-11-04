//! `Object.getOwnPropertyNames`, `Reflect.ownKeys` and `Reflect.construct` tests. The corpus
//! (`tools/fixtures/js/reflect-cases.txt`) is checked against what Node gave (`reflect-cases.expected`;
//! `tools/js/reflect_check.sh` re-runs Node). The tests below cover what Node cannot show: gates, caps, the
//! checkpoint contract, the collector and several scripts.

use super::*;
use crate::heap::Heap;

fn big<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(64 << 20).spawn(f).unwrap().join().unwrap()
}

fn esc(s: &str) -> String {
    s.replace('\\', "\\\\").replace('\n', "\\n").replace('\r', "\\r").replace('\t', "\\t")
}

fn show(it: &mut Interp, r: Result<Value, Error>) -> String {
    match r {
        Ok(v) => esc(&it.display(v)),
        Err(Error::Throw(v)) if matches!(v, Value::Object(_)) => format!("throw:{}:{}", esc(&it.get_str(v, "name")), esc(&it.get_str(v, "message"))),
        Err(Error::Throw(v)) => format!("throw-value:{}", esc(&it.display(v))),
        Err(Error::Unsupported(u)) => format!("unsupported:{}", u.what),
        Err(Error::Limit(k)) => format!("limit:{k:?}"),
        Err(e) => format!("internal-error:{e:?}"),
    }
}

fn out(it: &mut Interp, src: &str) -> String {
    let r = it.run_script(src);
    show(it, r)
}

fn realm_with(steps: u64) -> Interp<'static> {
    Interp::realm(Heap::default(), EvalLimits { max_steps: steps, ..Default::default() }).unwrap()
}

fn realm() -> Interp<'static> {
    realm_with(50_000_000)
}

#[test]
fn corpus_matches_node() {
    big(|| {
        let want: Vec<&str> = include_str!("../../../tools/fixtures/js/reflect-cases.expected").lines().collect();
        let mut cases: Vec<(&str, Vec<&str>)> = Vec::new();
        for line in include_str!("../../../tools/fixtures/js/reflect-cases.txt").lines() {
            if let Some(n) = line.strip_prefix("=== ") {
                cases.push((n.trim(), Vec::new()));
            } else if let Some(c) = cases.last_mut() {
                c.1.push(line);
            }
        }
        assert_eq!(cases.len(), want.len());
        let mut bad = Vec::new();
        for ((name, lines), w) in cases.iter().zip(want) {
            let mut it = realm();
            let reader = match it.run_script(&lines.join("\n")) {
                Ok(v) => v,
                Err(e) => {
                    let s = show(&mut it, Err(e));
                    bad.push(format!("{name}: {s}"));
                    continue;
                }
            };
            let a = it.call(reader, Value::Undefined, &[]);
            let a = show(&mut it, a);
            it.collect(&[reader]).unwrap();
            let b = it.call(reader, Value::Undefined, &[]);
            let b = show(&mut it, b);
            let got = format!("{name}\t{a} ## {b}");
            if got != w {
                bad.push(format!("node: {w}\nbare: {got}"));
            }
        }
        assert!(bad.is_empty(), "{} differences:\n{}", bad.len(), bad.join("\n"));
    });
}

#[test]
fn reflect_is_a_genuine_object_with_only_the_two_methods() {
    big(|| {
        let mut it = realm();
        assert_eq!(out(&mut it, "typeof Reflect"), "object");
        assert_eq!(out(&mut it, "Reflect.ownKeys(Reflect).length"), "3");
        assert_eq!(out(&mut it, "Reflect.ownKeys(Reflect).slice(0, 2).join()"), "construct,ownKeys");
        assert_eq!(out(&mut it, "Reflect[Symbol.toStringTag]"), "Reflect");
        // not claimed: the rest of the API stays absent, never faked
        assert_eq!(out(&mut it, "typeof Reflect.get + typeof Reflect.set + typeof Reflect.has + typeof Reflect.apply + typeof Reflect.defineProperty + typeof Reflect.getPrototypeOf"), "undefinedundefinedundefinedundefinedundefinedundefined");
        assert_eq!(out(&mut it, "typeof Proxy"), "undefined");
        assert_eq!(out(&mut it, "var d = Object.getOwnPropertyDescriptor(globalThis, 'Reflect'); [d.writable, d.enumerable, d.configurable].join()"), "true,false,true");
        assert_eq!(out(&mut it, "var d = Object.getOwnPropertyDescriptor(Reflect, 'construct'); [d.writable, d.enumerable, d.configurable].join()"), "true,false,true");
        assert_eq!(out(&mut it, "var d = Object.getOwnPropertyDescriptor(Object, 'getOwnPropertyNames'); [d.writable, d.enumerable, d.configurable].join()"), "true,false,true");
        // the two methods are plain natives: callable, not constructors
        assert_eq!(out(&mut it, "try { new Reflect.construct(Object, []); } catch (e) { e.name }"), "TypeError");
    });
}

#[test]
fn natives_whose_subclass_plumbing_is_missing_stay_honest_gates() {
    big(|| {
        for ctor in ["Map", "Set", "WeakMap", "WeakSet", "Promise", "AggregateError", "ArrayBuffer", "Uint8Array", "TextEncoder", "TextDecoder"] {
            let mut it = realm();
            out(&mut it, "function G() {}");
            let r = out(&mut it, &format!("Reflect.construct({ctor}, [], G)"));
            assert!(r.starts_with("unsupported:subclassing"), "{ctor}: {r}");
            // the class form was already a gate, and stays one
            let r = out(&mut it, &format!("class S extends {ctor} {{}} new S()"));
            assert!(r.starts_with("unsupported:subclassing"), "{ctor}: {r}");
            // the same constructor as new.target is a plain `new`
            let args = match ctor {
                "Promise" => "function () {}",
                "ArrayBuffer" | "Uint8Array" => "2",
                "AggregateError" => "[]",
                _ => "",
            };
            let r = out(&mut it, &format!("typeof Reflect.construct({ctor}, [{args}], {ctor})"));
            assert_eq!(r, "object", "{ctor}");
            // a bound function whose target is the same constructor is `new.target === target` only through the target
            let r = out(&mut it, &format!("var B = {ctor}.bind(null); typeof Reflect.construct(B, [{args}])"));
            assert_eq!(r, "object", "{ctor} bound");
            let r = out(&mut it, &format!("Reflect.construct(B, [{args}], G)"));
            assert!(r.starts_with("unsupported:subclassing"), "{ctor} bound with new.target: {r}");
        }
    });
}

#[test]
fn string_number_boolean_constructors_are_gates_not_type_errors() {
    big(|| {
        for c in ["String", "Number", "Boolean"] {
            let mut it = realm();
            let want = "unsupported:wrapper object (new String/Number/Boolean)";
            assert_eq!(out(&mut it, &format!("Reflect.construct({c}, [])")), want);
            assert_eq!(out(&mut it, &format!("function G() {{}} Reflect.construct({c}, [], G)")), want);
            assert_eq!(out(&mut it, &format!("Reflect.construct({c}.bind(null), [], G)")), want);
            assert_eq!(out(&mut it, &format!("Reflect.construct({c}.bind(null), [])")), want);
            // as new.target they are valid constructors: only the prototype is read
            // (`Boolean.prototype` does not exist in bare yet, so its prototype falls back to Object.prototype)
            let want_proto = if c == "Boolean" { "Object.prototype" } else { &format!("{c}.prototype") };
            assert_eq!(out(&mut it, &format!("function F() {{}} Object.getPrototypeOf(Reflect.construct(F, [], {c})) === {want_proto}")), "true");
            assert_eq!(out(&mut it, &format!("class K {{ constructor() {{ this.t = new.target === {c}; }} }} Reflect.construct(K, [], {c}).t")), "true");
        }
    });
}

#[test]
fn async_functions_and_methods_are_not_constructors() {
    big(|| {
        let mut it = realm();
        assert_eq!(out(&mut it, "var r = []; function F() {} var af = async function () {};
            function t(f) { try { f(); r.push('ok'); } catch (e) { r.push(e.name); } }
            t(function () { Reflect.construct(af, []); });
            t(function () { Reflect.construct(F, [], af); });
            t(function () { Reflect.construct(af.bind(null), []); });
            t(function () { new af(); });
            t(function () { class X extends af {} });
            r.join()"), "TypeError,TypeError,TypeError,TypeError,TypeError");
    });
}

#[test]
fn getters_run_for_arguments_and_missing_new_target_is_not_undefined() {
    big(|| {
        let mut it = realm();
        // `Reflect.construct(F, [], undefined)` passes the argument: it is not "absent"
        assert_eq!(out(&mut it, "function F() {} try { Reflect.construct(F, [], undefined); 'ok' } catch (e) { e.name }"), "TypeError");
        assert_eq!(out(&mut it, "Reflect.construct(F, []) instanceof F"), "true");
    });
}

#[test]
fn argument_list_is_bounded_and_charged_before_it_grows() {
    big(|| {
        // a huge length is a RangeError (as in apply), and no index getter runs
        let mut it = realm();
        out(&mut it, "var hits = 0; function F() {} var al = { get length() { return 4294967296; }, get 0() { hits++; return 1; } };");
        assert_eq!(out(&mut it, "try { Reflect.construct(F, al) } catch (e) { e.name }"), "RangeError");
        assert_eq!(out(&mut it, "hits"), "0");
        assert_eq!(out(&mut it, "try { Reflect.construct(F, { length: 9007199254740991 }) } catch (e) { e.name }"), "RangeError");
        assert_eq!(out(&mut it, "try { Reflect.construct(F, { length: Infinity }) } catch (e) { e.name }"), "RangeError");
        assert_eq!(out(&mut it, "try { Reflect.construct(F, { length: (1 << 20) + 1 }) } catch (e) { e.name }"), "RangeError");
        // a length in range but beyond the step budget stops before any index is read and cannot be caught
        let mut it = realm_with(100_000);
        out(&mut it, "var hits = 0; function F() {} var al = { length: 1 << 20, get 0() { hits++; return 1; } };");
        assert_eq!(out(&mut it, "try { Reflect.construct(F, al) } catch (e) { 'caught' }"), "limit:Steps");
        assert_eq!(out(&mut it, "hits"), "0");
        // a length that fits runs the constructor with that many arguments
        let mut it = realm();
        assert_eq!(out(&mut it, "function F() { this.n = arguments.length; } Reflect.construct(F, { length: 50000 }).n"), "50000");
    });
}

#[test]
fn views_and_long_strings_are_charged_before_their_key_list_exists() {
    big(|| {
        // 4 Mi virtual keys with a 1 Mi step budget: a limit, not an allocation
        for call in ["Object.getOwnPropertyNames(u)", "Reflect.ownKeys(u)"] {
            let mut it = realm_with(1 << 20);
            out(&mut it, "var u = new Uint8Array(1 << 22);");
            assert_eq!(out(&mut it, call), "limit:Steps", "{call}");
        }
        let mut it = realm_with(1 << 20);
        out(&mut it, "var s = 'x'; for (var i = 0; i < 22; i++) s = s + s;");
        assert_eq!(out(&mut it, "Object.getOwnPropertyNames(s)"), "limit:Steps");
        // with room, a small string and a small view give every index
        let mut it = realm();
        assert_eq!(out(&mut it, "Object.getOwnPropertyNames('héllo').join()"), "0,1,2,3,4,length");
        assert_eq!(out(&mut it, "Reflect.ownKeys(new Uint8Array(4)).join()"), "0,1,2,3");
        // heap bytes: a byte view as large as the heap allows is a heap limit, not a crash
        let mut it = Interp::realm(Heap::new(crate::heap::HeapLimits { max_bytes: 64 << 20, ..Default::default() }).unwrap(), EvalLimits { max_steps: 1 << 30, ..Default::default() }).unwrap();
        out(&mut it, "var u = new Uint8Array(1 << 24);");
        let r = out(&mut it, "Object.getOwnPropertyNames(u)");
        assert!(r.starts_with("limit:Heap") || r.starts_with("limit:Steps"), "{r}");
    });
}

#[test]
fn recursion_through_reflect_construct_hits_the_call_caps() {
    big(|| {
        let mut it = realm();
        assert_eq!(out(&mut it, "function R() { Reflect.construct(R, []); } try { new R() } catch (e) { 'caught' }"), "limit:CallDepth");
        let mut it = realm();
        assert_eq!(out(&mut it, "class C { constructor() { Reflect.construct(C, [], C); } } try { new C() } catch (e) { 'caught' }"), "limit:CallDepth");
        let mut it = realm();
        assert_eq!(out(&mut it, "function A() { return Reflect.construct(B, arguments, new.target); } function B() { return Reflect.construct(A, arguments, new.target); } try { new A() } catch (e) { 'caught' }"), "limit:CallDepth");
        // the interpreter is usable afterwards
        assert_eq!(out(&mut it, "typeof Reflect.construct(Object, [])"), "object");
    });
}

#[test]
fn a_limit_inside_reflect_construct_in_a_job_is_sticky_and_no_javascript_runs_after_it() {
    big(|| {
        let mut it = realm();
        out(&mut it, "var ran = 0; function Spin() { while (true) {} }
            Promise.resolve().then(function () { Reflect.construct(Spin, []); ran += 100; });
            Promise.resolve().then(function () { ran += 1; });");
        it.limits.max_steps = 5_000;
        let first = it.drain_microtasks();
        assert!(matches!(first, Err(Error::Limit(LimitKind::Steps))), "{first:?}");
        let queued = it.pending_jobs();
        for _ in 0..3 {
            assert!(matches!(it.drain_microtasks(), Err(Error::Limit(LimitKind::Steps))));
            assert_eq!(it.pending_jobs(), queued);
        }
        assert_eq!(out(&mut it, "ran"), "0");
    });
}

#[test]
fn a_throw_from_the_constructor_or_the_arguments_is_an_ordinary_throw() {
    big(|| {
        let mut it = realm();
        assert_eq!(out(&mut it, "var log = []; function F() { throw new RangeError('ctor'); }
            try { Reflect.construct(F, []); } catch (e) { log.push(e.name + e.message); }
            try { Reflect.construct(F, { length: 1, get 0() { throw 'str'; } }); } catch (e) { log.push(e); }
            try { Reflect.construct(F, { get length() { throw new TypeError('len'); } }); } catch (e) { log.push(e.name + e.message); }
            class K { constructor() { throw 7; } }
            try { Reflect.construct(K, [], F); } catch (e) { log.push(e); }
            log.join()"), "RangeErrorctor,str,TypeErrorlen,7");
        // the depth counters are restored after each throw
        assert_eq!(out(&mut it, "var n = 0; function T() { n++; } for (var i = 0; i < 300; i++) { try { Reflect.construct(F, []); } catch (e) {} Reflect.construct(T, []); } n"), "300");
    });
}

#[test]
fn constructors_and_new_target_work_across_scripts_and_after_collection() {
    big(|| {
        let mut it = realm();
        out(&mut it, "var seenA; class A { constructor(v) { this.v = v; seenA = new.target; } } function FA(v) { this.fa = v; this.nt = new.target === FB; }");
        out(&mut it, "function FB() {} class B extends A { constructor(v) { super(v + 1); this.b = true; } }");
        it.collect(&[]).unwrap();
        assert_eq!(out(&mut it, "var b = Reflect.construct(A, [1], B); [b.v, b instanceof B, b.b, seenA === B].join()"), "1,true,,true");
        assert_eq!(out(&mut it, "var c = Reflect.construct(B, [1], FB); [c.v, c instanceof FB, c.b, seenA === FB].join()"), "2,true,true,true");
        assert_eq!(out(&mut it, "var d = Reflect.construct(FA, [5], FB); [d.fa, d.nt, d instanceof FB, d instanceof FA].join()"), "5,true,true,false");
        // a callback made by one script is the new.target of another script's constructor, across a collection
        out(&mut it, "var keep = Reflect.construct(A, [9], FB);");
        it.collect(&[]).unwrap();
        assert_eq!(out(&mut it, "[keep.v, keep instanceof FB, Reflect.ownKeys(keep).join()].join()"), "9,true,v");
        let f = it.run_script("(function (x) { return Reflect.construct(A, [x], B).v; })").unwrap();
        it.collect(&[f]).unwrap();
        let r = it.call(f, Value::Undefined, &[Value::Number(4.0)]).unwrap();
        assert_eq!(it.display(r), "4");
        // the intrinsics are permanent: they survive a collection that has no other root
        it.collect(&[]).unwrap();
        assert_eq!(out(&mut it, "Object.getOwnPropertyNames({ a: 1 }).join() + Reflect.ownKeys({ b: 2 }).join() + typeof Reflect.construct(Object, [])"), "ab" .to_string() + "object");
    });
}

#[test]
fn a_non_enumerable_and_symbol_key_survive_a_collection() {
    big(|| {
        let mut it = realm();
        out(&mut it, "var sym = Symbol('keep'); var o = { a: 1 }; Object.defineProperty(o, 'h', { value: 1 }); o[sym] = 2;");
        it.collect(&[]).unwrap();
        assert_eq!(out(&mut it, "Object.getOwnPropertyNames(o).join()"), "a,h");
        assert_eq!(out(&mut it, "var k = Reflect.ownKeys(o); k.length + String(k[2] === sym)"), "3true");
    });
}

#[test]
fn heap_objects_cap_stops_construction_cleanly() {
    big(|| {
        let mut it = Interp::realm(Heap::new(crate::heap::HeapLimits { max_objects: 400, ..Default::default() }).unwrap(), EvalLimits::default()).unwrap();
        let r = out(&mut it, "function F() {} var keep = []; for (var i = 0; i < 1000; i++) keep.push(Reflect.construct(F, [])); 1");
        assert!(r.starts_with("limit:Heap"), "{r}");
        let mut it = Interp::realm(Heap::new(crate::heap::HeapLimits { max_objects: 400, ..Default::default() }).unwrap(), EvalLimits::default()).unwrap();
        let r = out(&mut it, "var keep = []; for (var i = 0; i < 1000; i++) keep.push(Reflect.ownKeys({})); 1");
        assert!(r.starts_with("limit:Heap"), "{r}");
    });
}
