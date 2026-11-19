//! The old engine behind `bare-js-api` (plan step S0.3): host functions, handles, rooting and errors through the interface.

use super::*;
use crate::heap::Heap;
use bare_js_api::{Args, Attrs, Engine, Local, Persistent, PromiseState, Scope, Type, enter};
use std::cell::RefCell;
use std::collections::HashMap;

fn big<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(64 << 20).spawn(f).unwrap().join().unwrap()
}

/// A host written only against the interface, so it would run on any engine.
#[derive(Default)]
struct H {
    kept: RefCell<Vec<Persistent>>,
}

impl<E: Engine> bare_js_api::Host<E> for H {
    fn call<'s>(&self, s: &mut Scope<'s, E>, id: u32, _this: Local<'s>, args: Args<'s>, _is_new: bool) -> Result<Local<'s>, bare_js_api::Error<'s>> {
        let arg = |s: &mut Scope<'s, E>, i| match args.get(i) {
            Some(v) => v,
            None => s.undefined(),
        };
        match id {
            // add(a, b)
            0 => {
                let (a, b) = (arg(s, 0), arg(s, 1));
                let n = s.as_number(a).unwrap_or(f64::NAN) + s.as_number(b).unwrap_or(f64::NAN);
                Ok(s.number(n))
            }
            // callback(f, x): f(x), from the host
            1 => {
                let (f, x, this) = (arg(s, 0), arg(s, 1), s.undefined());
                s.call(f, this, &[x])
            }
            // fail(v): throw v
            2 => Err(bare_js_api::Error::Throw(arg(s, 0))),
            // keep(v): a persistent handle
            3 => {
                let v = arg(s, 0);
                let p = s.persist(v);
                self.kept.borrow_mut().push(p);
                Ok(s.undefined())
            }
            // kept(i)
            4 => {
                let i = arg(s, 0);
                let i = s.as_number(i).unwrap_or(0.0) as usize;
                let kept = self.kept.borrow();
                Ok(s.local(&kept[i]))
            }
            // describe(o): "<o.name>:<typeof o.f>", built in an inner scope
            _ => {
                let o = arg(s, 0);
                s.escape_scope(|s| {
                    let name = s.get_str(o, "name")?;
                    let name = s.as_str(name).map(|t| t.to_string_lossy()).unwrap_or_default();
                    let f = s.get_str(o, "f")?;
                    let t = format!("{name}:{:?}", s.type_of(f));
                    s.string(&t)
                })
            }
        }
    }
}

fn setup() -> (Interp<'static>, Rc<H>) {
    let mut it = Interp::realm(Heap::default(), EvalLimits { max_steps: 100_000, ..Default::default() }).unwrap();
    let h = Rc::new(H::default());
    enter(&mut it, |s| {
        s.set_host(h.clone());
        let g = s.global();
        for (i, name) in ["add", "callback", "fail", "keep", "kept", "describe"].into_iter().enumerate() {
            let f = s.host_function(name, i as u32, 1).unwrap();
            let k = s.string(name).unwrap();
            s.define(g, k, f, Attrs::METHOD).unwrap();
        }
    });
    (it, h)
}

fn out(it: &mut Interp, src: &str) -> String {
    match it.run_script(src) {
        Ok(v) => it.display(v),
        Err(Error::Throw(v)) => format!("throw:{}", it.display(v)),
        Err(Error::Limit(l)) => format!("limit:{l:?}"),
        Err(e) => format!("other:{e:?}"),
    }
}

#[test]
fn host_functions_of_the_interface_run_in_scripts() {
    big(|| {
        let (mut it, _) = setup();
        assert_eq!(out(&mut it, "add(2, 3)"), "5");
        assert_eq!(out(&mut it, "callback(x => x * 10, 4)"), "40");
        assert_eq!(out(&mut it, "try { fail('no') } catch (e) { 'caught ' + e }"), "caught no");
        assert_eq!(out(&mut it, "describe({ name: 'box', f() {} })"), "box:Function");
        assert_eq!(it.stack_len(), 0, "every call gives its slots back");
    });
}

#[test]
fn an_engine_error_crosses_the_host_unchanged() {
    big(|| {
        let (mut it, _) = setup();
        assert_eq!(out(&mut it, "callback(() => { for (;;) {} }, 0)"), "limit:Steps");
        assert_eq!(it.stack_len(), 0);
    });
}

#[test]
fn a_persistent_handle_keeps_its_value_through_a_collection() {
    big(|| {
        let (mut it, h) = setup();
        out(&mut it, "keep({ tag: 'kept' }); var junk = []; for (var i = 0; i < 100; i++) junk.push({}); junk = null; 0");
        it.collect(&[]).unwrap();
        assert_eq!(out(&mut it, "kept(0).tag"), "kept");
        let p = h.kept.borrow_mut().pop().unwrap();
        drop(p);
        let mut roots = Vec::new();
        it.api.values(&mut roots);
        assert!(roots.is_empty(), "a dropped handle is no root");
        out(&mut it, "keep(1)");
        assert_eq!(it.api.persistent.len(), 1, "the freed slot is used again");
        assert_eq!(out(&mut it, "kept(0)"), "1");
    });
}

#[test]
fn values_objects_and_strings_through_a_scope() {
    big(|| {
        let (mut it, _) = setup();
        enter(&mut it, |s| {
            let o = s.new_object().unwrap();
            let (k, v) = (s.string("x").unwrap(), s.number(1.5));
            s.set(o, k, v).unwrap();
            let got = s.get(o, k).unwrap();
            assert_eq!(s.as_number(got), Some(1.5));
            let t = s.string("héllo").unwrap();
            assert_eq!(s.as_str(t).unwrap().to_string_lossy(), "héllo");
            let n = s.null();
            assert_eq!(s.type_of(n), Type::Null);
            assert!(matches!(s.get(n, k), Err(bare_js_api::Error::Throw(_))), "null.x throws");
            let e = s.boolean(true);
            assert!(matches!(s.define(e, k, v, Attrs::ALL), Err(bare_js_api::Error::Throw(_))));
        });
        assert_eq!(it.stack_len(), 0);
    });
}

/// A host that keeps one traced value per owner: `own(o, v)` makes `o` an owner of `v`.
#[derive(Default)]
struct Owners {
    refs: RefCell<Vec<Option<bare_js_api::Traced<Value>>>>,
    dead: RefCell<Vec<u32>>,
}

impl<'p> bare_js_api::Host<Interp<'p>> for Owners {
    fn call<'s>(&self, s: &mut Scope<'s, Interp<'p>>, _id: u32, _this: Local<'s>, args: Args<'s>, _is_new: bool) -> Result<Local<'s>, bare_js_api::Error<'s>> {
        let (o, v) = (args.get(0).unwrap(), args.get(1).unwrap());
        let key = self.refs.borrow().len() as u32;
        s.set_host_owner(o, key)?;
        let t = s.trace_ref(v);
        self.refs.borrow_mut().push(Some(t));
        Ok(s.undefined())
    }

    fn trace(&self, key: u32, t: &mut bare_js_api::Tracer<'_, Value>) {
        if let Some(Some(r)) = self.refs.borrow().get(key as usize) {
            t.visit(r);
        }
    }

    fn prune(&self, dead: &[u32]) {
        for &k in dead {
            self.refs.borrow_mut()[k as usize] = None;
        }
        self.dead.borrow_mut().extend_from_slice(dead);
    }
}

#[test]
fn an_owner_keeps_its_traced_values_while_it_lives() {
    big(|| {
        let mut it = Interp::realm(Heap::default(), EvalLimits { max_steps: 100_000, ..Default::default() }).unwrap();
        let h = Rc::new(Owners::default());
        enter(&mut it, |s| {
            s.set_host(h.clone());
            let (g, f, k) = (s.global(), s.host_function("own", 0, 2).unwrap(), s.string("own").unwrap());
            s.define(g, k, f, Attrs::METHOD).unwrap();
        });
        // Key 0 has no weak reference to its value (a reached WeakRef target is a heap root). Key 1 shows the value go.
        out(&mut it, "var o = {}; own(o, { tag: 'kept' }); var p = {}, v = { tag: 'weak' }, w = new WeakRef(v); own(p, v); v = null; 0");
        it.collect(&[]).unwrap();
        assert_eq!(out(&mut it, "w.deref() && w.deref().tag"), "weak", "the owner keeps the value");
        // The heap itself keeps the value too: new objects do not take its slot.
        out(&mut it, "for (var i = 0; i < 1000; i++) ({ junk: i }); 0");
        let tag = enter(&mut it, |s| {
            let r = h.refs.borrow();
            let (v, k) = (s.traced(r[0].as_ref().unwrap()), s.string("tag").unwrap());
            let t = s.get(v, k).unwrap();
            s.as_str(t).unwrap().to_string_lossy()
        });
        assert_eq!(tag, "kept");
        assert!(h.dead.borrow().is_empty());
        out(&mut it, "o = null; p = null; 0");
        it.collect(&[]).unwrap();
        let mut dead = h.dead.borrow().clone();
        dead.sort();
        assert_eq!(dead, vec![0, 1], "a dead owner's key goes to prune");
        assert_eq!(out(&mut it, "w.deref() === undefined"), "true", "the value goes with its owner");
        assert!(it.api.owners.is_empty());
    });
}

/// A host that answers host-filled properties by key: an indexed object has `key` items, each the string `text`; a named
/// object has the one name `k`, whose value is its key. Key 9 is busy.
#[derive(Default)]
struct Filled {
    text: std::cell::Cell<Option<Value>>,
}

impl<'p> bare_js_api::Host<Interp<'p>> for Filled {
    fn call<'s>(&self, s: &mut Scope<'s, Interp<'p>>, _id: u32, _this: Local<'s>, _args: Args<'s>, _is_new: bool) -> Result<Local<'s>, bare_js_api::Error<'s>> {
        Ok(s.undefined())
    }
    fn indexed_len(&self, _object: &Value, key: u32) -> Result<usize, bare_js_api::Pending> {
        if key == 9 { Err(bare_js_api::Pending::Busy) } else { Ok(key as usize) }
    }
    fn indexed_item(&self, _object: &Value, key: u32, index: usize) -> Result<bare_js_api::Item<Value>, bare_js_api::Pending> {
        Ok(if index < key as usize { bare_js_api::Item::Value(self.text.get().unwrap()) } else { bare_js_api::Item::Absent })
    }
    fn named_has(&self, _object: &Value, _key: u32, name: &[u16]) -> Result<bool, bare_js_api::Pending> {
        Ok(name == [u16::from(b'k')])
    }
    fn named_get(&self, _object: &Value, key: u32, _name: &[u16]) -> Result<Option<bare_js_api::Named<Value>>, bare_js_api::Pending> {
        Ok(Some(bare_js_api::Named::Text(key.to_string().encode_utf16().collect())))
    }
    fn named_len(&self, _object: &Value, _key: u32) -> Result<usize, bare_js_api::Pending> {
        Ok(1)
    }
    fn named_keys(&self, _object: &Value, _key: u32) -> Result<Vec<Vec<u16>>, bare_js_api::Pending> {
        Ok(vec![vec![u16::from(b'k')]])
    }
}

#[test]
fn host_filled_properties_are_answered_by_the_key_of_each_object() {
    big(|| {
        let mut it = Interp::realm(Heap::default(), EvalLimits { max_steps: 100_000, ..Default::default() }).unwrap();
        let h = Rc::new(Filled::default());
        h.text.set(Some(Value::String(it.heap.intern_str("x").unwrap())));
        it.set_api_sources(h.clone());
        let objs = [("two", it.heap.new_indexed(None, 2).unwrap()), ("three", it.heap.new_indexed(None, 3).unwrap()), ("busy", it.heap.new_indexed(None, 9).unwrap()), ("five", it.heap.new_named(None, 5).unwrap()), ("seven", it.heap.new_named(None, 7).unwrap())];
        enter(&mut it, |s| {
            let g = s.global();
            for (name, o) in objs {
                let (k, v) = (s.string(name).unwrap(), s.traced(&bare_js_api::Traced::from_raw(Value::Object(o))));
                s.define(g, k, v, Attrs::METHOD).unwrap();
            }
        });
        assert_eq!(out(&mut it, "Object.keys(two).length + ',' + Object.keys(three).length + ',' + three[2] + ',' + three[3]"), "2,3,x,undefined");
        assert_eq!(out(&mut it, "five.k + ',' + seven.k + ',' + ('k' in five) + ',' + ('j' in five) + ',' + Object.keys(seven)"), "5,7,true,false,k");
        assert!(out(&mut it, "Object.keys(busy)").starts_with("other:"), "a busy host stops the script");
    });
}

/// A host with lazy members: it logs what the engine asks, and defines a data property for each ask on the object that was asked.
#[derive(Default)]
struct Lazies {
    asked: RefCell<Vec<String>>,
}

impl<'p> bare_js_api::Host<Interp<'p>> for Lazies {
    fn call<'s>(&self, s: &mut Scope<'s, Interp<'p>>, _id: u32, _this: Local<'s>, _args: Args<'s>, _is_new: bool) -> Result<Local<'s>, bare_js_api::Error<'s>> {
        Ok(s.undefined())
    }
    fn lazy_member<'s>(&self, s: &mut Scope<'s, Interp<'p>>, object: Local<'s>, what: bare_js_api::Lazy<'_>) -> Result<(), bare_js_api::Error<'s>> {
        use bare_js_api::Lazy;
        let (log, name) = match what {
            Lazy::Name(n) if n == "onboom" => return Err(bare_js_api::Error::Throw(s.string("boom")?)),
            Lazy::Name(n) => (format!("name {n}"), n),
            Lazy::All => ("all".to_string(), "onall"),
            Lazy::Symbols => ("symbols".to_string(), "ontags"),
        };
        self.asked.borrow_mut().push(log);
        let (k, v) = (s.string(name)?, s.number(1.0));
        s.define(object, k, v, Attrs::METHOD)
    }
}

#[test]
fn lazy_members_are_asked_through_the_interface_once_per_name() {
    big(|| {
        let mut it = Interp::realm(Heap::default(), EvalLimits { max_steps: 100_000, ..Default::default() }).unwrap();
        let h = Rc::new(Lazies::default());
        let (a, b) = (it.heap.new_object(None).unwrap(), it.heap.new_object(None).unwrap());
        it.heap.set_lazy_on(a).unwrap();
        it.heap.set_lazy_on(b).unwrap();
        enter(&mut it, |s| {
            s.set_host(h.clone());
            let g = s.global();
            for (name, o) in [("a", a), ("b", b)] {
                let (k, v) = (s.string(name).unwrap(), s.traced(&bare_js_api::Traced::from_raw(Value::Object(o))));
                s.define(g, k, v, Attrs::METHOD).unwrap();
            }
        });
        // A name: the host defines it on the asked object, and the engine finds it. A second read and a name that the host did not
        // define ask no more. `b` has its own ask.
        assert_eq!(out(&mut it, "Object.getOwnPropertyDescriptor(a, 'onclick').value + ',' + (Object.getOwnPropertyDescriptor(a, 'onclick') !== undefined) + ',' + Object.getOwnPropertyDescriptor(b, 'onclick').value"), "1,true,1");
        assert_eq!(*h.asked.borrow(), ["name onclick", "name onclick"]);
        // All names: once, however often the keys are listed.
        assert_eq!(out(&mut it, "Object.getOwnPropertyNames(a).join() + '|' + Object.getOwnPropertyNames(a).join()"), "onclick,onall|onclick,onall");
        assert_eq!(h.asked.borrow().iter().filter(|l| *l == "all").count(), 1);
        // A symbol key asks for the symbol members once.
        assert_eq!(out(&mut it, "Object.getOwnPropertyDescriptor(a, Symbol.toStringTag) === undefined"), "true");
        assert_eq!(h.asked.borrow().iter().filter(|l| *l == "symbols").count(), 1);
        // A throw of the host reaches the script with its value.
        assert_eq!(out(&mut it, "try { Object.getOwnPropertyDescriptor(a, 'onboom'); 'no' } catch (e) { e }"), "boom");
    });
}

// ---- The base set of handler methods (plan step S0.4). One host function for each method; a script calls it and compares with the
// script's own operation.

const BASE_FNS: [&str; 19] = [
    "typeErr", "rangeErr", "charge", "num", "str", "bool", "arr", "isArr", "getProto", "setProto", "accessor", "ntProbe", "utf16", "now", "hasOwn", "ownKeys", "ownEnum", "isCallable", "intr",
];

/// A host written only against the interface: function `i` of `BASE_FNS` calls one base method.
struct Base;

impl<E: Engine> bare_js_api::Host<E> for Base {
    fn call<'s>(&self, s: &mut Scope<'s, E>, id: u32, _this: Local<'s>, args: Args<'s>, _is_new: bool) -> Result<Local<'s>, bare_js_api::Error<'s>> {
        let arg = |s: &mut Scope<'s, E>, i| match args.get(i) {
            Some(v) => v,
            None => s.undefined(),
        };
        let (a, b, c, d) = (arg(s, 0), arg(s, 1), arg(s, 2), arg(s, 3));
        match id {
            0 | 1 => {
                let msg = s.as_str(a).map(|t| t.to_string_lossy()).unwrap_or_default();
                Err(if id == 0 { s.type_error(&msg) } else { s.range_error(&msg) })
            }
            2 => {
                let n = s.as_number(a).unwrap_or(0.0) as u64;
                s.charge(n)?;
                Ok(s.undefined())
            }
            3 => {
                let n = s.to_number(a)?;
                Ok(s.number(n))
            }
            4 => s.to_string(a),
            5 => {
                let t = s.to_boolean(a);
                Ok(s.boolean(t))
            }
            6 => {
                let items: Vec<_> = args.iter().collect();
                s.new_array(&items)
            }
            7 => {
                let t = s.is_array(a)?;
                Ok(s.boolean(t))
            }
            8 => match s.get_prototype_of(a)? {
                Some(p) => Ok(p),
                None => Ok(s.null()),
            },
            9 => {
                let p = if s.type_of(b) == Type::Null { None } else { Some(b) };
                let t = s.set_prototype_of(a, p)?;
                Ok(s.boolean(t))
            }
            10 => {
                let opt = |s: &Scope<'s, E>, v| if s.type_of(v) == Type::Undefined { None } else { Some(v) };
                let (g, st) = (opt(s, c), opt(s, d));
                s.define_accessor(a, b, g, st, Attrs { writable: false, enumerable: false, configurable: true })?;
                Ok(s.undefined())
            }
            // The constructor: the default prototype is Array.prototype.
            11 => {
                let default = s.intrinsic(bare_js_api::Intrinsic::ArrayPrototype);
                let proto = s.proto_from_new_target(default)?;
                let o = s.new_object()?;
                s.set_prototype_of(o, Some(proto))?;
                Ok(o)
            }
            // utf16(): "\ud800a"
            12 => s.string_utf16(&[0xD800, u16::from(b'a')]),
            13 => {
                let t = s.wall_now_ms();
                Ok(s.number(t))
            }
            14 => {
                let t = s.has_own(a, b)?;
                Ok(s.boolean(t))
            }
            15 => {
                let keys = s.own_keys(a)?;
                s.new_array(&keys)
            }
            16 => match s.own_enumerable_data(a, b)? {
                Some(v) => Ok(v),
                None => s.string("<none>"),
            },
            17 => {
                let t = s.is_callable(a);
                Ok(s.boolean(t))
            }
            _ => {
                use bare_js_api::Intrinsic as I;
                let which = [I::ObjectPrototype, I::ArrayPrototype, I::IteratorPrototype, I::ErrorPrototype, I::SymbolIterator][s.as_number(a).unwrap_or(0.0) as usize];
                Ok(s.intrinsic(which))
            }
        }
    }
}

fn base_setup() -> Interp<'static> {
    let mut it = Interp::realm(Heap::default(), EvalLimits { max_steps: 1_000_000, ..Default::default() }).unwrap();
    it.set_wall_clock(crate::eval::WallClock::Virtual(1234.0)).unwrap();
    enter(&mut it, |s| {
        s.set_host(Rc::new(Base));
        let g = s.global();
        for (i, name) in BASE_FNS.into_iter().enumerate() {
            let f = s.host_function(name, i as u32, 1).unwrap();
            let k = s.string(name).unwrap();
            s.define(g, k, f, Attrs::METHOD).unwrap();
        }
    });
    // A constructor that accepts a `new.target` of a subclass: the interface makes no constructors yet, so the test makes it directly.
    let f = it.host_ctor_with_new_target(11, "NtCtor", 0).unwrap();
    let g = it.global_object();
    let k = it.heap.key_from_str("NtCtor").unwrap();
    it.define_data(g, k, Value::Object(f), true, false, true).unwrap();
    // The owner of a host constructor defines its `prototype`.
    let proto = it.heap.new_object(Some(it.object_prototype())).unwrap();
    let kp = it.heap.key_from_str("prototype").unwrap();
    it.define_data(f, kp, Value::Object(proto), false, false, false).unwrap();
    it
}

#[test]
fn errors_made_by_the_host_reach_the_script_with_their_constructor() {
    big(|| {
        let mut it = base_setup();
        assert_eq!(out(&mut it, "try { typeErr('bad') } catch (e) { [e instanceof TypeError, e instanceof RangeError, e.name, e.message, typeof e.stack].join() }"), "true,false,TypeError,bad,string");
        assert_eq!(out(&mut it, "try { rangeErr('far') } catch (e) { [e instanceof RangeError, e instanceof TypeError, e.name, e.message].join() }"), "true,false,RangeError,far");
        assert_eq!(out(&mut it, "try { typeErr('uncaught') } catch (e) { String(e) }"), "TypeError: uncaught");
        assert_eq!(it.stack_len(), 0);
    });
}

#[test]
fn charge_adds_to_the_step_budget_and_stops_at_the_limit() {
    big(|| {
        let mut it = base_setup();
        assert_eq!(out(&mut it, "charge(10); 'ok'"), "ok");
        assert_eq!(out(&mut it, "charge(1e9)"), "limit:Steps");
        assert_eq!(out(&mut it, "try { charge(1e9) } catch (e) { 'caught' }"), "limit:Steps", "a script cannot catch the limit");
        assert_eq!(out(&mut it, "charge(5); 'again'"), "again", "the next run has a new budget");
        assert_eq!(it.stack_len(), 0);
    });
}

#[test]
fn conversions_match_the_script() {
    big(|| {
        let mut it = base_setup();
        // `+v` and a template string are ToNumber and ToString; `Number(v)` and `String(v)` differ on a BigInt and a symbol.
        // `same`: the same value, or the same kind of error. `Object.is` tells NaN and -0 apart.
        let prelude = "function t(f, v) { try { return f(v) } catch (e) { return 'E:' + e.constructor.name } } \
                       function same(a, b) { return Object.is(a, b) } \
                       var n = 0; var vals = [NaN, -0, 0, 1.5, -1/0, '', ' 12 ', '0x10', '1e3', 'abc', '\\n', null, undefined, true, false, [], [5], [1, 2], {}, \
                         { valueOf() { return 7 } }, { toString() { return '8' } }, { valueOf() { return {} }, toString() { return '9' } }, \
                         { valueOf() { throw new RangeError('v') } }, Symbol('s'), 10n, new Date(5), function () {}];";
        assert_eq!(out(&mut it, &format!("{prelude} vals.filter(v => !same(t(num, v), t(v => +v, v))).length")), "0");
        assert_eq!(out(&mut it, &format!("{prelude} vals.filter(v => !same(t(str, v), t(v => `${{v}}`, v))).length")), "0");
        assert_eq!(out(&mut it, &format!("{prelude} vals.filter(v => !same(bool(v), Boolean(v))).length")), "0");
        assert_eq!(out(&mut it, "[num(NaN), 1/num(-0), str(-0), str(null), str([1, [2]]), bool(''), bool('0'), bool(0n), bool(-0)].join()"), "NaN,-Infinity,0,null,1,2,false,true,false,false");
        assert_eq!(out(&mut it, "var calls = 0; num({ valueOf() { calls++; return 1 } }); str({ toString() { calls++; return 'x' } }); calls"), "2", "a conversion runs the script once");
        assert_eq!(out(&mut it, "bool({ valueOf() { throw 1 } })"), "true", "ToBoolean runs no script");
        assert_eq!(it.stack_len(), 0);
    });
}

#[test]
fn arrays_are_made_and_tested() {
    big(|| {
        let mut it = base_setup();
        assert_eq!(out(&mut it, "var a = arr(1, 'x', {}, null); [Array.isArray(a), a.length, a[0], a[1], typeof a[2], String(a[3]), Object.getPrototypeOf(a) === Array.prototype].join()"), "true,4,1,x,object,null,true");
        assert_eq!(out(&mut it, "arr().length"), "0");
        assert_eq!(out(&mut it, "[isArr([]), isArr(arr()), isArr({}), isArr(1), isArr('s'), isArr(null), isArr(new Proxy([], {})), isArr(new Proxy({}, {})), isArr({ length: 0 })].join()"), "true,true,false,false,false,false,true,false,false");
        assert_eq!(out(&mut it, "var r = Proxy.revocable([], {}); r.revoke(); try { isArr(r.proxy) } catch (e) { e.constructor.name }"), "TypeError");
        assert_eq!(it.stack_len(), 0);
    });
}

#[test]
fn prototypes_are_read_and_set() {
    big(|| {
        let mut it = base_setup();
        assert_eq!(out(&mut it, "[getProto([]) === Array.prototype, getProto(Object.create(null)), getProto(Object.prototype), getProto(function () {}) === Function.prototype].map(String).join()"), "true,null,null,true");
        assert_eq!(out(&mut it, "var p = {}, o = {}; [setProto(o, p), Object.getPrototypeOf(o) === p, setProto(o, null), Object.getPrototypeOf(o)].map(String).join()"), "true,true,true,null");
        assert_eq!(out(&mut it, "var a = {}, b = Object.create(a); setProto(a, b)"), "false", "a cycle is refused");
        assert_eq!(out(&mut it, "setProto(Object.preventExtensions({}), {})"), "false", "a non-extensible object keeps its prototype");
        assert_eq!(out(&mut it, "try { setProto({}, 5) } catch (e) { e.constructor.name }"), "TypeError");
        assert_eq!(out(&mut it, "try { getProto(5) } catch (e) { e.constructor.name }"), "TypeError");
        // A proxy runs its trap.
        assert_eq!(out(&mut it, "var hit = 0, base = {}; getProto(new Proxy({}, { getPrototypeOf() { hit++; return base } })) === base && hit === 1"), "true");
        assert_eq!(it.stack_len(), 0);
    });
}

#[test]
fn an_accessor_is_called_with_the_receiver_as_this() {
    big(|| {
        let mut it = base_setup();
        assert_eq!(out(&mut it, "var o = {}; accessor(o, 'g', function () { return this }, function (v) { this.seen = v }); var c = Object.create(o); c.g = 5; [o.g === o, c.g === c, c.seen, 'seen' in o].join()"), "true,true,5,false");
        let d = "var o = {}; accessor(o, 'g', function () { return 1 }); var d = Object.getOwnPropertyDescriptor(o, 'g');";
        assert_eq!(out(&mut it, &format!("{d} [typeof d.get, d.set, d.enumerable, d.configurable, 'writable' in d].join()")), "function,,false,true,false");
        assert_eq!(out(&mut it, "var o = {}; accessor(o, 'g', undefined, function () {}); o.g"), "undefined", "no getter reads undefined");
        assert_eq!(out(&mut it, "try { accessor({}, 'g', 5) } catch (e) { e.constructor.name }"), "TypeError", "a getter must be callable");
        assert_eq!(out(&mut it, "var o = {}; Object.defineProperty(o, 'g', { value: 1 }); try { accessor(o, 'g', function () {}) } catch (e) { e.constructor.name }"), "TypeError", "a fixed property stays");
        assert_eq!(out(&mut it, "var o = {}; accessor(o, Symbol.iterator, function () { return 3 }); o[Symbol.iterator]"), "3");
        assert_eq!(it.stack_len(), 0);
    });
}

#[test]
fn a_constructor_takes_its_prototype_from_new_target() {
    big(|| {
        let mut it = base_setup();
        assert_eq!(out(&mut it, "Object.getPrototypeOf(new NtCtor()) === Array.prototype"), "true", "a plain new gets the default");
        assert_eq!(out(&mut it, "class B extends NtCtor {} var b = new B(); Object.getPrototypeOf(b) === B.prototype && b instanceof B"), "true");
        assert_eq!(out(&mut it, "class C extends B {} Object.getPrototypeOf(new C()) === C.prototype"), "true");
        assert_eq!(out(&mut it, "var T = function () {}; T.prototype = { tag: 1 }; Reflect.construct(NtCtor, [], T).tag"), "1");
        assert_eq!(out(&mut it, "var T = function () {}; T.prototype = 5; Object.getPrototypeOf(Reflect.construct(NtCtor, [], T)) === Object.prototype"), "true", "a new.target without an object prototype gives Object.prototype");
        assert_eq!(out(&mut it, "var T = new Proxy(function () {}, { get(t, k) { if (k === 'prototype') throw new RangeError('p'); return t[k] } }); try { Reflect.construct(NtCtor, [], T) } catch (e) { e.constructor.name }"), "RangeError", "a getter error comes out");
        assert_eq!(it.stack_len(), 0);
    });
}

#[test]
fn strings_from_code_units_keep_lone_surrogates() {
    big(|| {
        let mut it = base_setup();
        assert_eq!(out(&mut it, "var s = utf16(); [s.length, s.charCodeAt(0), s.charCodeAt(1), typeof s].join()"), "2,55296,97,string");
        assert_eq!(out(&mut it, "utf16() === utf16()"), "true");
        assert_eq!(it.stack_len(), 0);
    });
}

#[test]
fn the_wall_clock_is_the_one_of_date_now() {
    big(|| {
        let mut it = base_setup();
        assert_eq!(out(&mut it, "[now(), Date.now()].join()"), "1234,1234");
        it.set_wall_clock(crate::eval::WallClock::Virtual(99.0)).unwrap();
        assert_eq!(out(&mut it, "now() === Date.now() && now()"), "99");
    });
}

#[test]
fn own_property_tests_run_no_getter_and_no_trap() {
    big(|| {
        let mut it = base_setup();
        assert_eq!(out(&mut it, "[hasOwn({ a: 1 }, 'a'), hasOwn({}, 'a'), hasOwn(Object.create({ a: 1 }), 'a'), hasOwn([7], 0), hasOwn([7], 1), hasOwn([], 'length'), hasOwn({ [Symbol.iterator]: 1 }, Symbol.iterator)].join()"), "true,false,false,true,false,true,true");
        assert_eq!(out(&mut it, "var n = 0; var o = { get a() { n++ } }; [hasOwn(o, 'a'), n].join()"), "true,0");
        assert_eq!(out(&mut it, "var n = 0; var h = { has() { n++ }, getOwnPropertyDescriptor() { n++ } }; [hasOwn(new Proxy({ a: 1 }, h), 'a'), n].join()"), "false,0");
        assert_eq!(out(&mut it, "try { hasOwn(1, 'a') } catch (e) { e.constructor.name }"), "TypeError");
        assert_eq!(it.stack_len(), 0);
    });
}

#[test]
fn own_keys_come_in_the_order_of_reflect_own_keys() {
    big(|| {
        let mut it = base_setup();
        let show = "function show(o) { return ownKeys(o).map(k => typeof k === 'symbol' ? 'sym' : typeof k + ':' + k).join() } ";
        assert_eq!(out(&mut it, &format!("{show} var s = Symbol('s'); var o = {{ b: 1, 2: 1, a: 1, [s]: 1, 1: 1 }}; show(o)")), "string:1,string:2,string:b,string:a,sym");
        assert_eq!(out(&mut it, &format!("{show} show([10, 20])")), "string:0,string:1,string:length");
        assert_eq!(out(&mut it, &format!("{show} var o = {{}}; Object.defineProperty(o, 'hid', {{ value: 1 }}); show(o)")), "string:hid", "a hidden key is a key");
        assert_eq!(out(&mut it, "var s = Symbol('x'), o = { [s]: 1 }; ownKeys(o)[0] === s"), "true");
        assert_eq!(out(&mut it, "JSON.stringify(ownKeys(Object.create({ inherited: 1 })))"), "[]");
        assert_eq!(out(&mut it, "var log = []; var p = new Proxy({}, { ownKeys() { log.push('trap'); return ['z'] }, getOwnPropertyDescriptor() { return { value: 1, configurable: true } } }); ownKeys(p).join() + log"), "ztrap");
        assert_eq!(out(&mut it, "try { ownKeys(1) } catch (e) { e.constructor.name }"), "TypeError");
        assert_eq!(it.stack_len(), 0);
    });
}

#[test]
fn an_own_enumerable_property_is_read_with_its_getter() {
    big(|| {
        let mut it = base_setup();
        assert_eq!(out(&mut it, "[ownEnum({ a: 1 }, 'a'), ownEnum({ a: undefined }, 'a'), ownEnum([5], 0), ownEnum({ a: 1 }, 'b')].join()"), "1,,5,<none>");
        assert_eq!(out(&mut it, "ownEnum(Object.create({ a: 1 }), 'a')"), "<none>", "an inherited property is not own");
        assert_eq!(out(&mut it, "var o = {}; Object.defineProperty(o, 'h', { value: 1 }); ownEnum(o, 'h')"), "<none>", "a hidden property is skipped");
        assert_eq!(out(&mut it, "var n = 0; var o = { get x() { n++; return 'got' } }; [ownEnum(o, 'x'), n].join()"), "got,1");
        assert_eq!(out(&mut it, "try { ownEnum({ get x() { throw new RangeError('g') } }, 'x') } catch (e) { e.constructor.name }"), "RangeError");
        assert_eq!(out(&mut it, "var s = Symbol(); ownEnum({ [s]: 'sym' }, s)"), "sym");
        assert_eq!(it.stack_len(), 0);
    });
}

#[test]
fn callable_values_are_told_from_the_rest() {
    big(|| {
        let mut it = base_setup();
        assert_eq!(out(&mut it, "[function () {}, () => 1, class A {}, async function () {}, function* () {}, Math.max, isCallable, (function () {}).bind(null), new Proxy(function () {}, {}), new Proxy(class {}, {})].map(isCallable).join()"), "true,true,true,true,true,true,true,true,true,true");
        assert_eq!(out(&mut it, "[{}, [], null, undefined, 1, 's', true, Symbol(), new Proxy({}, {}), { call() {} }, Math, new (class {})()].map(isCallable).join()"), "false,false,false,false,false,false,false,false,false,false,false,false");
        assert_eq!(it.stack_len(), 0);
    });
}

#[test]
fn the_intrinsics_are_the_ones_of_the_realm() {
    big(|| {
        let mut it = base_setup();
        assert_eq!(out(&mut it, "intr(0) === Object.prototype"), "true");
        assert_eq!(out(&mut it, "intr(1) === Array.prototype"), "true");
        assert_eq!(out(&mut it, "intr(2) === Object.getPrototypeOf(Object.getPrototypeOf([][Symbol.iterator]()))"), "true");
        assert_eq!(out(&mut it, "intr(3) === Error.prototype"), "true");
        assert_eq!(out(&mut it, "intr(4) === Symbol.iterator"), "true");
        assert_eq!(out(&mut it, "var s = Symbol; Symbol = undefined; var r = intr(4) === s.iterator; Symbol = s; r"), "true", "a script cannot change it");
        assert_eq!(it.stack_len(), 0);
    });
}

// ---------------------------------------------------------------------------------------- promises, microtasks and iteration

fn global<'s, E: Engine>(s: &mut Scope<'s, E>, name: &str) -> Local<'s> {
    let g = s.global();
    s.get_str(g, name).unwrap()
}

#[test]
fn the_pending_rejection_bound_is_the_one_of_the_interface() {
    assert_eq!(super::MAX_PENDING_REJECTIONS, bare_js_api::MAX_PENDING_REJECTIONS);
}

#[test]
fn a_promise_of_the_interface_settles_and_its_reactions_run_on_the_drain() {
    big(|| {
        let (mut it, _) = setup();
        out(&mut it, "var log = []; var ok = v => log.push('ok:' + v); var bad = v => log.push('bad:' + v); 0");
        enter(&mut it, |s| {
            let (ok, bad) = (global(s, "ok"), global(s, "bad"));
            let p = s.new_promise().unwrap();
            assert_eq!(s.promise_status(p), Some(PromiseState::Pending));
            s.promise_then(p, ok, bad).unwrap();
            let seven = s.number(7.0);
            s.resolve_promise(p, seven).unwrap();
            let Some(PromiseState::Fulfilled(v)) = s.promise_status(p) else { panic!("fulfilled") };
            assert_eq!(s.as_number(v), Some(7.0));
            assert_eq!(s.pending_jobs(), 1, "the reaction waits for the drain");
            // A promise that is settled already: the reaction is queued by `promise_then`.
            let q = s.new_promise().unwrap();
            let why = s.string("no").unwrap();
            s.reject_promise(q, why).unwrap();
            s.promise_then(q, ok, bad).unwrap();
            let Some(PromiseState::Rejected(r)) = s.promise_status(q) else { panic!("rejected") };
            assert_eq!(s.as_str(r).unwrap().to_string_lossy(), "no");
            // A value that is not a promise: `react` wraps it, `status` says no.
            let n = s.number(3.0);
            assert_eq!(s.promise_status(n), None);
            s.promise_react(n, ok, bad).unwrap();
            assert!(matches!(s.resolve_promise(n, n), Err(bare_js_api::Error::Abort(bare_js_api::Abort::Unsupported(_)))));
            assert_eq!(s.drain_microtasks().unwrap(), 3);
            assert_eq!(s.pending_jobs(), 0);
        });
        assert_eq!(out(&mut it, "log.join()"), "ok:7,bad:no,ok:3");
        assert_eq!(it.stack_len(), 0);
    });
}

#[test]
fn a_microtask_runs_once_on_the_drain() {
    big(|| {
        let (mut it, _) = setup();
        out(&mut it, "var n = 0; var tick = () => { n++ }; 0");
        enter(&mut it, |s| {
            let tick = global(s, "tick");
            assert_eq!(s.pending_jobs(), 0);
            s.queue_microtask(tick).unwrap();
            assert_eq!(s.pending_jobs(), 1);
            let not_a_function = s.number(1.0);
            assert!(matches!(s.queue_microtask(not_a_function), Err(bare_js_api::Error::Throw(_))));
            assert_eq!(s.drain_microtasks().unwrap(), 1);
            assert_eq!(s.drain_microtasks().unwrap(), 0);
            assert_eq!(s.pending_jobs(), 0);
        });
        assert_eq!(out(&mut it, "n"), "1");
    });
}

/// `run(f)`: queue `f` as a microtask, then run a checkpoint from the host and give the number of jobs that ran.
struct Checkpointer;

impl<'p> bare_js_api::Host<Interp<'p>> for Checkpointer {
    fn call<'s>(&self, s: &mut Scope<'s, Interp<'p>>, _id: u32, _this: Local<'s>, args: Args<'s>, _is_new: bool) -> Result<Local<'s>, bare_js_api::Error<'s>> {
        s.queue_microtask(args.get(0).unwrap())?;
        let n = s.checkpoint()?;
        Ok(s.number(n as f64))
    }
}

#[test]
fn a_checkpoint_inside_a_script_function_does_nothing() {
    big(|| {
        let mut it = Interp::realm(Heap::default(), EvalLimits { max_steps: 100_000, ..Default::default() }).unwrap();
        enter(&mut it, |s| {
            s.set_host(Rc::new(Checkpointer));
            let (g, f, k) = (s.global(), s.host_function("run", 0, 1).unwrap(), s.string("run").unwrap());
            s.define(g, k, f, Attrs::METHOD).unwrap();
        });
        assert_eq!(out(&mut it, "var n = 0; function outer() { return run(() => { n++ }) } var ran = outer(); ran + ',' + n"), "0,0", "a script function is running: no job runs");
        assert_eq!(it.pending_jobs(), 1);
        // From a host scope with no script below (a driver dispatching an event) the same call runs the queue.
        let ran = it.host_scope(|it| Ok(enter(it, |s| s.checkpoint().unwrap()))).unwrap();
        assert_eq!(ran, 1);
        assert_eq!(out(&mut it, "n"), "1");
    });
}

const ITERABLES: &str = "var arr = [10, 20, 30]; var closed = 0, fin = 0;
    function* g() { try { yield 1; yield 2; yield 3 } finally { fin++ } }
    var custom = { [Symbol.iterator]() { return { i: 0, next() { return { value: this.i++, done: this.i > 5 } }, return() { closed++; return {} } } } };
    var set = new Set([5, 6]); 0";

fn items<'s, E: Engine>(s: &mut Scope<'s, E>, v: Local<'s>) -> Vec<String> {
    let mut it = s.iter_open(v).unwrap().expect("iterable");
    let mut out = Vec::new();
    while let Some(x) = s.iter_next(&mut it).unwrap() {
        out.push(match s.as_number(x) {
            Some(n) => n.to_string(),
            None => s.as_str(x).unwrap().to_string_lossy(),
        });
    }
    assert!(s.iter_next(&mut it).unwrap().is_none(), "an ended iteration stays ended");
    out
}

#[test]
fn an_iteration_steps_an_array_a_string_a_set_and_a_generator() {
    big(|| {
        let (mut it, _) = setup();
        out(&mut it, ITERABLES);
        enter(&mut it, |s| {
            let arr = global(s, "arr");
            assert_eq!(items(s, arr), ["10", "20", "30"]);
            let text = s.string("a😀").unwrap();
            assert_eq!(items(s, text), ["a", "😀"]);
            let set = global(s, "set");
            assert_eq!(items(s, set), ["5", "6"]);
            let (g, this) = (global(s, "g"), s.undefined());
            let gen_ = s.call(g, this, &[]).unwrap();
            assert_eq!(items(s, gen_), ["1", "2", "3"]);
            let custom = global(s, "custom");
            assert_eq!(items(s, custom), ["0", "1", "2", "3", "4"]);
            let (u, n) = (s.undefined(), s.number(5.0));
            assert!(s.iter_open(u).unwrap().is_none());
            assert!(s.iter_open(n).unwrap().is_none(), "a number has no Symbol.iterator");
        });
        assert_eq!(out(&mut it, "closed + ',' + fin"), "0,1", "an iteration that ends calls no `return`");
        assert_eq!(it.stack_len(), 0);
    });
}

#[test]
fn a_failed_iteration_calls_return_and_keeps_its_error() {
    big(|| {
        let (mut it, _) = setup();
        out(&mut it, ITERABLES);
        enter(&mut it, |s| {
            let boom = s.string("boom").unwrap();
            // A script iterator: `return` is called, and the error that comes back is the same value.
            let custom = global(s, "custom");
            let mut i = s.iter_open(custom).unwrap().unwrap();
            let first = s.iter_next(&mut i).unwrap().unwrap();
            assert_eq!(s.as_number(first), Some(0.0));
            let back = s.iter_fail(&mut i, bare_js_api::Error::Throw(boom));
            let bare_js_api::Error::Throw(v) = back else { panic!("the error stays a throw") };
            assert_eq!(s.as_str(v).unwrap().to_string_lossy(), "boom");
            assert!(s.iter_next(&mut i).unwrap().is_none(), "a closed iteration is done");
            // A generator: closing it runs its `finally`.
            let (g, this) = (global(s, "g"), s.undefined());
            let gen_ = s.call(g, this, &[]).unwrap();
            let mut i = s.iter_open(gen_).unwrap().unwrap();
            s.iter_next(&mut i).unwrap().unwrap();
            let back = s.iter_fail(&mut i, bare_js_api::Error::Throw(boom));
            assert!(matches!(back, bare_js_api::Error::Throw(_)));
            // An abort stays an abort and runs no script.
            let custom = global(s, "custom");
            let mut i = s.iter_open(custom).unwrap().unwrap();
            s.iter_next(&mut i).unwrap().unwrap();
            let back = s.iter_fail(&mut i, bare_js_api::Error::Abort(bare_js_api::Abort::Limit("steps")));
            assert!(matches!(back, bare_js_api::Error::Abort(_)));
        });
        assert_eq!(out(&mut it, "closed + ',' + fin"), "1,1", "`return` ran for the throw only, and `finally` ran");
        assert_eq!(it.stack_len(), 0);
    });
}

/// An old-style host whose `between_jobs` collects, as a host with a collection inside a task would.
#[derive(Default)]
struct Collector {
    collections: std::cell::Cell<usize>,
}

impl Host for Collector {
    fn call(&self, _it: &mut Interp<'_>, _id: u32, _this: Value, _args: &[Value], _is_new: bool) -> Result<Value, Error> {
        Ok(Value::Undefined)
    }
    fn between_jobs(&self, it: &mut Interp<'_>) -> Result<(), Error> {
        self.collections.set(self.collections.get() + 1);
        it.collect(&[]).map(|_| ())
    }
}

#[test]
fn a_local_promise_survives_a_collection_inside_the_drain() {
    big(|| {
        let (mut it, _) = setup();
        let c = Rc::new(Collector::default());
        it.set_host(c.clone()).unwrap();
        out(&mut it, "var log = []; var ok = v => log.push('ok:' + v.tag); var bad = () => {}; var tick = () => log.push('tick'); var w = new WeakRef({ tag: 'junk' }); 0");
        enter(&mut it, |s| {
            let (ok, bad, tick) = (global(s, "ok"), global(s, "bad"), global(s, "tick"));
            // The promise and its value are held by locals only: no script variable reaches them.
            let p = s.new_promise().unwrap();
            let (value, k, tag) = (s.new_object().unwrap(), s.string("tag").unwrap(), s.string("kept").unwrap());
            s.set(value, k, tag).unwrap();
            s.promise_then(p, ok, bad).unwrap();
            s.resolve_promise(p, value).unwrap();
            s.queue_microtask(tick).unwrap();
            assert_eq!(s.drain_microtasks().unwrap(), 2);
            let Some(PromiseState::Fulfilled(v)) = s.promise_status(p) else { panic!("fulfilled") };
            let t = s.get(v, k).unwrap();
            assert_eq!(s.as_str(t).unwrap().to_string_lossy(), "kept");
        });
        assert!(c.collections.get() >= 2, "the drain collected");
        assert_eq!(out(&mut it, "log.join() + ',' + (w.deref() === undefined)"), "ok:kept,tick,true", "a collection ran, and it freed what no handle held");
    });
}

// Modules through the interface (plan step S0.4). The host side is written against the interface only: a map of file names to sources.

type Files<'a> = &'a [(&'a str, &'a str)];

/// Parse `name` and what it imports, the way the browser loader does.
fn load<'s, E: Engine>(s: &mut Scope<'s, E>, files: Files, name: &str, cache: &mut HashMap<String, u32>) -> Result<u32, bare_js_api::Error<'s>> {
    if let Some(&m) = cache.get(name) {
        return Ok(m);
    }
    let src = files.iter().find(|f| f.0 == name).unwrap_or_else(|| panic!("no file {name}")).1;
    let m = s.module_parse(src, name)?;
    cache.insert(name.to_string(), m);
    let deps = s.module_requests(m).iter().map(|r| load(s, files, r, cache)).collect::<Result<Vec<_>, _>>()?;
    s.module_set_requested(m, &deps);
    Ok(m)
}

/// `Name:message` of a thrown error.
fn thrown<'s, E: Engine>(s: &mut Scope<'s, E>, e: bare_js_api::Error<'s>) -> String {
    let bare_js_api::Error::Throw(v) = e else { return "abort".to_string() };
    let (n, m) = (s.get_str(v, "name").unwrap(), s.get_str(v, "message").unwrap());
    format!("{}:{}", s.as_str(n).unwrap().to_string_lossy(), s.as_str(m).unwrap().to_string_lossy())
}

/// The state of a promise as a word.
fn state<'s, E: Engine>(s: &mut Scope<'s, E>, p: Local<'_>) -> String {
    match s.promise_status(p) {
        Some(PromiseState::Pending) => "pending".to_string(),
        Some(PromiseState::Fulfilled(_)) => "ok".to_string(),
        Some(PromiseState::Rejected(v)) => format!("rejected {}", thrown(s, bare_js_api::Error::Throw(v))),
        None => "no promise".to_string(),
    }
}

#[test]
fn modules_are_parsed_linked_and_evaluated_through_the_interface() {
    big(|| {
        let (mut it, _) = setup();
        out(&mut it, "var log = []; 0");
        let files: Files = &[("a", "import { b } from 'b'; log.push('a' + b);"), ("b", "export const b = 1; log.push('b');")];
        enter(&mut it, |s| {
            let mut cache = HashMap::new();
            let a = load(s, files, "a", &mut cache).unwrap();
            assert_eq!((s.module_requests(a), s.module_url(a).to_string()), (vec!["b".to_string()], "a".to_string()));
            s.module_link(a).unwrap();
            let p = s.module_evaluate(a).unwrap();
            assert_eq!(state(s, p), "ok", "no top-level await: settled at once");
            // A second call gives the promise of the first.
            let p2 = s.module_evaluate(a).unwrap();
            assert_eq!(state(s, p2), "ok");
            // The imports of a module that was not linked are an error of the host, not of the script.
            let c = s.module_parse("import 'b'; export {}", "c").unwrap();
            assert!(matches!(s.module_evaluate(c), Err(bare_js_api::Error::Abort(bare_js_api::Abort::Unsupported(_)))));
        });
        assert_eq!(out(&mut it, "log.join()"), "b,a1");
        assert_eq!(it.stack_len(), 0);
    });
}

#[test]
fn a_cycle_and_a_top_level_await_run_through_the_interface() {
    big(|| {
        let (mut it, _) = setup();
        out(&mut it, "var log = []; 0");
        let files: Files = &[
            ("a", "import { y } from 'b'; export const x = 'x'; log.push('a' + y);"),
            ("b", "import { x } from 'a'; export const y = 'y'; log.push('b'); export function f() { return x }"),
            ("w", "log.push('w1'); await null; log.push('w2');"),
        ];
        enter(&mut it, |s| {
            let mut cache = HashMap::new();
            let a = load(s, files, "a", &mut cache).unwrap();
            s.module_link(a).unwrap();
            let p = s.module_evaluate(a).unwrap();
            assert_eq!(state(s, p), "ok");
            let w = load(s, files, "w", &mut cache).unwrap();
            s.module_link(w).unwrap();
            let p = s.module_evaluate(w).unwrap();
            assert_eq!(state(s, p), "pending", "the module waits at its `await`");
            assert!(s.drain_microtasks().unwrap() > 0);
            assert_eq!(state(s, p), "ok");
        });
        assert_eq!(out(&mut it, "log.join()"), "b,ay,w1,w2");
    });
}

#[test]
fn module_errors_are_thrown_to_the_host() {
    big(|| {
        let (mut it, _) = setup();
        enter(&mut it, |s| {
            let e = s.module_parse("export const = ;", "bad").unwrap_err();
            assert!(thrown(s, e).starts_with("SyntaxError:"));
            let files: Files = &[("a", "import { nope } from 'b'"), ("b", "export const yes = 1"), ("t", "throw new RangeError('boom')")];
            let mut cache = HashMap::new();
            let a = load(s, files, "a", &mut cache).unwrap();
            let e = s.module_link(a).unwrap_err();
            assert!(thrown(s, e).starts_with("SyntaxError:"), "a name that the module does not export");
            let t = load(s, files, "t", &mut cache).unwrap();
            s.module_link(t).unwrap();
            let p = s.module_evaluate(t).unwrap();
            assert_eq!(state(s, p), "rejected RangeError:boom", "an error in the module rejects the promise");
        });
        assert_eq!(it.stack_len(), 0);
    });
}

#[test]
fn an_import_call_is_taken_and_then_finished_rejected_or_failed() {
    big(|| {
        let (mut it, _) = setup();
        out(&mut it, "var log = []; var note = tag => v => log.push(tag + ':' + (v && (v.name || v.v)));");
        out(
            &mut it,
            "import('ok').then(note('ok'), note('bad')); import('syntax').then(note('bad'), note('syntax')); import('net').then(note('bad'), note('net')); import('throws').then(note('bad'), note('throws'));",
        );
        let files: Files = &[("ok", "export const v = 7"), ("syntax", "export const = ;"), ("throws", "throw new RangeError('x')")];
        let reqs = enter(&mut it, |s| s.take_dynamic_imports());
        assert_eq!(reqs.iter().map(|r| (r.specifier.as_str(), r.referrer)).collect::<Vec<_>>(), [("ok", None), ("syntax", None), ("net", None), ("throws", None)]);
        assert!(enter(&mut it, |s| s.take_dynamic_imports()).is_empty(), "a request is taken once");
        enter(&mut it, |s| {
            let mut cache = HashMap::new();
            let m = load(s, files, "ok", &mut cache).unwrap();
            s.finish_dynamic_import(&reqs[0].promise, m).unwrap();
            let e = load(s, files, "syntax", &mut cache).unwrap_err();
            let bare_js_api::Error::Throw(reason) = e else { panic!("a throw") };
            s.reject_dynamic_import(&reqs[1].promise, reason).unwrap();
            s.fail_dynamic_import(&reqs[2].promise, "Failed to fetch net").unwrap();
            let m = load(s, files, "throws", &mut cache).unwrap();
            s.finish_dynamic_import(&reqs[3].promise, m).unwrap();
            s.drain_microtasks().unwrap();
        });
        assert_eq!(out(&mut it, "log.join()"), "syntax:SyntaxError,net:TypeError,ok:7,throws:RangeError", "a finished import settles after the evaluation promise: later than a rejected one");
        drop(reqs);
        assert_eq!(it.stack_len(), 0);
    });
}

#[test]
fn an_import_call_in_a_module_names_the_module_as_its_referrer() {
    big(|| {
        let (mut it, _) = setup();
        enter(&mut it, |s| {
            let m = s.module_parse("import('x');", "root").unwrap();
            s.module_link(m).unwrap();
            s.module_evaluate(m).unwrap();
            let reqs = s.take_dynamic_imports();
            assert_eq!(reqs.iter().map(|r| (r.specifier.as_str(), r.referrer)).collect::<Vec<_>>(), [("x", Some(m))]);
        });
    });
}

#[test]
fn a_persistent_import_promise_survives_a_collection_between_take_and_finish() {
    big(|| {
        let (mut it, _) = setup();
        out(&mut it, "var log = []; var w = new WeakRef({ tag: 'junk' }); import('ok').then(v => log.push('ok:' + v.v), e => log.push('bad:' + e)); import('no').catch(e => log.push(e.name)); 0");
        let reqs = enter(&mut it, |s| s.take_dynamic_imports());
        assert_eq!(reqs.len(), 2);
        let slots = it.persistent_slots();
        // Only the handles reach the promises now: the engine gave up its root, and no script variable holds them.
        it.collect(&[]).unwrap();
        it.collect(&[]).unwrap();
        assert_eq!(out(&mut it, "w.deref() === undefined"), "true", "a collection ran, and it freed what nothing held");
        enter(&mut it, |s| {
            let files: Files = &[("ok", "export const v = 5")];
            let m = load(s, files, "ok", &mut HashMap::new()).unwrap();
            s.finish_dynamic_import(&reqs[0].promise, m).unwrap();
            s.fail_dynamic_import(&reqs[1].promise, "Failed to fetch no").unwrap();
            s.drain_microtasks().unwrap();
        });
        assert_eq!(out(&mut it, "log.join()"), "TypeError,ok:5");
        drop(reqs);
        assert_eq!(it.persistent_slots(), slots, "the slots are free again and the table did not grow");
        let mut live = 0;
        it.api.slots.for_each(|_| live += 1);
        assert_eq!(live, 0, "no handle is left");
    });
}
