//! The tests that every engine must pass (`docs/design-js2-host-api.md`, section 5). Feature `suite`.
//!
//! An engine makes a [`Harness`]: a realm, a way to run script text and a way to collect. Each function below is one test. It uses
//! only the interface, so the same function runs on `bare-js` and on `bare-js2` (`tests/engines.rs`). A test that needs an engine
//! detail stays in the crate of that engine.
//!
//! Scripts are written so that the value of the last statement is an expression.

use crate::{Args, Attrs, Engine, Error, Host, HostFn, Intrinsic, Item, Lazy, Local, Named, Pending, Persistent, PromiseState, Scope, Str, Traced, Tracer, Type, enter, task};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
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
    /// Let the engine answer for host-filled objects through `host` (an engine that needs a call besides `Engine::set_host`).
    fn attach(&mut self, _host: &Rc<Fill<<Self::E as Engine>::Raw>>) {}
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

/// The host of the owner, host-filled and lazy tests. It is generic over the raw value of the engine, so one value serves each engine
/// (`Fill<E::Raw>` is a `Host<E>` for every `E` with that raw value).
///
/// Host functions: 0 `mkIndexed(proto)`, 1 `mkNamed(proto, open)`, 2 `own(obj, key, ...held)`, 3 `setLazy(obj, chain)`,
/// 4 `clearLazy(obj)`, 5 `hostKey(obj)`, 6 `pruned()`, 7 `held(key, i)`, 8 `traced()`, 9 `asked()`.
/// An indexed object has the items `i0`, `i1`, `i2` (key 10). A named object has the names `a` (`alpha`) and `b` (`beta`) (key 20 for
/// closed, 21 for open). A lazy object gets the property `onlazy` (7) when the host is asked for that name.
pub struct Fill<R> {
    items: RefCell<Vec<R>>,
    keep: RefCell<Vec<Persistent>>,
    held: RefCell<HashMap<u32, Vec<Traced<R>>>>,
    traced: RefCell<Vec<u32>>,
    pruned: RefCell<Vec<u32>>,
    asked: RefCell<Vec<String>>,
}

impl<R> Default for Fill<R> {
    fn default() -> Self {
        Fill { items: Default::default(), keep: Default::default(), held: Default::default(), traced: Default::default(), pruned: Default::default(), asked: Default::default() }
    }
}

const FILL: [&str; 10] = ["mkIndexed", "mkNamed", "own", "setLazy", "clearLazy", "hostKey", "pruned", "held", "traced", "asked"];

fn units(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}

impl<E: Engine> Host<E> for Fill<E::Raw> {
    fn call<'s>(&self, s: &mut Scope<'s, E>, id: u32, _this: Local<'s>, args: Args<'s>, _is_new: bool) -> Result<Local<'s>, Error<'s>> {
        let arg = |s: &mut Scope<'s, E>, i| match args.get(i) {
            Some(v) => v,
            None => s.undefined(),
        };
        let (a, b) = (arg(s, 0), arg(s, 1));
        let key_of = |s: &Scope<'s, E>, v| s.as_number(v).unwrap_or(0.0) as u32;
        match id {
            0 | 1 => {
                if self.items.borrow().is_empty() {
                    for t in ["i0", "i1", "i2"] {
                        let l = s.string(t)?;
                        let raw = s.engine().raw(l);
                        self.keep.borrow_mut().push(s.persist(l));
                        self.items.borrow_mut().push(raw);
                    }
                }
                let proto = (s.type_of(a) == Type::Object).then_some(a);
                if id == 0 {
                    s.new_host_indexed(proto, 10)
                } else {
                    let open = s.to_boolean(b);
                    s.new_host_named(proto, 20 + open as u32, open)
                }
            }
            2 => {
                let key = key_of(s, b);
                s.set_host_owner(a, key)?;
                let held: Vec<_> = args.iter().skip(2).map(|v| s.trace_ref(v)).collect();
                self.held.borrow_mut().insert(key, held);
                Ok(s.undefined())
            }
            3 => {
                let chain = s.to_boolean(b);
                s.set_lazy(a, chain)?;
                Ok(s.undefined())
            }
            4 => {
                s.clear_lazy(a);
                Ok(s.undefined())
            }
            5 => {
                let k = s.host_key(a).map_or(-1.0, f64::from);
                Ok(s.number(k))
            }
            6 => {
                let mut p = self.pruned.borrow().clone();
                p.sort();
                s.string(&p.iter().map(u32::to_string).collect::<Vec<_>>().join(","))
            }
            7 => {
                let (key, i) = (key_of(s, a), key_of(s, b) as usize);
                let held = self.held.borrow();
                Ok(s.traced(&held[&key][i]))
            }
            8 => {
                let mut t = self.traced.borrow().clone();
                t.sort();
                t.dedup();
                s.string(&t.iter().map(u32::to_string).collect::<Vec<_>>().join(","))
            }
            _ => s.string(&self.asked.borrow().join(",")),
        }
    }

    fn trace(&self, key: u32, t: &mut Tracer<'_, E::Raw>) {
        self.traced.borrow_mut().push(key);
        for v in self.held.borrow().get(&key).into_iter().flatten() {
            t.visit(v);
        }
    }

    fn prune(&self, dead: &[u32]) {
        self.pruned.borrow_mut().extend_from_slice(dead);
        for k in dead {
            self.held.borrow_mut().remove(k);
        }
    }

    fn indexed_len(&self, _object: &E::Raw, key: u32) -> Result<usize, Pending> {
        Ok(if key == 10 { self.items.borrow().len() } else { 0 })
    }

    fn indexed_item(&self, _object: &E::Raw, _key: u32, index: usize) -> Result<Item<E::Raw>, Pending> {
        Ok(self.items.borrow().get(index).cloned().map_or(Item::Absent, Item::Value))
    }

    fn named_has(&self, _object: &E::Raw, _key: u32, name: &[u16]) -> Result<bool, Pending> {
        Ok(name == units("a") || name == units("b"))
    }

    fn named_get(&self, _object: &E::Raw, _key: u32, name: &[u16]) -> Result<Option<Named<E::Raw>>, Pending> {
        Ok(Some(Named::Text(units(if name == units("a") { "alpha" } else { "beta" }))))
    }

    fn named_len(&self, _object: &E::Raw, _key: u32) -> Result<usize, Pending> {
        Ok(2)
    }

    fn named_keys(&self, _object: &E::Raw, _key: u32) -> Result<Vec<Vec<u16>>, Pending> {
        Ok(vec![units("a"), units("b")])
    }

    fn lazy_member<'s>(&self, s: &mut Scope<'s, E>, object: Local<'s>, what: Lazy<'_>) -> Result<(), Error<'s>> {
        if let Lazy::Name("onlazy") = what {
            self.asked.borrow_mut().push("onlazy".into());
            let v = s.number(7.0);
            s.define_str(object, "onlazy", v, Attrs::ALL)?;
        }
        Ok(())
    }
}

fn setup_fill<H: Harness>(stress: bool) -> (H, Rc<Fill<<H::E as Engine>::Raw>>) {
    let mut h = H::new(stress);
    let host = Rc::new(Fill::default());
    h.attach(&host);
    enter(h.engine(), |s| {
        s.set_host(host.clone());
        let g = s.global();
        for (i, name) in FILL.into_iter().enumerate() {
            let f = s.host_function(name, i as u32, 1, HostFn::Method).unwrap();
            s.define_str(g, name, f, Attrs::METHOD).unwrap();
        }
    });
    (h, host)
}

/// An owner keeps the values of its host alive through `trace`, a dead owner is pruned once, and a cycle through an owner is freed.
pub fn owners_are_traced_while_alive_and_pruned_once<H: Harness>(stress: bool) {
    let (mut h, _) = setup_fill::<H>(stress);
    h.eval("var keep = {}; own(keep, 6, { n: 2 }); own({}, 5, { n: 1 }); var c = {}; own(c, 7, { back: c }); c = null; 0");
    h.collect();
    h.collect();
    assert_eq!(h.eval("pruned()"), "5,7", "an unreachable owner is pruned, also one in a cycle with its held value");
    assert_eq!(h.eval("held(6, 0).n"), "2", "a live owner keeps what it holds");
    assert!(h.eval("traced()").contains('6'));
    h.collect();
    assert_eq!(h.eval("pruned()"), "5,7", "each dead owner is pruned once");
    assert_eq!(h.eval("keep = null; 0"), "0");
    h.collect();
    assert_eq!(h.eval("pruned()"), "5,6,7");
    // A chain of owners: each is reached only through the one before.
    h.eval("var a = {}, b = {}, d = {}; own(d, 3, { tag: 'end' }); own(b, 2, d); own(a, 1, b); var root = a; a = b = d = null; 0");
    h.collect();
    assert_eq!(h.eval("held(3, 0).tag"), "end");
    assert_eq!(h.eval("traced().indexOf('3') >= 0 && traced().indexOf('2') >= 0"), "true");
    assert_eq!(depth(&mut h), 0);
}

/// An object made by `new_host_indexed` reads its items from the host, and the host key can be read back.
pub fn a_host_indexed_object_reads_the_host<H: Harness>(stress: bool) {
    let (mut h, _) = setup_fill::<H>(stress);
    h.eval("var proto = { foo: 5 }; var nl = mkIndexed(proto); 0");
    h.collect();
    assert_eq!(h.eval("nl[0] + nl[2]"), "i0i2");
    assert_eq!(h.eval("String(nl[3])"), "undefined");
    assert_eq!(h.eval("nl.foo"), "5");
    assert_eq!(h.eval("(1 in nl) + ',' + (3 in nl)"), "true,false");
    assert_eq!(h.eval("Object.keys(nl).join()"), "0,1,2");
    assert_eq!(h.eval("nl.hasOwnProperty(1) + ',' + nl.hasOwnProperty(5)"), "true,false");
    assert_eq!(h.eval("(nl[0] = 'x', nl[0])"), "i0", "an index of an indexed getter with no setter cannot be written");
    assert_eq!(h.eval("hostKey(nl) + ',' + hostKey({})"), "10,-1");
    h.collect();
    assert_eq!(h.eval("nl[1]"), "i1", "the same after a collection");
    assert_eq!(depth(&mut h), 0);
}

/// An object made by `new_host_named` answers its names from the host.
pub fn a_host_named_object_reads_the_host<H: Harness>(stress: bool) {
    let (mut h, _) = setup_fill::<H>(stress);
    h.eval("var proto = { foo: 5 }; var ds = mkNamed(proto, false); var op = mkNamed(proto, true); 0");
    h.collect();
    assert_eq!(h.eval("ds.a + ds.b"), "alphabeta");
    assert_eq!(h.eval("('a' in ds) + ',' + ('zz' in ds)"), "true,false");
    assert_eq!(h.eval("Object.keys(ds).join()"), "a,b");
    assert_eq!(h.eval("String(ds.zz) + ds.foo"), "undefined5");
    assert_eq!(h.eval("ds.hasOwnProperty('b') + ',' + ds.hasOwnProperty('zz')"), "true,false");
    assert_eq!(h.eval("op.a"), "alpha");
    assert_eq!(h.eval("hostKey(ds) + ',' + hostKey(op)"), "20,21");
    assert_eq!(h.eval("(proto.b = 'pb', ds.b + ',' + op.b)"), "beta,pb", "a closed object answers its names first, an open one after its chain");
    assert_eq!(depth(&mut h), 0);
}

/// A lazy object lets the host define a member at the first own-property lookup that misses (`set_lazy(false)`) or at a read that
/// misses through the chain to it (`set_lazy(true)`); the host is asked once per name. `clear_lazy` stops the asks.
pub fn a_lazy_member_is_made_at_the_first_miss<H: Harness>(stress: bool) {
    let (mut h, _) = setup_fill::<H>(stress);
    h.eval("var lz = {}; setLazy(lz, false); 0");
    h.collect();
    assert_eq!(h.eval("Object.getOwnPropertyDescriptor(lz, 'onlazy').value"), "7");
    assert_eq!(h.eval("lz.onlazy + Object.getOwnPropertyDescriptor(lz, 'onlazy').value"), "14");
    assert_eq!(h.eval("asked()"), "onlazy", "asked once");
    h.eval("var other = {}; setLazy(other, false); clearLazy(other); 0");
    assert_eq!(h.eval("String(Object.getOwnPropertyDescriptor(other, 'onlazy'))"), "undefined");
    assert_eq!(h.eval("asked()"), "onlazy", "a cleared mark asks nothing");
    // A chain mark: a read through a child that misses reaches the marked prototype.
    h.eval("var lp = {}; setLazy(lp, true); var child = Object.create(lp); 0");
    h.collect();
    assert_eq!(h.eval("child.onlazy"), "7");
    assert_eq!(h.eval("asked()"), "onlazy,onlazy");
    assert_eq!(depth(&mut h), 0);
}
