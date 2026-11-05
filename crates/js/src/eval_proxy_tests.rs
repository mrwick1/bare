//! `Proxy` tests. The corpus (`tools/fixtures/js/proxy-cases.txt`) is checked against what Node gave (`proxy-cases.expected`;
//! `tools/js/proxy_check.sh` re-runs Node). It holds the trap-order matrix and the invariant checks. The tests below cover what
//! Node cannot show: the caps (a 10k-deep chain, trap recursion, a runaway trap), the collector, and the paths that stay `Unsupported`.

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
        let want: Vec<&str> = include_str!("../../../tools/fixtures/js/proxy-cases.expected").lines().collect();
        let mut cases: Vec<(&str, Vec<&str>)> = Vec::new();
        for line in include_str!("../../../tools/fixtures/js/proxy-cases.txt").lines() {
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
fn a_10k_deep_proxy_chain_is_a_limit_not_a_stack_overflow() {
    big(|| {
        let build = "var t = { v: 1 }; var p = t; for (var i = 0; i < 10000; i++) p = new Proxy(p, {}); ";
        // every operation that walks the chain ends as a `Limit`; none overflows the native stack
        for op in [
            "p.v",
            "p.v = 2",
            "'v' in p",
            "delete p.v",
            "Object.keys(p)",
            "Object.getPrototypeOf(p)",
            "Object.setPrototypeOf(p, null)",
            "Object.isExtensible(p)",
            "Object.preventExtensions(p)",
            "Object.defineProperty(p, 'z', { value: 1 })",
            "Object.getOwnPropertyDescriptor(p, 'v')",
            "JSON.stringify(p)",
            "for (var k in p) {}",
            "Object.freeze(p)",
            "Reflect.ownKeys(p)",
            "({ ...p })",
            "p instanceof Object",
        ] {
            let mut it = realm();
            let r = out(&mut it, &format!("{build}{op}"));
            assert!(r.starts_with("limit:"), "{op}: {r}");
        }
        // `Array.isArray` follows the chain in a loop: it is still right at 10k (true for an array at the bottom, false for an object)
        let mut it = realm();
        assert_eq!(out(&mut it, "var t = []; var p = t; for (var i = 0; i < 10000; i++) p = new Proxy(p, {}); Array.isArray(p)"), "true");
        assert_eq!(out(&mut it, &format!("{build}Array.isArray(p)")), "false");
        // a chain within the cap works
        let mut it = realm();
        assert_eq!(out(&mut it, "var p = { v: 1 }; for (var i = 0; i < 100; i++) p = new Proxy(p, {}); p.v + ':' + Object.keys(p).join()"), "1:v");
        // the realm is still usable after a limit that came from inside a trap chain
        let mut it = realm();
        assert!(out(&mut it, &format!("{build}p.v")).starts_with("limit:"));
        assert_eq!(out(&mut it, "1 + 1"), "2");
    });
}

#[test]
fn a_10k_deep_chain_of_proxy_prototypes_is_a_limit() {
    big(|| {
        let build = "var o = { v: 1 }; for (var i = 0; i < 10000; i++) o = Object.create(new Proxy(o, {})); ";
        for op in ["o.v", "o.nope", "'v' in o", "o.w = 1"] {
            let mut it = realm();
            let r = out(&mut it, &format!("{build}{op}"));
            assert!(r.starts_with("limit:"), "{op}: {r}");
        }
        // `instanceof` and for-in follow the chain in a loop: they are still right at 10k
        let mut it = realm();
        assert_eq!(out(&mut it, &format!("{build}var n = 0; for (var k in o) n++; n")), "1");
        assert_eq!(out(&mut it, &format!("{build}o instanceof Object")), "true");
    });
}

#[test]
fn trap_recursion_is_a_limit() {
    big(|| {
        let cases = [
            "var p = new Proxy({}, { get: function (t, k, r) { return r.x; } }); p.x",
            "var p = new Proxy({}, { set: function (t, k, v, r) { r.x = v; return true; } }); p.x = 1",
            "var p = new Proxy({}, { has: function (t, k) { return k in p; } }); 'x' in p",
            "var p = new Proxy({}, { ownKeys: function (t) { return Reflect.ownKeys(p); } }); Object.keys(p)",
            "var p = new Proxy({}, { getPrototypeOf: function (t) { return Object.getPrototypeOf(p); } }); Object.getPrototypeOf(p)",
            "var p = new Proxy(function () {}, { apply: function (t, th, a) { return p(); } }); p()",
            "var p = new Proxy(function () {}, { construct: function (t, a) { return new p(); } }); new p()",
            "var p = new Proxy({}, { deleteProperty: function (t, k) { return delete p[k]; } }); delete p.x",
            "var p = new Proxy({}, { defineProperty: function (t, k, d) { return Reflect.defineProperty(p, k, d); } }); Object.defineProperty(p, 'x', {})",
            "var p = new Proxy({}, { getOwnPropertyDescriptor: function (t, k) { return Reflect.getOwnPropertyDescriptor(p, k); } }); Object.getOwnPropertyDescriptor(p, 'x')",
            // the handler is the proxy: every trap lookup is a `get` on it
            "var h = new Proxy({}, { get: function (t, k) { return p.zzz; } }); var p = new Proxy({}, h); p.x",
            // a cycle: the proxy is its own prototype's proxy
            "var p = new Proxy({}, { get: function (t, k, r) { return Object.create(p)[k]; } }); p.x",
        ];
        for src in cases {
            let mut it = realm();
            let r = out(&mut it, src);
            assert!(r.starts_with("limit:"), "{src}: {r}");
            // the realm still works
            assert_eq!(out(&mut it, "typeof new Proxy({}, {})"), "object");
        }
    });
}

#[test]
fn a_runaway_trap_hits_the_step_limit() {
    big(|| {
        let mut it = realm_with(100_000);
        let r = out(&mut it, "var p = new Proxy({}, { get: function () { while (true) {} } }); p.x");
        assert_eq!(r, "limit:Steps");
        let mut it = realm_with(200_000);
        let r = out(&mut it, "var p = new Proxy({}, { ownKeys: function () { for (;;) {} } }); Object.keys(p)");
        assert_eq!(r, "limit:Steps");
        // a trap that throws is caught by script; a trap that hits a limit is not
        let mut it = realm_with(100_000);
        let r = out(&mut it, "var p = new Proxy({}, { get: function () { while (true) {} } }); try { p.x } catch (e) { 'caught' }");
        assert_eq!(r, "limit:Steps");
    });
}

#[test]
fn a_huge_own_keys_result_is_capped() {
    big(|| {
        // the trap answers a 2-million-element array-like: the result list has a cap, so this is an error or a limit, never a hang
        let mut it = realm();
        let r = out(&mut it, "Reflect.ownKeys(new Proxy({}, { ownKeys: function () { return { length: 4000000 }; } })).length");
        assert!(r.starts_with("limit:") || r.starts_with("throw:") || r.starts_with("unsupported:"), "{r}");
    });
}

#[test]
fn proxies_handlers_and_revokers_survive_a_collection() {
    big(|| {
        let mut it = realm();
        let reader = it
            .run_script(
                "var rv = Proxy.revocable({ a: 1 }, { get: function (t, k, r) { return Reflect.get(t, k, r); } });
                 var plain = new Proxy({ b: 2 }, { has: function () { return true; } });
                 (function () { return rv.proxy.a + ':' + plain.b + ':' + ('zz' in plain); })",
            )
            .unwrap();
        // the target and the handler are reachable only through the proxy
        it.collect(&[reader]).unwrap();
        let r = it.call(reader, Value::Undefined, &[]);
        assert_eq!(show(&mut it, r), "1:2:true");
        it.collect(&[]).unwrap();
        assert_eq!(out(&mut it, "rv.proxy.a"), "1");
        // revoke after a collection, then collect again: the proxy stays revoked
        assert_eq!(out(&mut it, "rv.revoke(); try { rv.proxy.a } catch (e) { e.name }"), "TypeError");
        it.collect(&[]).unwrap();
        assert_eq!(out(&mut it, "try { rv.proxy.a } catch (e) { e.name }"), "TypeError");
        assert_eq!(out(&mut it, "typeof rv.proxy"), "object");
        // a callable proxy keeps working after a collection
        out(&mut it, "var fp = new Proxy(function (x) { return x * 2; }, { apply: function (t, th, a) { return t(a[0]) + 1; } });");
        it.collect(&[]).unwrap();
        assert_eq!(out(&mut it, "fp(4) + ':' + new Proxy(function () {}, {}).name"), "9:");
    });
}

#[test]
fn a_revoked_proxy_frees_its_target_and_handler() {
    big(|| {
        let mut it = realm();
        out(&mut it, "var rv; (function () { rv = Proxy.revocable({ big: [1, 2, 3, { x: 1 }] }, { get: function () { return 1; } }); })();");
        it.collect(&[]).unwrap();
        let live = it.heap.stats().live_objects;
        assert_eq!(out(&mut it, "rv.revoke()"), "undefined");
        it.collect(&[]).unwrap();
        // the target (with its array and element) and the handler (with its trap function and its prototype chain link) are gone
        assert!(it.heap.stats().live_objects < live, "{} !< {live}", it.heap.stats().live_objects);
    });
}

#[test]
fn unreachable_proxies_are_collected_with_their_parts() {
    big(|| {
        let mut it = realm();
        it.collect(&[]).unwrap();
        let base = it.heap.stats().live_objects;
        out(&mut it, "for (var i = 0; i < 1000; i++) new Proxy({ a: i }, { get: function () { return 1; } }); 0;");
        it.collect(&[]).unwrap();
        assert_eq!(it.heap.stats().live_objects, base);
        out(&mut it, "for (var i = 0; i < 100; i++) Proxy.revocable({}, {}).revoke; 0;");
        it.collect(&[]).unwrap();
        assert_eq!(it.heap.stats().live_objects, base);
    });
}

#[test]
fn proxy_does_not_change_the_plain_object_paths() {
    big(|| {
        let mut it = realm();
        // a plain object, an array and a function are not proxies and never answer as one
        assert_eq!(out(&mut it, "[{}, [], function () {}, new Map(), new Uint8Array(1)].map(function (o) { return Object.keys(o).length + typeof o; }).join()"), "0object,0object,0function,0object,1object");
        assert_eq!(out(&mut it, "var o = { a: 1 }; o.b = 2; delete o.a; Object.keys(o).join() + ('b' in o) + JSON.stringify(o)"), "btrue{\"b\":2}");
    });
}

#[test]
fn builtin_surface_of_proxy() {
    big(|| {
        let mut it = realm();
        assert_eq!(out(&mut it, "typeof Proxy"), "function");
        assert_eq!(out(&mut it, "Reflect.ownKeys(Proxy).join()"), "length,name,revocable");
        assert_eq!(out(&mut it, "var d = Object.getOwnPropertyDescriptor(globalThis, 'Proxy'); [d.writable, d.enumerable, d.configurable].join()"), "true,false,true");
        assert_eq!(out(&mut it, "var d = Object.getOwnPropertyDescriptor(Proxy, 'revocable'); [d.writable, d.enumerable, d.configurable].join()"), "true,false,true");
        assert_eq!(out(&mut it, "Proxy.hasOwnProperty('prototype')"), "false");
        assert_eq!(out(&mut it, "Object.getPrototypeOf(Proxy) === Function.prototype"), "true");
        // `revocable` is not a constructor, and neither is the revoke function
        assert_eq!(out(&mut it, "try { new Proxy.revocable({}, {}) } catch (e) { e.name }"), "TypeError");
        assert_eq!(out(&mut it, "try { new (Proxy.revocable({}, {}).revoke)() } catch (e) { e.name }"), "TypeError");
        // a proxy has no own keys of its own
        assert_eq!(out(&mut it, "Reflect.ownKeys(new Proxy({}, {})).length"), "0");
    });
}

#[test]
fn paths_that_hold_a_proxy_but_cannot_reach_a_trap_are_not_wrong() {
    big(|| {
        // the typed-array and wrapper-object receivers: a proxy of one is a `TypeError` where Node has one, never a quiet wrong answer
        let mut it = realm();
        let r = out(&mut it, "new Proxy(new Uint8Array(2), {}).length");
        assert!(r.starts_with("throw:TypeError"), "{r}");
        let mut it = realm();
        let r = out(&mut it, "Number(new Proxy(new Number(5), {}))");
        assert!(r.starts_with("throw:TypeError"), "{r}");
    });
}

#[test]
fn a_proxy_as_an_observer_wrapper_keeps_checksums_equal() {
    big(|| {
        let mut it = realm();
        let src = "var plain = { n: 0 }, wrapped = new Proxy({ n: 0 }, { get: function (t, k, r) { return Reflect.get(t, k, r); }, set: function (t, k, v, r) { return Reflect.set(t, k, v, r); } });
                   for (var i = 0; i < 1000; i++) { plain.n += i; wrapped.n += i; }
                   plain.n + ':' + wrapped.n";
        assert_eq!(out(&mut it, src), "499500:499500");
    });
}
