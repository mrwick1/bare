//! Tests of modules (`module.rs`, `compile_module.rs`). A tiny host loads a graph from a table of sources: a specifier is the file name
//! with `./` cut off. Results go to `globalThis.out`.
use super::*;
use crate::interp::Vm;
use crate::script;
use bare_js_api::{Abort, Engine, Error};

fn text(r: &mut Realm, e: Error<'_>) -> String {
    match e {
        Error::Throw(l) => {
            let s = Engine::to_string(r, l).ok();
            s.and_then(|s| Engine::as_str(r, s).map(|t| t.to_string_lossy())).unwrap_or_default()
        }
        Error::Abort(Abort::Unsupported(w)) => format!("unsupported:{w}"),
        Error::Abort(a) => format!("{a:?}"),
    }
}

struct Host<'a> {
    files: &'a [(&'a str, &'a str)],
    done: Vec<(String, u32)>,
}

impl Host<'_> {
    fn load(&mut self, r: &mut Realm, url: &str) -> Result<u32, String> {
        if let Some((_, m)) = self.done.iter().find(|(u, _)| u == url) {
            return Ok(*m);
        }
        let Some((_, src)) = self.files.iter().find(|(u, _)| *u == url) else { return Err(format!("no file {url}")) };
        let m = Engine::module_parse(r, src, url).map_err(|e| text(r, e))?;
        self.done.push((url.to_owned(), m));
        let deps: Result<Vec<u32>, String> = Engine::module_requests(r, m).iter().map(|q| self.load(r, q.trim_start_matches("./"))).collect();
        Engine::module_set_requested(r, m, &deps?);
        Ok(m)
    }
}

/// Load the graph of `root`, link it and evaluate it. `Err` is the text of the first error (a parse, a link or an evaluation).
fn run_in(vm: &mut Vm, files: &[(&str, &str)], root: &str) -> Result<u32, String> {
    let r = &mut vm.realm;
    let mut h = Host { files, done: Vec::new() };
    let m = h.load(r, root)?;
    Engine::module_link(r, m).map_err(|e| text(r, e))?;
    let p = r.md_evaluate(m).map_err(|e| r.error_text(e))?;
    match Realm::promise_status(r, p) {
        Some((2, e)) => Err(r.error_text(e)),
        _ => Ok(m),
    }
}

fn run(files: &[(&str, &str)]) -> Result<Vm, String> {
    let mut vm = script::load("globalThis.out = [];").map_err(|e| e.to_string())?;
    vm.run_main(0).map_err(|e| format!("{e:?}"))?;
    run_in(&mut vm, files, files[0].0)?;
    Ok(vm)
}

fn out(files: &[(&str, &str)]) -> String {
    let mut vm = run(files).unwrap_or_else(|e| panic!("{e}"));
    vm.global_text("out").replace('"', "")
}

#[test]
fn a_module_imports_a_value_and_a_function() {
    let o = out(&[("a.js", "import {x, f} from './b.js'; out.push(x, f(2));"), ("b.js", "export const x = 7; export function f(n) { return n * 3 }")]);
    assert_eq!(o, "[7,6]");
}

#[test]
fn bindings_are_live() {
    let o = out(&[
        ("a.js", "import {x, inc} from './b.js'; out.push(x); inc(); out.push(x); inc(); out.push(x);"),
        ("b.js", "export let x = 1; export function inc() { x++ }"),
    ]);
    assert_eq!(o, "[1,2,3]");
}

#[test]
fn the_evaluation_order_is_depth_first() {
    let o = out(&[
        ("a.js", "import './b.js'; import './c.js'; out.push('a');"),
        ("b.js", "import './d.js'; out.push('b');"),
        ("c.js", "out.push('c');"),
        ("d.js", "out.push('d');"),
    ]);
    assert_eq!(o, "[d,b,c,a]");
}

#[test]
fn a_module_runs_once() {
    let o = out(&[("a.js", "import './b.js'; import './c.js'; out.push('a')"), ("b.js", "import './c.js'; out.push('b')"), ("c.js", "out.push('c')")]);
    assert_eq!(o, "[c,b,a]");
}

#[test]
fn default_exports() {
    let o = out(&[
        ("a.js", "import d, {e as g} from './b.js'; import K from './c.js'; import F from './d.js'; out.push(d, g, new K().v, F(), F.name, K.name);"),
        ("b.js", "export default 5; export const e = 'e';"),
        ("c.js", "export default class { constructor() { this.v = 'k' } }"),
        ("d.js", "export default function () { return 'f' }"),
    ]);
    assert_eq!(o, "[5,e,k,f,default,default]");
}

#[test]
fn a_namespace_object() {
    let o = out(&[
        ("a.js", "import * as ns from './b.js'; out.push(Object.keys(ns).join('|'), ns.x, ns.f(), Object.prototype.toString.call(ns), Object.getPrototypeOf(ns), Object.isExtensible(ns), typeof ns.default);"),
        ("b.js", "export let x = 1; export function f() { return x + 1 } export default 3; export {x as z};"),
    ]);
    assert_eq!(o, "[default|f|x|z,1,2,[object Module],null,false,number]");
}

#[test]
fn re_exports() {
    let o = out(&[
        ("a.js", "import {x, y, z, w, Ns, fn} from './b.js'; out.push(x, y, z, w, Ns.q, fn());"),
        ("b.js", "export {x, q as y} from './c.js'; export * from './d.js'; export * as Ns from './c.js'; export {z as w} from './d.js'; import {x as x2} from './c.js'; export function fn() { return x2 }"),
        ("c.js", "export let x = 'cx'; export let q = 'cq';"),
        ("d.js", "export let z = 'dz'; export const x = 'ignored';"),
    ]);
    assert_eq!(o, "[cx,cq,dz,dz,cq,cx]");
}

#[test]
fn an_import_that_is_exported_again_is_the_same_binding() {
    let o = out(&[
        ("a.js", "import {v, bump} from './b.js'; out.push(v); bump(); out.push(v);"),
        ("b.js", "import {v} from './c.js'; export {v}; export {bump} from './c.js';"),
        ("c.js", "export let v = 1; export function bump() { v += 10 }"),
    ]);
    assert_eq!(o, "[1,11]");
}

#[test]
fn a_cycle_can_call_a_hoisted_function() {
    let o = out(&[
        ("a.js", "import {g} from './b.js'; export function f() { return 'f' } out.push(g());"),
        ("b.js", "import {f} from './a.js'; export function g() { return f() + 'g' } out.push(f());"),
    ]);
    assert_eq!(o, "[f,fg]");
}

#[test]
fn a_cycle_reads_a_let_in_its_dead_zone() {
    let e = run(&[("a.js", "import {x} from './b.js'; export let y = 1;"), ("b.js", "import {y} from './a.js'; export let x = y;")]).err().unwrap();
    assert!(e.starts_with("ReferenceError"), "{e}");
}

#[test]
fn an_import_cannot_be_assigned() {
    let e = run(&[("a.js", "import {x} from './b.js'; x = 2;"), ("b.js", "export let x = 1;")]).err().unwrap();
    assert!(e.starts_with("TypeError"), "{e}");
}

#[test]
fn a_missing_export_is_a_syntax_error_at_link() {
    let e = run(&[("a.js", "out.push('no'); import {nope} from './b.js';"), ("b.js", "export let x = 1;")]).err().unwrap();
    assert!(e.starts_with("SyntaxError"), "{e}");
    let mut vm = script::load("globalThis.out = [];").unwrap();
    vm.run_main(0).unwrap();
    let _ = run_in(&mut vm, &[("a.js", "out.push('no'); import {nope} from './b.js';"), ("b.js", "export let x = 1;")], "a.js");
    assert_eq!(vm.global_text("out"), "[]", "nothing ran");
}

#[test]
fn conflicting_star_exports_are_ambiguous() {
    let e = run(&[("a.js", "import {x} from './b.js';"), ("b.js", "export * from './c.js'; export * from './d.js';"), ("c.js", "export let x = 1"), ("d.js", "export let x = 2")]).err().unwrap();
    assert!(e.starts_with("SyntaxError"), "{e}");
}

#[test]
fn an_evaluation_error_is_kept_and_thrown_again() {
    let mut vm = script::load("globalThis.out = [];").unwrap();
    vm.run_main(0).unwrap();
    let files = [("a.js", "import './b.js'; out.push('a')"), ("b.js", "out.push('b'); throw new RangeError('boom');")];
    let mut h = Host { files: &files, done: Vec::new() };
    let a = h.load(&mut vm.realm, "a.js").unwrap();
    Engine::module_link(&mut vm.realm, a).unwrap();
    for m in [a, h.done[1].1, a] {
        let p = vm.realm.md_evaluate(m).unwrap();
        let Some((2, e)) = Realm::promise_status(&vm.realm, p) else { panic!("not rejected") };
        assert_eq!(vm.realm.error_text(e), "RangeError: boom");
    }
    assert_eq!(vm.global_text("out"), "[\"b\"]");
}

#[test]
fn this_is_undefined_and_the_code_is_strict() {
    let o = out(&[("a.js", "out.push(this === undefined, (function () { return this })() === undefined, (() => this)() === undefined);")]);
    assert_eq!(o, "[true,true,true]");
    let e = run(&[("a.js", "undeclared = 1;")]).err().unwrap();
    assert!(e.starts_with("ReferenceError"), "{e}");
}

#[test]
fn import_meta_has_the_url() {
    let o = out(&[("dir/a.js", "out.push(import.meta.url, import.meta === import.meta); function f() { return import.meta.url } out.push(f());")]);
    assert_eq!(o, "[dir/a.js,true,dir/a.js]");
}

#[test]
fn closures_keep_module_bindings_and_each_module_has_its_own() {
    let o = out(&[
        ("a.js", "import {count, next} from './b.js'; import {count as c2, next as n2} from './c.js'; next(); next(); n2(); out.push(count, c2); let local = 1; const g = () => local++; g(); g(); out.push(local);"),
        ("b.js", "export let count = 0; export const next = () => ++count;"),
        ("c.js", "export let count = 100; export function next() { count++ }"),
    ]);
    assert_eq!(o, "[2,101,3]");
}

#[test]
fn exports_of_all_kinds() {
    let o = out(&[
        ("a.js", "import * as m from './b.js'; out.push(Object.keys(m).join());"),
        ("b.js", "export var v = 1; export let l = 2; export const c = 3; export function f() {} export class K {} export {v as alias, v as alias2}; export {f as 'a string'}; export default 1;"),
    ]);
    assert_eq!(o, "[K,a string,alias,alias2,c,default,f,l,v]");
}

#[test]
fn top_level_await_is_unsupported() {
    let e = run(&[("a.js", "await 1;")]).err().unwrap();
    assert_eq!(e, "unsupported:top-level await");
}

#[test]
fn a_syntax_error_is_thrown_by_the_parse() {
    let e = run(&[("a.js", "export let a = ;")]).err().unwrap();
    assert!(e.starts_with("SyntaxError"), "{e}");
}

#[test]
fn destructuring_loops_and_classes_at_the_top_level() {
    let o = out(&[
        ("a.js", "import {a, b} from './b.js'; for (let i = 0; i < 3; i++) out.push(i * a); let {p, q: [r]} = {p: 1, q: [2]}; out.push(p, r, b.length); class C extends b.K { m() { return a } } out.push(new C().m());"),
        ("b.js", "export const a = 2; export const b = {length: 4, K: class {}};"),
    ]);
    assert_eq!(o, "[0,2,4,1,2,4,2]");
}

#[test]
fn a_var_and_a_function_in_a_block_of_a_module() {
    let o = out(&[("a.js", "{ var v = 1; function f() { return 2 } out.push(f()) } out.push(v, typeof f);")]);
    assert_eq!(o, "[2,1,undefined]");
}

/// Serve the `import()` calls that wait: load the graph, then finish (or reject or fail). Runs the jobs after each step.
fn serve(vm: &mut Vm, files: &[(&str, &str)], h: &mut Host) {
    loop {
        let reqs = Engine::take_dynamic_imports(&mut vm.realm);
        if reqs.is_empty() {
            break;
        }
        for q in reqs {
            let url = q.specifier.trim_start_matches("./").to_owned();
            match h.load(&mut vm.realm, &url) {
                Ok(m) => Engine::finish_dynamic_import(&mut vm.realm, &q.promise, m).unwrap_or_else(|_| panic!("finish")),
                Err(msg) if msg.starts_with("no file") => Engine::fail_dynamic_import(&mut vm.realm, &q.promise, &msg).unwrap_or_else(|_| panic!("fail")),
                Err(msg) => {
                    let e = vm.realm.error(ErrKind::Syntax, &msg);
                    let l = Engine::raw_local(&mut vm.realm, &e);
                    Engine::reject_dynamic_import(&mut vm.realm, &q.promise, l).unwrap_or_else(|_| panic!("reject"));
                }
            }
            let _ = files;
        }
        vm.drain_microtasks(&mut |_, _| {}).unwrap();
    }
}

fn run_script_with_imports(script_src: &str, files: &[(&str, &str)]) -> String {
    let mut vm = script::load("globalThis.out = [];").unwrap();
    vm.run_main(0).unwrap();
    vm.realm.run_text(script_src).unwrap_or_else(|_| panic!("script"));
    let mut h = Host { files, done: Vec::new() };
    serve(&mut vm, files, &mut h);
    vm.drain_microtasks(&mut |_, _| {}).unwrap();
    vm.global_text("out").replace('"', "")
}

#[test]
fn import_in_a_classic_script() {
    let o = run_script_with_imports(
        "import('./b.js').then(ns => out.push(ns.x, ns.f())); out.push('sync');",
        &[("b.js", "export let x = 'bx'; export function f() { return 'bf' } out.push('b ran');")],
    );
    assert_eq!(o, "[sync,b ran,bx,bf]");
}

#[test]
fn import_twice_gives_the_same_namespace() {
    let o = run_script_with_imports(
        "import('./b.js').then(a => import('./b.js').then(b => out.push(a === b, a.x)));",
        &[("b.js", "export let x = 1;")],
    );
    assert_eq!(o, "[true,1]");
}

#[test]
fn import_of_a_missing_file_rejects_with_a_type_error() {
    let o = run_script_with_imports("import('./nope.js').catch(e => out.push(e instanceof TypeError))", &[]);
    assert_eq!(o, "[true]");
}

#[test]
fn import_of_a_bad_module_rejects() {
    let o = run_script_with_imports(
        "import('./b.js').catch(e => out.push(e.name)); import('./c.js').catch(e => out.push(e.message));",
        &[("b.js", "export let = ;"), ("c.js", "throw new Error('c failed')")],
    );
    assert_eq!(o, "[SyntaxError,c failed]");
}

#[test]
fn import_from_a_module_names_its_referrer() {
    let mut vm = script::load("globalThis.out = [];").unwrap();
    vm.run_main(0).unwrap();
    let files = [("a.js", "import('./b.js').then(ns => out.push(ns.v)); out.push('a')"), ("b.js", "export const v = 'bv'")];
    let mut h = Host { files: &files, done: Vec::new() };
    let a = h.load(&mut vm.realm, "a.js").unwrap();
    Engine::module_link(&mut vm.realm, a).unwrap();
    vm.realm.md_evaluate(a).unwrap();
    let reqs = Engine::take_dynamic_imports(&mut vm.realm);
    assert_eq!(reqs.len(), 1);
    assert_eq!(reqs[0].referrer, Some(a));
    assert_eq!(reqs[0].specifier, "./b.js");
    let m = h.load(&mut vm.realm, "b.js").unwrap();
    Engine::finish_dynamic_import(&mut vm.realm, &reqs[0].promise, m).unwrap_or_else(|_| panic!("finish"));
    vm.drain_microtasks(&mut |_, _| {}).unwrap();
    assert_eq!(vm.global_text("out").replace('"', ""), "[a,bv]");
}

#[test]
fn import_of_an_object_with_to_string_and_a_throwing_one() {
    let o = run_script_with_imports(
        "import({ toString() { return './b.js' } }).then(ns => out.push(ns.x)); import({ toString() { throw new RangeError('no') } }).catch(e => out.push(e.name))",
        &[("b.js", "export let x = 1;")],
    );
    assert_eq!(o, "[RangeError,1]");
}

#[test]
fn namespace_properties_cannot_be_deleted() {
    let o = out(&[
        ("a.js", "import * as ns from './b.js'; try { delete ns.x } catch (e) { out.push(e.constructor === TypeError) } out.push(delete ns.nope, Object.getOwnPropertyDescriptor(ns, 'x').configurable)"),
        ("b.js", "export let x = 1;"),
    ]);
    assert_eq!(o, "[true,true,false]");
}

#[test]
fn modules_survive_collections() {
    let files = [
        ("a.js", "import {make, count} from './b.js'; import * as ns from './b.js'; for (let i = 0; i < 300; i++) make(i); out.push(count(), ns.count(), ns.list.length)"),
        ("b.js", "export const list = []; export function make(i) { list.push({ i, s: 'x' + i }) } export function count() { let a = []; for (let i = 0; i < 2000; i++) a.push([i]); return list.length }"),
    ];
    let mut vm = script::load("globalThis.out = [];").unwrap();
    vm.run_main(0).unwrap();
    vm.realm.heap.set_trigger(crate::heap::Trigger { min: 1 << 10, slack: 1 << 10 });
    run_in(&mut vm, &files, "a.js").unwrap();
    vm.collect_idle();
    assert_eq!(vm.global_text("out").replace('"', ""), "[300,300,300]");
}
