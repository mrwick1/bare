//! Module tests: the host side is a map of file names to sources. `log` is a global array that the modules push to.

use super::*;
use crate::heap::Heap;
use std::collections::HashMap;

fn big<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(64 << 20).spawn(f).unwrap().join().unwrap()
}

fn load(it: &mut Interp<'static>, files: &HashMap<String, String>, name: &str, cache: &mut HashMap<String, u32>) -> Result<u32, Error> {
    if let Some(&m) = cache.get(name) {
        return Ok(m);
    }
    let src = files.get(name).unwrap_or_else(|| panic!("no file {name}"));
    let m = it.module_parse(src, name)?;
    cache.insert(name.to_string(), m);
    let deps = it.module_requests(m).iter().map(|r| load(it, files, r, cache)).collect::<Result<Vec<_>, _>>()?;
    it.module_set_requested(m, &deps);
    Ok(m)
}

fn files(list: &[(&str, &str)]) -> HashMap<String, String> {
    list.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect()
}

fn show(it: &mut Interp, e: Error) -> String {
    match e {
        Error::Throw(v) if matches!(v, Value::Object(_)) => format!("throw:{}:{}", it.get_str(v, "name"), it.get_str(v, "message")),
        Error::Throw(v) => format!("throw-value:{}", it.display(v)),
        e => format!("other:{e:?}"),
    }
}

/// Run the jobs and answer the `import()` calls from the files, until nothing is left.
fn pump(it: &mut Interp<'static>, files: &HashMap<String, String>, cache: &mut HashMap<String, u32>) -> Result<(), Error> {
    loop {
        it.drain_microtasks()?;
        let reqs = it.take_dynamic_imports();
        if reqs.is_empty() {
            return Ok(());
        }
        for r in reqs {
            match load(it, files, &r.specifier, cache) {
                Ok(m) => it.finish_dynamic_import(r.promise, m)?,
                Err(e) => it.reject_dynamic_import(r.promise, e)?,
            }
        }
    }
}

/// Load, link, evaluate the root, run the jobs, and say how it ended: `log | state`.
fn run(list: &[(&str, &str)], root: &str) -> String {
    let (list, root): (Vec<(String, String)>, String) = (list.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect(), root.to_string());
    big(move || {
        let files: HashMap<String, String> = list.into_iter().collect();
        let mut it = Interp::realm(Heap::default(), EvalLimits { max_steps: 2_000_000, ..Default::default() }).expect("realm");
        it.run_script("var log = [];").unwrap();
        let mut cache = HashMap::new();
        let end = (|| -> Result<String, Error> {
            let m = load(&mut it, &files, &root, &mut cache)?;
            it.module_link(m)?;
            let p = it.module_evaluate(m)?;
            pump(&mut it, &files, &mut cache)?;
            Ok(match it.promise_status(Value::Object(p)) {
                Some(PromiseStatus::Pending) => "pending".to_string(),
                Some(PromiseStatus::Fulfilled(_)) => "ok".to_string(),
                Some(PromiseStatus::Rejected(v)) => format!("rejected {}", show(&mut it, Error::Throw(v))),
                None => "?".to_string(),
            })
        })();
        let state = end.unwrap_or_else(|e| show(&mut it, e));
        let log = it.run_script("log.join(',')").unwrap();
        format!("{} | {}", it.display(log), state)
    })
}

#[test]
fn a_module_runs_once_with_strict_code_and_an_undefined_this() {
    assert_eq!(run(&[("a", "log.push(typeof this, (function(){ return this })() === undefined); log.push('a')")], "a"), "undefined,true,a | ok");
    assert_eq!(run(&[("a", "undeclared = 1;")], "a"), " | rejected throw:ReferenceError:undeclared is not defined");
}

#[test]
fn imports_read_named_default_and_renamed_exports() {
    let r = run(
        &[
            ("a", "import d, { x, y as z } from 'b'; log.push(d, x, z);"),
            ("b", "export default 'D'; export const x = 1; const y = 2; export { y };"),
        ],
        "a",
    );
    assert_eq!(r, "D,1,2 | ok");
    let r = run(&[("a", "import { 'a b' as ab, q } from 'b'; log.push(ab, q);"), ("b", "const v = 5; export { v as 'a b', v as q };")], "a");
    assert_eq!(r, "5,5 | ok");
}

#[test]
fn a_shared_module_runs_once_in_dependency_order() {
    let r = run(
        &[
            ("a", "import 'b'; import 'c'; log.push('a');"),
            ("b", "import 'd'; log.push('b');"),
            ("c", "import 'd'; log.push('c');"),
            ("d", "log.push('d');"),
        ],
        "a",
    );
    assert_eq!(r, "d,b,c,a | ok");
}

#[test]
fn bindings_are_live_and_imports_are_read_only() {
    let r = run(
        &[
            ("a", "import { n, inc, o } from 'b'; log.push(n); inc(); log.push(n); o.k = 2; log.push(o.k); try { n = 9 } catch (e) { log.push(e.name) }"),
            ("b", "export let n = 0; export function inc() { n++ } export const o = { k: 1 };"),
        ],
        "a",
    );
    assert_eq!(r, "0,1,2,TypeError | ok");
    // `typeof` and a closure see the live value too.
    let r = run(
        &[
            ("a", "import { n, set } from 'b'; const f = () => n; log.push(typeof n, f()); set(5); log.push(f(), n + 1);"),
            ("b", "export let n = 1; export function set(v) { n = v }"),
        ],
        "a",
    );
    assert_eq!(r, "number,1,5,6 | ok");
}

#[test]
fn a_cycle_sees_hoisted_functions_and_the_tdz_of_the_rest() {
    let r = run(
        &[
            ("a", "import { f, late } from 'b'; export function g() { return 'g' } export let v = 1; log.push(f()); log.push(late);"),
            ("b", "import { g, v } from 'a'; export function f() { return 'f' + g() } export const late = 'L'; log.push(g()); try { v } catch (e) { log.push(e.name) }"),
        ],
        "a",
    );
    assert_eq!(r, "g,ReferenceError,fg,L | ok");
    // The root of the cycle can be either module.
    let r = run(&[("b", "import 'a'; log.push('b')"), ("a", "import 'b'; log.push('a')")], "b");
    assert_eq!(r, "a,b | ok");
}

#[test]
fn star_exports_and_namespaces() {
    let files = [
        ("a", "import * as ns from 'b'; log.push(Object.keys(ns).join('/'), ns.x, ns.y, ns.z, ns.default, typeof ns.q); log.push(Object.prototype.toString.call(ns), Object.getPrototypeOf(ns) === null, Object.isExtensible(ns), 'x' in ns, 'q' in ns); try { 'use strict'; ns.x = 1 } catch (e) { log.push('threw') }"),
        ("b", "export * from 'c'; export * as inner from 'c'; export const x = 'X'; export default 'DEF';"),
        ("c", "export const y = 'Y'; export const z = 'Z'; export const x = 'cX'; export default 'cDEF';"),
    ];
    let r = run(&files, "a");
    assert_eq!(r, "default/inner/x/y/z,X,Y,Z,DEF,undefined,[object Module],true,false,true,false,threw | ok");
    let r = run(&[("a", "import { inner } from 'b'; log.push(inner.p, Object.keys(inner)); import * as s from 'c'; log.push(s === inner)"), ("b", "export * as inner from 'c'"), ("c", "export const p = 1")], "a");
    assert_eq!(r, "1,p,true | ok");
}

#[test]
fn an_ambiguous_star_name_is_left_out_and_cannot_be_imported() {
    let r = run(
        &[
            ("a", "import * as ns from 'b'; log.push(Object.keys(ns).join())"),
            ("b", "export * from 'c'; export * from 'd';"),
            ("c", "export const same = 1; export const diff = 1; export { s } from 'e';"),
            ("d", "export const diff = 2; export { s } from 'e';"),
            ("e", "export const s = 0;"),
        ],
        "a",
    );
    assert_eq!(r, "s,same | ok");
    let r = run(&[("a", "import { diff } from 'b';"), ("b", "export * from 'c'; export * from 'd';"), ("c", "export const diff = 1;"), ("d", "export const diff = 2;")], "a");
    assert_eq!(r, " | throw:SyntaxError:The requested module 'b' has an ambiguous export named 'diff'");
}

#[test]
fn a_missing_export_is_a_syntax_error_at_link_and_nothing_runs() {
    let r = run(&[("a", "import { nope } from 'b'; log.push('a');"), ("b", "log.push('b'); export const yes = 1;")], "a");
    assert_eq!(r, " | throw:SyntaxError:The requested module 'b' does not provide an export named 'nope'");
    let r = run(&[("a", "export { nope } from 'b';"), ("b", "export const yes = 1;")], "a");
    assert_eq!(r, " | throw:SyntaxError:The requested module 'b' does not provide an export named 'nope'");
    let r = run(&[("a", "import x from 'b';"), ("b", "export const yes = 1;")], "a");
    assert_eq!(r, " | throw:SyntaxError:The requested module 'b' does not provide an export named 'default'");
}

#[test]
fn default_exports_get_the_name_default() {
    let r = run(
        &[
            ("a", "import f from 'b'; import C from 'c'; import g, { h } from 'd'; import e from 'e'; import arrow from 'f'; log.push(f.name, C.name, g.name, h.name, e(), arrow.name, new C() instanceof C);"),
            ("b", "export default function () {}"),
            ("c", "export default class {}"),
            ("d", "export default function* () {}; export { named as h }; function named() {}"),
            ("e", "export default function hoisted() { return 'h' } "),
            ("f", "export default () => 1"),
        ],
        "a",
    );
    assert_eq!(r, "default,default,default,named,h,default,true | ok");
    // An anonymous default function declaration is not hoisted the way a named one is. The value is there after the statement ran.
    let r = run(&[("a", "import v from 'b'; log.push(v)"), ("b", "export default 40 + 2;")], "a");
    assert_eq!(r, "42 | ok");
}

#[test]
fn a_throwing_module_rejects_and_every_importer_sees_the_same_error() {
    let r = run(&[("a", "import 'b'; log.push('a')"), ("b", "log.push('b'); throw new RangeError('boom')")], "a");
    assert_eq!(r, "b | rejected throw:RangeError:boom");
}

#[test]
fn import_meta_has_the_url() {
    let r = run(&[("a", "log.push(import.meta.url, import.meta === import.meta, Object.getPrototypeOf(import.meta) === null); import 'b'"), ("b", "log.push(import.meta.url)")], "a");
    assert_eq!(r, "b,a,true,true | ok");
}

#[test]
fn top_level_await_orders_the_modules() {
    // `a` waits; `b` does not depend on it and runs at once; `c` depends on `a` and waits for it.
    let r = run(
        &[
            ("root", "import 'a'; import 'b'; import 'c'; log.push('root')"),
            ("a", "log.push('a1'); await 0; log.push('a2')"),
            ("b", "log.push('b')"),
            ("c", "import 'a'; log.push('c')"),
        ],
        "root",
    );
    assert_eq!(r, "a1,b,a2,c,root | ok");
    // A value awaited from a promise chain, and an export set after the await.
    let r = run(
        &[
            ("a", "import { v } from 'b'; log.push(v)"),
            ("b", "export let v = 'before'; await Promise.resolve().then(() => 1).then(() => { v = 'after' });"),
        ],
        "a",
    );
    assert_eq!(r, "after | ok");
    // The promise stays pending while a module waits for a promise that never settles.
    let r = run(&[("a", "await new Promise(() => {}); log.push('never')")], "a");
    assert_eq!(r, " | pending");
}

#[test]
fn a_rejected_top_level_await_rejects_the_graph() {
    let r = run(
        &[
            ("a", "import 'b'; import 'c'; log.push('a')"),
            ("b", "await Promise.reject(new TypeError('bad'))"),
            ("c", "log.push('c')"),
        ],
        "a",
    );
    assert_eq!(r, "c | rejected throw:TypeError:bad");
    // for await at the top level, and an await inside a block.
    let r = run(&[("a", "for await (const x of [Promise.resolve(1), 2]) { log.push(x) } { log.push(await 3) }")], "a");
    assert_eq!(r, "1,2,3 | ok");
}

#[test]
fn dynamic_import_asks_the_host_and_resolves_with_the_namespace() {
    big(|| {
        let files = files(&[("a", "export const v = 1; export default 'd';"), ("m", "log.push('m'); export const w = 2;")]);
        let mut it = Interp::realm(Heap::default(), EvalLimits { max_steps: 1_000_000, ..Default::default() }).expect("realm");
        it.run_script("var log = []; var got; import('a').then(ns => { got = ns.v + ns.default }); import('nope').catch(e => { log.push(e.name) });").unwrap();
        let reqs = it.take_dynamic_imports();
        assert_eq!(reqs.iter().map(|r| r.specifier.as_str()).collect::<Vec<_>>(), ["a", "nope"]);
        assert!(reqs.iter().all(|r| r.referrer.is_none()));
        let mut cache = HashMap::new();
        let m = load(&mut it, &files, "a", &mut cache).unwrap();
        it.finish_dynamic_import(reqs[0].promise, m).unwrap();
        it.fail_dynamic_import(reqs[1].promise, "Failed to fetch nope").unwrap();
        it.drain_microtasks().unwrap();
        let r = it.run_script("got + ',' + log.join()").unwrap();
        assert_eq!(it.display(r), "1d,TypeError");
        // Inside a module the referrer is the module. A bad specifier value rejects.
        let m2 = load(&mut it, &files, "m", &mut cache).unwrap();
        it.module_link(m2).unwrap();
        let _ = it.module_evaluate(m2).unwrap();
        it.drain_microtasks().unwrap();
        let r = it.run_script("log.join()").unwrap();
        assert_eq!(it.display(r), "TypeError,m");
    });
    big(|| {
        let mut it = Interp::realm(Heap::default(), EvalLimits::default()).expect("realm");
        let m = it.module_parse("import('x'); import({ toString() { throw new RangeError('s') } }).catch(e => { globalThis.err = e.name }); import('y', 5).catch(e => { globalThis.err2 = e.name });", "root.js").unwrap();
        it.module_link(m).unwrap();
        it.module_evaluate(m).unwrap();
        let reqs = it.take_dynamic_imports();
        assert_eq!(reqs.len(), 1);
        assert_eq!((reqs[0].specifier.as_str(), reqs[0].referrer), ("x", Some(m)));
        it.drain_microtasks().unwrap();
        let r = it.run_script("err + err2").unwrap();
        assert_eq!(it.display(r), "RangeErrorTypeError");
    });
}

#[test]
fn a_dynamic_import_of_a_failing_module_rejects() {
    big(|| {
        let files = files(&[("bad", "throw new RangeError('x')")]);
        let mut it = Interp::realm(Heap::default(), EvalLimits::default()).expect("realm");
        it.run_script("var r; import('bad').catch(e => { r = e.name })").unwrap();
        let reqs = it.take_dynamic_imports();
        let m = load(&mut it, &files, "bad", &mut HashMap::new()).unwrap();
        it.finish_dynamic_import(reqs[0].promise, m).unwrap();
        it.drain_microtasks().unwrap();
        let r = it.run_script("r").unwrap();
        assert_eq!(it.display(r), "RangeError");
    });
}

#[test]
fn module_state_survives_a_collection() {
    big(|| {
        let files = files(&[
            ("a", "import { get } from 'b'; globalThis.read = get; import * as ns from 'b'; globalThis.nsb = ns;"),
            ("b", "let secret = { n: 41 }; export function get() { return secret.n + 1 } export const k = [1, 2];"),
        ]);
        let mut it = Interp::realm(Heap::default(), EvalLimits::default()).expect("realm");
        let mut cache = HashMap::new();
        let m = load(&mut it, &files, "a", &mut cache).unwrap();
        it.module_link(m).unwrap();
        let p = it.module_evaluate(m).unwrap();
        it.drain_microtasks().unwrap();
        it.collect(&[Value::Object(p)]).unwrap();
        let r = it.run_script("read() + ',' + nsb.k.length + ',' + Object.keys(nsb)").unwrap();
        assert_eq!(it.display(r), "42,2,get,k");
    });
}

#[test]
fn a_classic_script_cannot_use_import_meta_or_declarations() {
    big(|| {
        let mut it = Interp::realm(Heap::default(), EvalLimits::default()).expect("realm");
        let r = it.run_script("import.meta");
        assert!(matches!(r, Err(Error::Throw(_))));
        let r = it.run_script("import a from 'x'");
        assert!(matches!(r, Err(Error::Throw(_))));
    });
}

#[test]
fn a_module_syntax_error_is_thrown_at_parse() {
    big(|| {
        let mut it = Interp::realm(Heap::default(), EvalLimits::default()).expect("realm");
        let e = it.module_parse("export { x };", "u").err().unwrap();
        assert!(matches!(e, Error::Throw(_)));
        let e = it.module_parse("let a; let a;", "u").err().unwrap();
        assert!(matches!(e, Error::Throw(_)));
    });
}

#[test]
fn closures_declared_in_a_module_keep_the_module_scope() {
    let r = run(
        &[
            ("a", "import { make } from 'b'; const c1 = make(), c2 = make(); c1(); c1(); log.push(c1(), c2());"),
            ("b", "let count = 0; const local = { v: 'm' }; export function make() { let mine = 0; return () => ++mine + count++ * 100 + local.v.length }"),
        ],
        "a",
    );
    assert_eq!(r, "204,302 | ok");
}

#[test]
fn a_late_import_joins_a_module_that_is_still_waiting() {
    // `b` waits for `gate`. `a` is imported while `b` waits, so it waits for `b` and fails after it.
    let r = run(
        &[
            ("root", "import { gate } from 'setup'; \
                import('b').finally(() => log.push('B')).catch(() => {}); \
                Promise.resolve().then(() => import('a')).finally(() => log.push('A')).catch(() => {}); \
                Promise.resolve().then(() => Promise.resolve()).then(() => Promise.resolve()).then(() => gate.reject('no'));"),
            ("setup", "export const gate = Promise.withResolvers();"),
            ("b", "import { gate } from 'setup'; log.push('b start'); await gate.promise;"),
            ("a", "import 'b'; log.push('a runs')"),
        ],
        "root",
    );
    assert_eq!(r, "b start,B,A | ok");
}
