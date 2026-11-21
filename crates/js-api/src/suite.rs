//! The tests that every engine must pass (`docs/design-js2-host-api.md`, section 5). Feature `suite`.
//!
//! An engine makes a [`Harness`]: a realm, a way to run script text and a way to collect. Each function below is one test. It uses
//! only the interface, so the same function runs on `bare-js` and on `bare-js2` (`tests/engines.rs`). A test that needs an engine
//! detail stays in the crate of that engine.
//!
//! Scripts are written so that the value of the last statement is an expression.

use crate::{Args, Attrs, Engine, Error, Host, HostFn, Intrinsic, Local, Persistent, PromiseState, Scope, Str, Type, enter, task};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

/// What an engine gives the suite.
pub trait Harness: Sized {
    type E: Engine;
    /// A realm with a budget of about 1,000,000 steps for each task. With `stress` the engine collects as often as it can (a tiny
    /// trigger); an engine that collects only between tasks collects after each [`Harness::eval`].
    fn new(stress: bool) -> Self;
    fn engine(&mut self) -> &mut Self::E;
    /// Run `src` as a script. The answer is the engine's text of the value of its last expression statement, `throw:` and the text of
    /// a thrown value, `limit` for a stop by a limit, or `unsupported:` and the name of a method that the engine lacks.
    fn eval(&mut self, src: &str) -> String;
    /// Collect now, with no script running (between tasks).
    fn collect(&mut self);
}

const NAMES: [&str; 31] = [
    "add", "callback", "fail", "keep", "kept", "describe", "typeErr", "rangeErr", "charge", "num", "str", "bool", "arr", "isArr", "protoOf", "setProto",
    "accessor", "Point", "PointNT", "utf16", "hasOwn", "ownKeys", "ownData", "isCallable", "bind", "echoAfter", "thisOf", "intrinsic", "isNew", "runJobs", "queue",
];

/// The host of the suite: function `i` of `NAMES` is host function `i`.
#[derive(Default)]
struct Suite {
    kept: RefCell<Vec<Persistent>>,
    /// The calls of `between_jobs`, and the call that fails (0: none).
    between: Cell<usize>,
    fail_between_at: Cell<usize>,
    /// The texts of the values that `report_exception` and `promise_rejections` got.
    reported: RefCell<Vec<String>>,
    rejections: RefCell<Vec<String>>,
}

/// The text of a string or a number, for the logs of the hooks.
fn show<'s, E: Engine>(s: &mut Scope<'s, E>, v: Local<'s>) -> String {
    match (s.as_str(v), s.as_number(v)) {
        (Some(t), _) => t.to_string_lossy(),
        (_, Some(n)) => n.to_string(),
        _ => "?".to_string(),
    }
}

impl<E: Engine> Host<E> for Suite {
    fn call<'s>(&self, s: &mut Scope<'s, E>, id: u32, this: Local<'s>, args: Args<'s>, is_new: bool) -> Result<Local<'s>, Error<'s>> {
        let arg = |s: &mut Scope<'s, E>, i| match args.get(i) {
            Some(v) => v,
            None => s.undefined(),
        };
        let (a, b, c, d) = (arg(s, 0), arg(s, 1), arg(s, 2), arg(s, 3));
        match id {
            0 => {
                let n = s.as_number(a).unwrap_or(f64::NAN) + s.as_number(b).unwrap_or(f64::NAN);
                Ok(s.number(n))
            }
            // callback(f, x): f(x), called by the host
            1 => {
                let this = s.undefined();
                s.call(a, this, &[b])
            }
            2 => Err(Error::Throw(a)),
            3 => {
                let p = s.persist(a);
                self.kept.borrow_mut().push(p);
                Ok(s.undefined())
            }
            4 => {
                let i = s.as_number(a).unwrap_or(0.0) as usize;
                let kept = self.kept.borrow();
                Ok(s.local(&kept[i]))
            }
            // describe(o): "<o.name>:<typeof o.f>", built in an inner scope
            5 => s.escape_scope(|s| {
                let name = s.get_str(a, "name")?;
                let name = s.as_str(name).map(|t| t.to_string_lossy()).unwrap_or_default();
                let f = s.get_str(a, "f")?;
                let t = format!("{name}:{:?}", s.type_of(f));
                s.string(&t)
            }),
            6 | 7 => {
                let msg = s.as_str(a).map(|t| t.to_string_lossy()).unwrap_or_default();
                Err(if id == 6 { s.type_error(&msg) } else { s.range_error(&msg) })
            }
            8 => {
                let n = s.as_number(a).unwrap_or(0.0) as u64;
                s.charge(n)?;
                Ok(s.undefined())
            }
            9 => {
                let n = s.to_number(a)?;
                Ok(s.number(n))
            }
            10 => s.to_string(a),
            11 => {
                let t = s.to_boolean(a);
                Ok(s.boolean(t))
            }
            12 => {
                let items: Vec<_> = args.iter().collect();
                s.new_array(&items)
            }
            13 => {
                let t = s.is_array(a)?;
                Ok(s.boolean(t))
            }
            14 => match s.get_prototype_of(a)? {
                Some(p) => Ok(p),
                None => Ok(s.null()),
            },
            15 => {
                let p = if s.type_of(b) == Type::Null { None } else { Some(b) };
                let t = s.set_prototype_of(a, p)?;
                Ok(s.boolean(t))
            }
            16 => {
                let opt = |s: &Scope<'s, E>, v| if s.type_of(v) == Type::Undefined { None } else { Some(v) };
                let (g, st) = (opt(s, c), opt(s, d));
                s.define_accessor(a, b, g, st, Attrs { writable: false, enumerable: false, configurable: true })?;
                Ok(s.undefined())
            }
            // The constructors: `new Point(x)` makes `{ x }`. `Point` takes the prototype of `Point.prototype`, `PointNT` the one of `new.target`.
            17 | 18 => {
                if !is_new {
                    return s.string("called");
                }
                let g = s.global();
                let f = s.get_str(g, if id == 17 { "Point" } else { "PointNT" })?;
                let default = s.get_str(f, "prototype")?;
                let proto = if id == 17 { default } else { s.proto_from_new_target(default)? };
                let o = s.new_object()?;
                s.set_prototype_of(o, Some(proto))?;
                s.define_str(o, "x", a, Attrs::ALL)?;
                Ok(o)
            }
            19 => s.string_utf16(&[0xD800, u16::from(b'a')]),
            20 => {
                let t = s.has_own(a, b)?;
                Ok(s.boolean(t))
            }
            21 => {
                let keys = s.own_keys(a)?;
                s.new_array(&keys)
            }
            22 => match s.own_enumerable_data(a, b)? {
                Some(v) => Ok(v),
                None => s.string("<none>"),
            },
            23 => {
                let t = s.is_callable(a);
                Ok(s.boolean(t))
            }
            24 => s.bind(a, b),
            // echoAfter(f, x): call `f`, then give `x` back. The host reads its argument after a call into script.
            25 => {
                let this = s.undefined();
                s.call(a, this, &[])?;
                Ok(b)
            }
            26 => Ok(this),
            27 => {
                let which = [Intrinsic::ObjectPrototype, Intrinsic::ArrayPrototype, Intrinsic::ErrorPrototype][s.as_number(a).unwrap_or(0.0) as usize];
                Ok(s.intrinsic(which))
            }
            // runJobs(f): queue `f` as a microtask, run a checkpoint from the host and give the number of jobs that ran.
            29 => {
                s.queue_microtask(a)?;
                let n = s.checkpoint()?;
                Ok(s.number(n as f64))
            }
            // queue(f): queue `f` as a microtask (the old engine has no global `queueMicrotask`).
            30 => {
                s.queue_microtask(a)?;
                Ok(s.undefined())
            }
            _ => Ok(s.boolean(is_new)),
        }
    }

    fn between_jobs<'s>(&self, s: &mut Scope<'s, E>) -> Result<(), Error<'s>> {
        self.between.set(self.between.get() + 1);
        if self.between.get() == self.fail_between_at.get() {
            return Err(s.type_error("between"));
        }
        Ok(())
    }

    fn report_exception<'s>(&self, s: &mut Scope<'s, E>, thrown: Local<'s>) {
        let t = show(s, thrown);
        self.reported.borrow_mut().push(t);
    }

    fn promise_rejections<'s>(&self, s: &mut Scope<'s, E>, unhandled: &[Local<'s>], handled: &[Local<'s>], dropped: u32) -> Result<(), Error<'s>> {
        for &p in unhandled {
            let t = match s.promise_status(p) {
                Some(PromiseState::Rejected(v)) => show(s, v),
                _ => "not rejected".to_string(),
            };
            self.rejections.borrow_mut().push(format!("unhandled:{t}"));
        }
        for _ in handled {
            self.rejections.borrow_mut().push("handled".to_string());
        }
        if dropped > 0 {
            self.rejections.borrow_mut().push(format!("dropped:{dropped}"));
        }
        Ok(())
    }
}

fn setup<H: Harness>(stress: bool) -> (H, Rc<Suite>) {
    let mut h = H::new(stress);
    let host = Rc::new(Suite::default());
    enter(h.engine(), |s| {
        s.set_host(host.clone());
        let g = s.global();
        for (i, name) in NAMES.into_iter().enumerate() {
            let kind = match name {
                "Point" => HostFn::Ctor,
                "PointNT" => HostFn::CtorNewTarget,
                _ => HostFn::Method,
            };
            let f = s.host_function(name, i as u32, 1, kind).unwrap();
            s.define_str(g, name, f, Attrs::METHOD).unwrap();
            if kind != HostFn::Method {
                let proto = s.new_object().unwrap();
                s.define_str(f, "prototype", proto, Attrs { writable: false, enumerable: false, configurable: false }).unwrap();
            }
        }
    });
    (h, host)
}

fn depth<H: Harness>(h: &mut H) -> u32 {
    h.engine().stack_len()
}

pub fn host_functions_run_in_scripts<H: Harness>(stress: bool) {
    let (mut h, _) = setup::<H>(stress);
    assert_eq!(h.eval("add(2, 3)"), "5");
    assert_eq!(h.eval("callback(x => x * 10, 4)"), "40");
    assert_eq!(h.eval("(function () { try { fail('no') } catch (e) { return 'caught ' + e } })()"), "caught no");
    assert_eq!(h.eval("describe({ name: 'box', f() {} })"), "box:Function");
    assert_eq!(h.eval("var o = { m: thisOf }; o.m() === o"), "true");
    assert_eq!(depth(&mut h), 0, "every call gives its slots back");
}

pub fn an_engine_error_crosses_the_host_unchanged<H: Harness>(stress: bool) {
    let (mut h, _) = setup::<H>(stress);
    assert_eq!(h.eval("callback(() => { for (;;) {} }, 0)"), "limit");
    assert_eq!(h.eval("(function () { try { callback(() => { for (;;) {} }, 0) } catch (e) { return 'caught' } })()"), "limit", "a script cannot catch a limit");
    assert_eq!(h.eval("add(1, 1)"), "2", "the next run has a new budget");
    assert_eq!(depth(&mut h), 0);
}

pub fn a_persistent_handle_keeps_its_value_through_a_collection<H: Harness>(stress: bool) {
    let (mut h, host) = setup::<H>(stress);
    h.eval("keep({ tag: 'kept' }); var junk = []; for (var i = 0; i < 100; i++) junk.push({}); junk = null; 0");
    h.collect();
    assert_eq!(h.eval("kept(0).tag"), "kept");
    let live = |h: &mut H| {
        let mut n = 0;
        h.engine().slots().for_each(|_| n += 1);
        n
    };
    assert_eq!(live(&mut h), 1);
    drop(host.kept.borrow_mut().pop());
    assert_eq!(live(&mut h), 0, "a dropped handle is no root");
    h.eval("keep(1)");
    assert_eq!(live(&mut h), 1);
    assert_eq!(h.eval("kept(0)"), "1", "the freed slot is used again");
}

pub fn values_objects_and_strings_through_a_scope<H: Harness>(stress: bool) {
    let (mut h, _) = setup::<H>(stress);
    // A host operation outside a script is one task (`Engine::task`): the old engine counts its steps there.
    task(h.engine(), |s| {
        let o = s.new_object().unwrap();
        let (k, v) = (s.string("x").unwrap(), s.number(1.5));
        s.set(o, k, v).unwrap();
        let got = s.get(o, k).unwrap();
        assert_eq!(s.as_number(got), Some(1.5));
        let t = s.string("héllo").unwrap();
        assert_eq!(s.as_str(t).unwrap().to_string_lossy(), "héllo");
        assert_eq!(s.type_of(t), Type::String);
        let n = s.null();
        assert_eq!(s.type_of(n), Type::Null);
        assert!(matches!(s.get(n, k), Err(Error::Throw(_))), "null.x throws");
        let e = s.boolean(true);
        assert_eq!(s.as_bool(e), Some(true));
        assert!(matches!(s.define(e, k, v, Attrs::ALL), Err(Error::Throw(_))));
        // A value made in an inner scope reaches the outer one only through `escape_scope`.
        let kept = s.escape_scope(|s| {
            let a = s.string("in")?;
            let b = s.string("ner")?;
            let o = s.new_array(&[a, b])?;
            s.get_str(o, "length")
        });
        let kept = kept.unwrap();
        assert_eq!(s.as_number(kept), Some(2.0));
    })
    .unwrap();
    assert_eq!(depth(&mut h), 0);
}

pub fn a_window_is_read_after_calls_into_script<H: Harness>(stress: bool) {
    let (mut h, _) = setup::<H>(stress);
    assert_eq!(h.eval("echoAfter(function () { return add(1, 2) }, 'kept')"), "kept");
    assert_eq!(h.eval("callback(function (x) { return echoAfter(function () { return [x, x, x].length }, x + 1) }, 5)"), "6");
    assert_eq!(h.eval("callback(function (o) { return echoAfter(function () { for (var i = 0; i < 2000; i++) o = { n: i, s: 'n' + i }; return o }, o.tag) }, { tag: 't' })"), "t");
    assert_eq!(depth(&mut h), 0);
}

pub fn constructors_take_new_target_as_declared<H: Harness>(stress: bool) {
    let (mut h, _) = setup::<H>(stress);
    assert_eq!(h.eval("new Point(3).x"), "3");
    assert_eq!(h.eval("Point(3)"), "called");
    assert_eq!(h.eval("new Point(3) instanceof Point"), "true");
    // `Ctor` refuses `class extends`: the new engine throws a `TypeError`, the old one stops the script as unsupported.
    let refused = h.eval("(function () { class B extends Point {} try { new B(1); return 'allowed' } catch (e) { return e.name } })()");
    assert!(refused == "TypeError" || refused.starts_with("unsupported:"), "{refused}");
    assert_eq!(h.eval("class B2 extends PointNT {}\nnew B2(1) instanceof B2"), "true");
    assert_eq!(h.eval("(function () { try { new add(1, 2); return 'allowed' } catch (e) { return e.name } })()"), "TypeError");
    assert_eq!(depth(&mut h), 0);
}

pub fn errors_made_by_the_host_reach_the_script_with_their_constructor<H: Harness>(stress: bool) {
    let (mut h, _) = setup::<H>(stress);
    assert_eq!(h.eval("(function () { try { typeErr('bad') } catch (e) { return [e instanceof TypeError, e instanceof RangeError, e.name, e.message].join() } })()"), "true,false,TypeError,bad");
    assert_eq!(h.eval("(function () { try { rangeErr('far') } catch (e) { return [e instanceof RangeError, e instanceof TypeError, e.name, e.message].join() } })()"), "true,false,RangeError,far");
    assert_eq!(h.eval("charge(10); 'ok'"), "ok");
    assert_eq!(h.eval("charge(1e9)"), "limit");
    assert_eq!(h.eval("(function () { try { charge(1e9) } catch (e) { return 'caught' } })()"), "limit", "a script cannot catch the limit");
    assert_eq!(h.eval("charge(5); 'again'"), "again", "the next run has a new budget");
    assert_eq!(depth(&mut h), 0);
}

pub fn conversions_run_the_script_of_the_value<H: Harness>(stress: bool) {
    let (mut h, _) = setup::<H>(stress);
    assert_eq!(h.eval("num('12') + 1"), "13");
    assert_eq!(h.eval("num({ valueOf() { return 7 } })"), "7");
    assert_eq!(h.eval("str({ toString() { return 'x' } }) + '!'"), "x!");
    assert_eq!(h.eval("str(12.5)"), "12.5");
    assert_eq!(h.eval("[bool(''), bool('a'), bool({}), bool(0), bool(null)].join()"), "false,true,true,false,false");
    assert_eq!(h.eval("(function () { try { num({ valueOf() { throw 'boom' } }) } catch (e) { return e } })()"), "boom");
    assert_eq!(h.eval("num({ valueOf() { var a = []; for (var i = 0; i < 3000; i++) a.push({ i: i }); return a.length } })"), "3000");
    assert_eq!(depth(&mut h), 0);
}

pub fn arrays_prototypes_and_accessors<H: Harness>(stress: bool) {
    let (mut h, _) = setup::<H>(stress);
    assert_eq!(h.eval("arr(1, 2, 3).join('-')"), "1-2-3");
    assert_eq!(h.eval("[isArr([]), isArr({}), isArr('s')].join()"), "true,false,false");
    assert_eq!(h.eval("protoOf([]) === Array.prototype"), "true");
    assert_eq!(h.eval("protoOf(Object.create(null)) === null"), "true");
    assert_eq!(h.eval("var a = {}; var b = Object.create(a); setProto(a, b)"), "false", "a cycle is refused");
    assert_eq!(h.eval("var c = {}; setProto(c, Array.prototype) && c instanceof Array"), "true");
    assert_eq!(h.eval("var o = {}; accessor(o, 'v', function () { return this.n }, undefined); o.n = 5; o.v"), "5");
    assert_eq!(h.eval("[intrinsic(0) === Object.prototype, intrinsic(1) === Array.prototype, intrinsic(2) === Error.prototype].join()"), "true,true,true");
    assert_eq!(depth(&mut h), 0);
}

pub fn own_properties_are_read_without_running_code_except_getters<H: Harness>(stress: bool) {
    let (mut h, _) = setup::<H>(stress);
    assert_eq!(h.eval("ownKeys({ b: 1, 2: 1, a: 1, 1: 1 }).join()"), "1,2,b,a");
    assert_eq!(h.eval("[hasOwn({ x: 1 }, 'x'), hasOwn(Object.create({ x: 1 }), 'x'), hasOwn({}, 'y')].join()"), "true,false,false");
    assert_eq!(h.eval("ownData({ get k() { return 'got' } }, 'k')"), "got");
    assert_eq!(h.eval("ownData(Object.defineProperty({}, 'h', { value: 1 }), 'h')"), "<none>");
    assert_eq!(h.eval("[isCallable(function () {}), isCallable({}), isCallable(class {})].join()"), "true,false,true");
    assert_eq!(h.eval("bind(function () { return this.v }, { v: 'bound' })()"), "bound");
    assert_eq!(depth(&mut h), 0);
}

pub fn strings_from_code_units_keep_lone_surrogates<H: Harness>(stress: bool) {
    let (mut h, _) = setup::<H>(stress);
    assert_eq!(h.eval("utf16().length"), "2");
    enter(h.engine(), |s| {
        let t = s.string_utf16(&[0xD800, u16::from(b'a')]).unwrap();
        assert!(matches!(s.as_str(t), Some(Str::Utf16(&[0xD800, 0x61]))));
        assert_eq!(s.as_str(t).unwrap().to_string_lossy(), "\u{FFFD}a");
        let l = s.string_utf16(&[0x68, 0xE9]).unwrap();
        assert_eq!(s.as_str(l).unwrap().to_string_lossy(), "hé");
        // A string that a script builds by `+` reads as the same characters.
        let g = s.global();
        let f = s.get_str(g, "isNew").unwrap();
        assert_eq!(s.type_of(f), Type::Function);
    });
    assert_eq!(h.eval("var s = ''; for (var i = 0; i < 40; i++) s += 'ab'; num(s.length) + str(s).length"), "160");
    assert_eq!(depth(&mut h), 0);
}

/// The host keeps a local and a persistent handle across a top-level call into script that allocates a lot. The engine may collect
/// inside that call: both values must stay alive.
pub fn handles_survive_a_top_level_call_that_allocates<H: Harness>(stress: bool) {
    let (mut h, _) = setup::<H>(stress);
    h.eval("var churn = function (n) { var a = []; for (var i = 0; i < n; i++) a.push({ i: i, s: 'x' + i }); return a.length }; 0");
    let e = h.engine();
    let p = enter(e, |s| {
        let o = s.new_object().unwrap();
        let v = s.string("live").unwrap();
        s.define_str(o, "tag", v, Attrs::ALL).unwrap();
        s.persist(o)
    });
    enter(e, |s| {
        let o = s.new_object().unwrap();
        let v = s.string("local").unwrap();
        s.define_str(o, "tag", v, Attrs::ALL).unwrap();
        let text = s.string("a string that the host keeps").unwrap();
        let g = s.global();
        let f = s.get_str(g, "churn").unwrap();
        let (this, n) = (s.undefined(), s.number(20000.0));
        let r = s.call(f, this, &[n]).unwrap();
        assert_eq!(s.as_number(r), Some(20000.0));
        let t = s.get_str(o, "tag").unwrap();
        assert_eq!(s.as_str(t).unwrap().to_string_lossy(), "local");
        assert_eq!(s.as_str(text).unwrap().to_string_lossy(), "a string that the host keeps");
        let q = s.local(&p);
        let t = s.get_str(q, "tag").unwrap();
        assert_eq!(s.as_str(t).unwrap().to_string_lossy(), "live");
    });
    assert_eq!(depth(&mut h), 0);
}

/// Load the graph of `url` from `files` (a request `./x.js` is the file `x.js`) and give its module number.
fn load_graph<'s, E: Engine>(s: &mut Scope<'s, E>, files: &[(&str, &str)], url: &str, done: &mut Vec<(String, u32)>) -> Result<u32, String> {
    if let Some((_, m)) = done.iter().find(|(u, _)| u == url) {
        return Ok(*m);
    }
    let (_, src) = files.iter().find(|(u, _)| *u == url).ok_or_else(|| format!("no file {url}"))?;
    let m = s.module_parse(src, url).map_err(|e| error_text(s, e))?;
    done.push((url.to_owned(), m));
    let mut deps = Vec::new();
    for r in s.module_requests(m) {
        deps.push(load_graph(s, files, r.trim_start_matches("./"), done)?);
    }
    s.module_set_requested(m, &deps);
    Ok(m)
}

fn error_text<'s, E: Engine>(s: &mut Scope<'s, E>, e: Error<'s>) -> String {
    match e {
        Error::Throw(v) => s.to_string(v).ok().and_then(|t| s.as_str(t).map(|t| t.to_string_lossy())).unwrap_or_default(),
        Error::Abort(a) => format!("{a:?}"),
    }
}

/// Parse, link and evaluate `root` of `files`. `Err` is the text of the first error.
fn run_modules<H: Harness>(h: &mut H, files: &[(&str, &str)], root: &str) -> Result<(), String> {
    enter(h.engine(), |s| {
        let m = load_graph(s, files, root, &mut Vec::new())?;
        s.module_link(m).map_err(|e| error_text(s, e))?;
        s.module_evaluate(m).map_err(|e| error_text(s, e))?;
        Ok(())
    })
}

/// Static modules: the order of evaluation, live bindings, default exports, re-exports, a namespace object and a cycle.
pub fn modules_link_and_run_in_order_with_live_bindings<H: Harness>(stress: bool) {
    let (mut h, _) = setup::<H>(stress);
    h.eval("globalThis.out = []; 0");
    let files = [
        ("a.js", "import d, { x, inc } from './b.js'; import * as ns from './c.js'; import { g } from './e.js'; out.push('a', d, x); inc(); out.push(x, ns.y, ns.z, Object.keys(ns).join('|'), g());"),
        ("b.js", "import './c.js'; export let x = 1; export function inc() { x++ } export default 'dflt'; out.push('b');"),
        ("c.js", "export { x as y } from './b.js'; export * from './d.js'; out.push('c');"),
        ("d.js", "export const z = 'zed'; out.push('d');"),
        ("e.js", "import { f } from './a2.js'; export function g() { return f() + 'g' }"),
        ("a2.js", "import { g } from './e.js'; export function f() { return typeof g } "),
    ];
    run_modules(&mut h, &files, "a.js").unwrap();
    assert_eq!(h.eval("out.join()"), "d,c,b,a,dflt,1,2,2,zed,y|z,functiong");
    assert_eq!(depth(&mut h), 0);
}

/// An error of the graph is found before any module runs, and the parse of a bad module is a `SyntaxError`.
pub fn modules_report_link_and_parse_errors<H: Harness>(stress: bool) {
    let (mut h, _) = setup::<H>(stress);
    h.eval("globalThis.out = []; 0");
    let files = [("a.js", "out.push('ran'); import { nope } from './b.js';"), ("b.js", "export let x = 1;"), ("bad.js", "export let = ;")];
    let e = run_modules(&mut h, &files, "a.js").unwrap_err();
    assert!(e.starts_with("SyntaxError"), "{e}");
    assert_eq!(h.eval("out.length"), "0", "nothing ran");
    let e = run_modules(&mut h, &files, "bad.js").unwrap_err();
    assert!(e.starts_with("SyntaxError"), "{e}");
    let files = [("a.js", "import { x } from './b.js'; x = 2;"), ("b.js", "export let x = 1;")];
    // An assignment to an import is a `TypeError` when the code runs: the evaluation gives a rejected promise or a throw.
    let _ = run_modules(&mut h, &files, "a.js");
    assert_eq!(depth(&mut h), 0);
}

/// `import()` in a script reaches the host as a request. The host answers it; the answer settles the promise.
pub fn dynamic_import_reaches_the_host<H: Harness>(stress: bool) {
    let (mut h, _) = setup::<H>(stress);
    h.eval("var p = import('./b.js'); 0");
    enter(h.engine(), |s| {
        let reqs = s.take_dynamic_imports();
        assert_eq!(reqs.len(), 1);
        assert_eq!(reqs[0].specifier, "./b.js");
        assert_eq!(reqs[0].referrer, None);
        assert!(s.take_dynamic_imports().is_empty());
        s.fail_dynamic_import(&reqs[0].promise, "no such file").unwrap();
    });
    assert_eq!(depth(&mut h), 0);
}

/// The text of a value, by its string conversion (`Engine::display` is not built in every engine).
fn text<'s, E: Engine>(s: &mut Scope<'s, E>, v: Local<'s>) -> String {
    match s.to_string(v) {
        Ok(t) => s.as_str(t).map(|t| t.to_string_lossy()).unwrap_or_default(),
        Err(_) => "<error>".into(),
    }
}

pub fn run_script_gives_the_completion_value<H: Harness>(stress: bool) {
    let (mut h, _) = setup::<H>(stress);
    let mut run = |src: &str| {
        enter(h.engine(), |s| match s.run_script(src) {
            Ok(v) => text(s, v),
            Err(Error::Throw(e)) => format!("throw:{}", text(s, e)),
            Err(Error::Abort(_)) => "abort".into(),
        })
    };
    assert_eq!(run("1; 2"), "2");
    assert_eq!(run("var rs = 5; rs * 2"), "10");
    assert_eq!(run("rs"), "5", "the next script sees the globals of the first");
    assert_eq!(run("1; if (true) { 2 }"), "2");
    assert_eq!(run("1; if (true) {}"), "undefined");
    assert_eq!(run("var i = 0; while (i < 3) { i++ }"), "2");
    assert_eq!(run("1; throw new TypeError('x')"), "throw:TypeError: x");
    assert!(run("1 +").starts_with("throw:SyntaxError"));
    assert_eq!(run("add(1, 2)"), "3", "a host function runs from a script that run_script started");
    assert_eq!(depth(&mut h), 0);
}

pub fn eval_runs_in_the_global_environment<H: Harness>(stress: bool) {
    let (mut h, _) = setup::<H>(stress);
    enter(h.engine(), |s| {
        let ev = s.eval_function().unwrap();
        let this = s.undefined();
        let eval = |s: &mut Scope<'_, H::E>, src: &str| {
            let x = s.string(src).unwrap();
            match s.call(ev, this, &[x]) {
                Ok(v) => text(s, v),
                Err(Error::Throw(e)) => format!("throw:{}", text(s, e)),
                Err(Error::Abort(_)) => "abort".into(),
            }
        };
        assert_eq!(eval(s, "var ev1 = 4; ev1 + 1"), "5");
        assert_eq!(eval(s, "typeof ev1"), "number");
        assert_eq!(eval(s, "function evf() { return 8 } evf()"), "8");
        assert!(eval(s, "1 +").starts_with("throw:SyntaxError"));
        let n = s.number(3.0);
        let r = s.eval_global(n).unwrap();
        assert_eq!(s.as_number(r), Some(3.0), "a value that is not a string is the result");
        let src = s.string("var ev2 = 6; ev2 * 7").unwrap();
        let r = s.eval_global(src).unwrap();
        assert_eq!(s.as_number(r), Some(42.0));
    });
    assert_eq!(h.eval("ev1 + ev2"), "10");
    assert_eq!(depth(&mut h), 0);
}

pub fn handler_functions_look_names_up_in_the_scope<H: Harness>(stress: bool) {
    let (mut h, _) = setup::<H>(stress);
    h.eval("var holder = { v: 1, get g() { return this === holder }, m() { return this === holder }, set w(x) { this.v = x } }; var v = 'global'; var other = { v: 'other' }; function plain() { return typeof m } 0");
    enter(h.engine(), |s| {
        let g = s.global();
        let (holder, other) = (s.get_str(g, "holder").unwrap(), s.get_str(g, "other").unwrap());
        let f = s.compile_function("h", "event", "w = 5; return [v, g, m(), typeof nope, typeof m, plain()].join()").unwrap();
        let this = s.undefined();
        let r = s.call_scoped(f, this, &[], &[holder, other]).unwrap();
        assert_eq!(text(s, r), "5,true,true,undefined,function,undefined");
        let v = s.get_str(holder, "v").unwrap();
        assert_eq!(s.as_number(v), Some(5.0));
        let (n, l) = (s.get_str(f, "name").unwrap(), s.get_str(f, "length").unwrap());
        assert_eq!((text(s, n), text(s, l)), ("h".to_string(), "1".to_string()));
        // With no scope the global is used.
        let p = s.compile_function("h", "", "return typeof g + v").unwrap();
        let r = s.call_scoped(p, this, &[], &[]).unwrap();
        assert_eq!(text(s, r), "undefinedglobal");
        // The same text makes a new function.
        let f2 = s.compile_function("h", "event", "w = 5; return [v, g, m(), typeof nope, typeof m, plain()].join()").unwrap();
        assert_eq!(s.type_of(f2), Type::Function);
        // A body that closes the function is a SyntaxError.
        for body in ["}); globalThis.bad = 1; (function () {", "}, function () { globalThis.bad = 2", "} } {"] {
            assert!(matches!(s.compile_function("h", "event", body), Err(Error::Throw(_))), "{body}");
        }
    });
    assert_eq!(h.eval("typeof bad"), "undefined");
    assert_eq!(depth(&mut h), 0);
}

pub fn the_host_walks_iterables<H: Harness>(stress: bool) {
    let (mut h, _) = setup::<H>(stress);
    let mut take = |src: &str| -> String {
        enter(h.engine(), |s| {
            let v = match s.run_script(src) {
                Ok(v) => v,
                Err(_) => return "script failed".into(),
            };
            let mut it = match s.iter_open(v) {
                Ok(Some(it)) => it,
                Ok(None) => return "none".into(),
                Err(Error::Throw(e)) => return format!("throw:{}", text(s, e)),
                Err(Error::Abort(_)) => return "abort".into(),
            };
            let mut out = Vec::new();
            loop {
                match s.iter_next(&mut it) {
                    Ok(Some(x)) => out.push(text(s, x)),
                    Ok(None) => break,
                    Err(Error::Throw(e)) => return format!("throw:{}", text(s, e)),
                    Err(Error::Abort(_)) => return "abort".into(),
                }
            }
            assert!(matches!(s.iter_next(&mut it), Ok(None)), "an iteration that ended stays ended");
            out.join(",")
        })
    };
    assert_eq!(take("[1, 2, 3]"), "1,2,3");
    assert_eq!(take("'a\u{1F600}b'"), "a,\u{1F600},b");
    assert_eq!(take("({ [Symbol.iterator]() { var i = 0; return { next() { return i < 3 ? { value: i++, done: false } : { done: true } } } } })"), "0,1,2");
    assert_eq!(take("undefined"), "none");
    assert_eq!(take("({})"), "none");
    assert_eq!(take("({ [Symbol.iterator]() { return { next() { throw 'boom' } } } })"), "throw:boom");
    // `iter_fail` calls `return` and keeps the error.
    h.eval("var closed = 0; var it1 = { [Symbol.iterator]() { return { next() { return { value: 1, done: false } }, return() { closed++; return {} } } } }; 0");
    enter(h.engine(), |s| {
        let g = s.global();
        let v = s.get_str(g, "it1").unwrap();
        let mut it = s.iter_open(v).unwrap().unwrap();
        assert!(s.iter_next(&mut it).unwrap().is_some());
        let e = s.type_error("stop");
        assert!(matches!(s.iter_fail(&mut it, e), Error::Throw(_)));
        assert!(matches!(s.iter_next(&mut it), Ok(None)));
    });
    assert_eq!(h.eval("closed"), "1");
    assert_eq!(depth(&mut h), 0);
}

pub fn symbols_and_the_iterator_intrinsics<H: Harness>(stress: bool) {
    let (mut h, _) = setup::<H>(stress);
    enter(h.engine(), |s| {
        let i = s.intrinsic(Intrinsic::SymbolIterator);
        assert_eq!(s.type_of(i), Type::Symbol);
        let t = s.intrinsic(Intrinsic::SymbolToStringTag);
        assert_eq!(s.type_of(t), Type::Symbol);
        let g = s.global();
        s.define_str(g, "symIter", i, Attrs::ALL).unwrap();
        s.define_str(g, "symTag", t, Attrs::ALL).unwrap();
        let same = s.run_script("symIter === Symbol.iterator && symTag === Symbol.toStringTag").unwrap();
        assert_eq!(s.as_bool(same), Some(true));
        let d = s.run_script("Symbol('why')").unwrap();
        let why = s.symbol_description(d).unwrap().unwrap();
        assert_eq!(s.as_str(why).unwrap().to_string_lossy(), "why");
        let nd = s.run_script("Symbol()").unwrap();
        assert!(s.symbol_description(nd).unwrap().is_none());
        let p = s.intrinsic(Intrinsic::IteratorPrototype);
        assert_eq!(s.type_of(p), Type::Object);
        s.define_str(g, "itProto", p, Attrs::ALL).unwrap();
        let r = s.run_script("var o = Object.create(itProto); o[Symbol.iterator]() === o").unwrap();
        assert_eq!(s.as_bool(r), Some(true), "an iterator inherits the method that returns this");
    });
    assert_eq!(depth(&mut h), 0);
}

fn global<'s, E: Engine>(s: &mut Scope<'s, E>, name: &str) -> Local<'s> {
    let g = s.global();
    s.get_str(g, name).unwrap()
}

/// A promise that the host makes settles with a value, and its reactions run on the drain, in the order they were added. A value that
/// is no promise is wrapped by `promise_react`; `promise_status` says no. The handler allocates, so the stress setting collects inside a job.
pub fn a_promise_of_the_host_settles_and_its_reactions_run_on_the_drain<H: Harness>(stress: bool) {
    let (mut h, _) = setup::<H>(stress);
    h.eval(
        "var log = []; var churn = () => { var a = []; for (var i = 0; i < 200; i++) a.push({ i: i, s: 'x' + i }); return a.length };
         var ok = v => { churn(); log.push('ok:' + (v && v.tag ? v.tag : v)) }; var bad = v => log.push('bad:' + v); 0",
    );
    enter(h.engine(), |s| {
        let (ok, bad) = (global(s, "ok"), global(s, "bad"));
        let p = s.new_promise().unwrap();
        assert_eq!(s.promise_status(p), Some(PromiseState::Pending));
        s.promise_then(p, ok, bad).unwrap();
        let value = s.new_object().unwrap();
        let tag = s.string("kept").unwrap();
        s.define_str(value, "tag", tag, Attrs::ALL).unwrap();
        s.resolve_promise(p, value).unwrap();
        let Some(PromiseState::Fulfilled(v)) = s.promise_status(p) else { panic!("fulfilled") };
        let t = s.get_str(v, "tag").unwrap();
        assert_eq!(s.as_str(t).unwrap().to_string_lossy(), "kept");
        assert_eq!(s.pending_jobs(), 1, "the reaction waits for the drain");
        // A promise that is settled already: the reaction is queued by `promise_then`.
        let q = s.new_promise().unwrap();
        let why = s.string("no").unwrap();
        s.reject_promise(q, why).unwrap();
        s.promise_then(q, ok, bad).unwrap();
        let Some(PromiseState::Rejected(r)) = s.promise_status(q) else { panic!("rejected") };
        assert_eq!(s.as_str(r).unwrap().to_string_lossy(), "no");
        // A value that is not a promise: `react` wraps it, `status` says no, and a settle is refused.
        let n = s.number(3.0);
        assert_eq!(s.promise_status(n), None);
        s.promise_react(n, ok, bad).unwrap();
        assert!(matches!(s.resolve_promise(n, n), Err(Error::Abort(crate::Abort::Unsupported(_)))));
        assert!(matches!(s.promise_then(n, ok, bad), Err(Error::Abort(crate::Abort::Unsupported(_)))));
        // A native promise goes through `react` as it is.
        let r = s.new_promise().unwrap();
        s.promise_react(r, ok, bad).unwrap();
        let eight = s.number(8.0);
        s.resolve_promise(r, eight).unwrap();
        assert_eq!(s.drain_microtasks().unwrap(), 4);
        assert_eq!(s.pending_jobs(), 0);
    });
    assert_eq!(h.eval("log.join()"), "ok:kept,bad:no,ok:3,ok:8");
    assert_eq!(depth(&mut h), 0);
}

/// The host settles a promise once: a second settle does nothing.
pub fn a_second_settle_by_the_host_changes_nothing<H: Harness>(stress: bool) {
    let (mut h, _) = setup::<H>(stress);
    enter(h.engine(), |s| {
        let p = s.new_promise().unwrap();
        let (one, two) = (s.number(1.0), s.number(2.0));
        s.resolve_promise(p, one).unwrap();
        s.resolve_promise(p, two).unwrap();
        s.reject_promise(p, two).unwrap();
        let Some(PromiseState::Fulfilled(v)) = s.promise_status(p) else { panic!("fulfilled") };
        assert_eq!(s.as_number(v), Some(1.0));
    });
}

/// A promise that a script holds is read by the host, and one that the host makes is read by the script.
pub fn promises_cross_between_script_and_host<H: Harness>(stress: bool) {
    let (mut h, _) = setup::<H>(stress);
    assert_eq!(
        h.eval("var out = []; var pr = Promise.resolve(5); pr.then(v => out.push('a' + v)); queue(() => out.push('m')); 0"),
        "0"
    );
    enter(h.engine(), |s| {
        let pr = global(s, "pr");
        let Some(PromiseState::Fulfilled(v)) = s.promise_status(pr) else { panic!("fulfilled") };
        assert_eq!(s.as_number(v), Some(5.0));
        assert_eq!(s.pending_jobs(), 2);
        // A thenable that a script made is assimilated in a job.
        let out = global(s, "out");
        let push = s.get_str(out, "push").unwrap();
        let (this, tag) = (out, s.string("t").unwrap());
        s.call(push, this, &[tag]).unwrap();
        assert_eq!(s.drain_microtasks().unwrap(), 2);
    });
    assert_eq!(h.eval("out.join()"), "t,a5,m");
}

/// A microtask runs once, in the checkpoint; one that is no function is a `TypeError`.
pub fn a_microtask_runs_once_on_the_drain<H: Harness>(stress: bool) {
    let (mut h, _) = setup::<H>(stress);
    h.eval("var n = 0; var tick = () => { n++ }; 0");
    enter(h.engine(), |s| {
        let tick = global(s, "tick");
        assert_eq!(s.pending_jobs(), 0);
        s.queue_microtask(tick).unwrap();
        assert_eq!(s.pending_jobs(), 1);
        let not_a_function = s.number(1.0);
        assert!(matches!(s.queue_microtask(not_a_function), Err(Error::Throw(_))));
        assert_eq!(s.drain_microtasks().unwrap(), 1);
        assert_eq!(s.drain_microtasks().unwrap(), 0);
        assert_eq!(s.pending_jobs(), 0);
    });
    assert_eq!(h.eval("n"), "1");
    assert_eq!(depth(&mut h), 0);
}

/// A checkpoint from a host call that a script function called does nothing (the HTML rule); the job runs in the next one.
pub fn a_checkpoint_inside_a_script_function_does_nothing<H: Harness>(stress: bool) {
    let (mut h, _) = setup::<H>(stress);
    assert_eq!(h.eval("var n = 0; function outer() { return runJobs(() => { n++ }) } var ran = outer(); ran + ',' + n"), "0,0");
    assert_eq!(h.engine().pending_jobs(), 1);
    task(h.engine(), |s| assert_eq!(s.checkpoint().unwrap(), 1)).unwrap();
    assert_eq!(h.eval("n"), "1");
    assert_eq!(depth(&mut h), 0);
}

/// A promise survives collections when the host holds it as a local that it settled in the call, or as a persistent handle that it
/// settles in a later call. The value of a queued reaction lives in the queue only.
pub fn a_promise_survives_collections_as_a_local_and_as_a_persistent_handle<H: Harness>(stress: bool) {
    let (mut h, _) = setup::<H>(stress);
    h.eval("var log = []; var ok = v => { var a = []; for (var i = 0; i < 200; i++) a.push({ i: i }); log.push('ok:' + v.tag) }; var bad = () => log.push('bad'); 0");
    let kept = enter(h.engine(), |s| {
        let (ok, bad) = (global(s, "ok"), global(s, "bad"));
        let p = s.new_promise().unwrap();
        s.promise_then(p, ok, bad).unwrap();
        s.persist(p)
    });
    h.collect();
    enter(h.engine(), |s| {
        let p = s.local(&kept);
        assert_eq!(s.promise_status(p), Some(PromiseState::Pending));
        let value = s.new_object().unwrap();
        let tag = s.string("later").unwrap();
        s.define_str(value, "tag", tag, Attrs::ALL).unwrap();
        s.resolve_promise(p, value).unwrap();
        assert_eq!(s.pending_jobs(), 1);
    });
    h.collect();
    enter(h.engine(), |s| {
        assert_eq!(s.drain_microtasks().unwrap(), 1);
        // A promise that was settled in the call, and whose reaction runs in a drain that allocates.
        let (ok, bad) = (global(s, "ok"), global(s, "bad"));
        let p = s.new_promise().unwrap();
        let value = s.new_object().unwrap();
        let tag = s.string("now").unwrap();
        s.define_str(value, "tag", tag, Attrs::ALL).unwrap();
        s.promise_then(p, ok, bad).unwrap();
        s.resolve_promise(p, value).unwrap();
        assert_eq!(s.drain_microtasks().unwrap(), 1);
        let Some(PromiseState::Fulfilled(v)) = s.promise_status(p) else { panic!("fulfilled") };
        let t = s.get_str(v, "tag").unwrap();
        assert_eq!(s.as_str(t).unwrap().to_string_lossy(), "now");
    });
    assert_eq!(h.eval("log.join()"), "ok:later,ok:now");
    assert_eq!(depth(&mut h), 0);
}

/// `between_jobs` runs before each job and once when the queue is empty. An error of it ends the drain like a failed job: the same error
/// comes again until `clear_failed_job`, and the jobs that were left run then.
pub fn between_jobs_runs_before_each_job_and_an_error_ends_the_drain<H: Harness>(stress: bool) {
    let (mut h, host) = setup::<H>(stress);
    h.eval("var n = 0; var f = () => { n++ }; 0");
    enter(h.engine(), |s| {
        let f = global(s, "f");
        for _ in 0..3 {
            s.queue_microtask(f).unwrap();
        }
        assert_eq!(s.drain_microtasks().unwrap(), 3);
    });
    assert_eq!(host.between.get(), 4, "one call before each of 3 jobs, one when the queue is empty");
    assert_eq!(depth(&mut h), 0);
    host.between.set(0);
    host.fail_between_at.set(2);
    enter(h.engine(), |s| {
        let f = global(s, "f");
        for _ in 0..3 {
            s.queue_microtask(f).unwrap();
        }
        assert!(matches!(s.drain_microtasks(), Err(Error::Throw(_))));
        assert!(matches!(s.drain_microtasks(), Err(Error::Throw(_))), "the failure is sticky");
        assert_eq!(s.pending_jobs(), 2);
        s.clear_failed_job();
        assert_eq!(s.drain_microtasks().unwrap(), 2);
    });
    assert_eq!(h.eval("n"), "6", "3 jobs, then 1, then the 2 that were left");
    assert_eq!(depth(&mut h), 0);
}

/// The host hears a throw of a `queueMicrotask` callback, an unhandled rejection once, and a late handler once. The drain goes on.
pub fn the_host_hears_thrown_callbacks_and_rejections<H: Harness>(stress: bool) {
    let (mut h, host) = setup::<H>(stress);
    h.eval("var log = []; queue(() => { throw 'boom' }); queue(() => log.push('after')); var p = Promise.reject('nope'); 0");
    enter(h.engine(), |s| assert_eq!(s.drain_microtasks().unwrap(), 2));
    assert_eq!(*host.reported.borrow(), ["boom"]);
    assert_eq!(*host.rejections.borrow(), ["unhandled:nope"]);
    assert_eq!(h.eval("log.join()"), "after");
    h.eval("p.catch(() => {}); 0");
    enter(h.engine(), |s| assert_eq!(s.drain_microtasks().unwrap(), 1));
    assert_eq!(*host.rejections.borrow(), ["unhandled:nope", "handled"]);
    enter(h.engine(), |s| assert_eq!(s.drain_microtasks().unwrap(), 0));
    assert_eq!(host.rejections.borrow().len(), 2, "each is told once");
    assert_eq!(depth(&mut h), 0);
}

/// At most `MAX_PENDING_REJECTIONS` rejections are kept; the rest are counted.
pub fn the_rejections_over_the_bound_are_counted<H: Harness>(stress: bool) {
    let (mut h, host) = setup::<H>(stress);
    h.eval("for (var i = 0; i < 1100; i++) Promise.reject(i); 0");
    enter(h.engine(), |s| assert_eq!(s.drain_microtasks().unwrap(), 0));
    let r = host.rejections.borrow();
    assert_eq!(r.iter().filter(|t| t.starts_with("unhandled:")).count(), crate::MAX_PENDING_REJECTIONS);
    assert_eq!(r.last().unwrap(), &format!("dropped:{}", 1100 - crate::MAX_PENDING_REJECTIONS));
}
