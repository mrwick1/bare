//! Tests of host named properties (`Heap::new_named`, `NamedSource`, `HeapError::NeedsNamed`, `Host::named_write`). The host here
//! is an ordered list of (name, value) strings. It is the contract of `DOMStringMap`, without a DOM. A name that starts with
//! `bad` is refused by the setter with a thrown error, and a name that starts with `no` is refused with `false`. Each test runs on
//! a big stack.

use super::*;
use crate::heap::{Heap, HeapError, NamedOp, NamedRequest, NamedSource};
use std::cell::{Cell, RefCell};

fn big<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(64 << 20).spawn(f).unwrap().join().unwrap()
}

#[derive(Default)]
struct X {
    owner: Cell<Option<ObjId>>,
    pairs: RefCell<Vec<(String, String)>>,
    /// Calls of `named_write` (a repeat that does not call the host again must not count).
    writes: Cell<usize>,
    /// `named_masking` is `false` (the `Storage` rules: the prototype chain wins).
    open: Cell<bool>,
}

fn text(units: &[u16]) -> String {
    String::from_utf16_lossy(units)
}

impl NamedSource for X {
    fn named_has(&self, owner: ObjId, key: &[u16]) -> Result<bool, HeapError> {
        let k = text(key);
        Ok(self.owner.get() == Some(owner) && self.pairs.try_borrow().map_err(|_| HeapError::HostUnavailable)?.iter().any(|(n, _)| *n == k))
    }
    fn named_get(&self, owner: ObjId, key: &[u16]) -> Result<Option<Box<[u16]>>, HeapError> {
        let k = text(key);
        if self.owner.get() != Some(owner) {
            return Ok(None);
        }
        Ok(self.pairs.try_borrow().map_err(|_| HeapError::HostUnavailable)?.iter().find(|(n, _)| *n == k).map(|(_, v)| v.encode_utf16().collect()))
    }
    fn named_len(&self, owner: ObjId) -> Result<usize, HeapError> {
        Ok(if self.owner.get() == Some(owner) { self.pairs.try_borrow().map_err(|_| HeapError::HostUnavailable)?.len() } else { 0 })
    }
    fn named_keys(&self, owner: ObjId) -> Result<Vec<Box<[u16]>>, HeapError> {
        if self.owner.get() != Some(owner) {
            return Ok(Vec::new());
        }
        Ok(self.pairs.try_borrow().map_err(|_| HeapError::HostUnavailable)?.iter().map(|(n, _)| n.encode_utf16().collect()).collect())
    }
    fn named_masking(&self, _: ObjId) -> bool {
        !self.open.get()
    }
}

impl Host for X {
    fn call(&self, it: &mut Interp<'_>, id: u32, _this: Value, args: &[Value], _new: bool) -> Result<Value, Error> {
        let n = |i: usize| args.get(i).copied().unwrap_or(Value::Undefined);
        match id {
            // make(proto?): the named object
            0 => {
                let proto = match n(0) { Value::Object(o) => o, _ => it.object_prototype() };
                let o = it.heap.new_named(Some(proto)).unwrap();
                self.owner.set(Some(o));
                Ok(Value::Object(o))
            }
            // put(name, value): a change that does not come through the object
            1 => {
                let (k, v) = (it.to_js_string(n(0))?, it.to_js_string(n(1))?);
                let (k, v) = (text(it.heap.str_units(k).unwrap()), text(it.heap.str_units(v).unwrap()));
                let mut p = self.pairs.borrow_mut();
                match p.iter_mut().find(|(a, _)| *a == k) {
                    Some(e) => e.1 = v,
                    None => p.push((k, v)),
                }
                Ok(Value::Undefined)
            }
            // writes()
            2 => Ok(Value::Number(self.writes.get() as f64)),
            // open(): the object stops hiding the prototype chain
            3 => {
                self.open.set(true);
                Ok(Value::Undefined)
            }
            _ => unreachable!(),
        }
    }
    fn roots(&self, out: &mut Vec<Value>) {
        out.extend(self.owner.get().map(Value::Object));
    }
    fn named_write(&self, it: &mut Interp<'_>, req: NamedRequest) -> Result<bool, Error> {
        self.writes.set(self.writes.get() + 1);
        if self.owner.get() != Some(req.owner) {
            return Ok(false);
        }
        let name = match it.heap.key_to_value(req.key) {
            Ok(Value::String(s)) => text(it.heap.str_units(s).unwrap()),
            _ => panic!("a symbol reached the host"),
        };
        if name.starts_with("bad") {
            return Err(it.type_error(&format!("bad name {name}")));
        }
        if name.starts_with("no") {
            return Ok(false);
        }
        match req.op {
            NamedOp::Set(v) | NamedOp::Define(crate::heap::PropertyDescriptor { value: Some(v), .. }) => {
                // ToString runs here, with the evaluator: it can call user code.
                let s = it.to_js_string(v)?;
                let s = text(it.heap.str_units(s).unwrap());
                let mut p = self.pairs.borrow_mut();
                match p.iter_mut().find(|(a, _)| *a == name) {
                    Some(e) => e.1 = s,
                    None => p.push((name, s)),
                }
                Ok(true)
            }
            // An accessor, or a descriptor without a value: refused.
            NamedOp::Define(_) => Ok(false),
            NamedOp::Delete => {
                self.pairs.borrow_mut().retain(|(a, _)| *a != name);
                Ok(true)
            }
        }
    }
}

fn setup() -> (Interp<'static>, Rc<X>) {
    let mut it = Interp::realm(Heap::default(), EvalLimits { max_steps: 1_000_000, ..Default::default() }).unwrap();
    let x = Rc::new(X::default());
    it.set_host(x.clone()).unwrap();
    it.heap.set_named_source(x.clone());
    let g = it.global_object();
    for (i, name) in ["make", "put", "writes", "open"].iter().enumerate() {
        let f = it.host_function(i as u32, name, 0, false).unwrap();
        let k = it.heap.key_from_str(name).unwrap();
        it.define_data(g, k, Value::Object(f), true, false, true).unwrap();
    }
    (it, x)
}

fn out(it: &mut Interp, src: &str) -> String {
    match it.run_script(src) {
        Ok(v) => it.display(v),
        Err(Error::Throw(v)) if matches!(v, Value::Object(_)) => format!("throw:{}:{}", it.get_str(v, "name"), it.get_str(v, "message")),
        Err(Error::Limit(l)) => format!("limit:{l:?}"),
        Err(Error::Unsupported(u)) => format!("unsupported:{}", u.what),
        Err(e) => format!("other:{e:?}"),
    }
}

#[test]
fn reads_are_live_and_cover_every_read_path() {
    big(|| {
        let (mut it, _) = setup();
        assert_eq!(out(&mut it, "var m = make(); put('a', '1'); put('b', '2'); [m.a, m.b, m.c, 'a' in m, 'c' in m, m.hasOwnProperty('a'), Object.keys(m).join(), Reflect.ownKeys(m).join()].join('|')"), "1|2||true|false|true|a,b|a,b");
        // Nothing is cached: a change made by the host is seen by the next read.
        assert_eq!(out(&mut it, "put('a', 'X'); put('z', 'Z'); [m.a, m.z, Object.keys(m).join(), JSON.stringify(m)].join('|')"), "X|Z|a,b,z|{\"a\":\"X\",\"b\":\"2\",\"z\":\"Z\"}");
        assert_eq!(out(&mut it, "var k = []; for (var n in m) k.push(n); k.join() + '|' + Object.values(m).join() + '|' + Object.entries(m).join(';')"), "a,b,z|X,2,Z|a,X;b,2;z,Z");
        assert_eq!(out(&mut it, "var s = { ...m }; s.a + s.z + Object.assign({}, m).b"), "XZ2");
        assert_eq!(out(&mut it, "JSON.stringify(Object.getOwnPropertyDescriptor(m, 'b'))"), "{\"value\":\"2\",\"writable\":true,\"enumerable\":true,\"configurable\":true}");
        assert_eq!(out(&mut it, "String(Object.getOwnPropertyDescriptor(m, 'q'))"), "undefined");
    });
}

#[test]
fn an_open_object_lets_the_prototype_chain_win_and_refuses_integrity_levels() {
    big(|| {
        let (mut it, x) = setup();
        assert_eq!(out(&mut it, "open(); var p = { m: function () { return 'method'; } }; var m = make(); Object.setPrototypeOf(m, p); put('m', 'item'); put('z', 'zed'); [typeof m.m, m.z, 'm' in m, m.hasOwnProperty('m'), String(Object.getOwnPropertyDescriptor(m, 'm')), Object.keys(m).join(), Reflect.ownKeys(m).join()].join('|')"), "function|zed|true|false|undefined|z|m,z");
        // A write, a define and a delete of a hidden name are ordinary: they make and remove an own property, and the host sees nothing.
        let w0 = x.writes.get();
        assert_eq!(out(&mut it, "m.m = 5; var a = [m.m, m.hasOwnProperty('m'), Reflect.ownKeys(m).join()]; delete m.m; a.push(typeof m.m, delete m.m); Object.defineProperty(m, 'm', { value: 6 }); a.push(m.m, Reflect.ownKeys(m).join()); a.join('|')"), "5|true|m,z|function|true|6|m,z");
        assert_eq!(x.writes.get(), w0, "no request for a hidden name");
        // A name that no chain property hides is still a request.
        assert_eq!(out(&mut it, "m.q = 1; m.q + Object.keys(m).join()"), "1z,q");
        // Index names never hide; a name added to the chain later hides at once.
        assert_eq!(out(&mut it, "p[9] = 'p9'; m[9] = 'own9'; var b = [m[9], Object.keys(m).join()]; p.q = 'late'; b.push(m.q, m.hasOwnProperty('q'), Reflect.ownKeys(m).join()); b.join('|')"), "own9|z,q,9|late|false|m,z,q,9");
        // The Standard's `[[PreventExtensions]]` of a legacy platform object is `false`, even with no name. `seal` and `freeze` follow.
        assert_eq!(out(&mut it, "[Reflect.preventExtensions(m), Object.isExtensible(m), Object.isFrozen(m), Object.isSealed(m)].join()"), "false,true,false,false");
        assert_eq!(out(&mut it, "var e = []; for (var f of [Object.preventExtensions, Object.seal, Object.freeze]) { try { f(m); e.push('no') } catch (x) { e.push(x.name + ':' + x.message) } } e.join('|')"), "TypeError:Cannot prevent extensions|TypeError:Cannot seal|TypeError:Cannot freeze");
    });
}

#[test]
fn a_name_may_hold_a_lone_surrogate() {
    big(|| {
        let (mut it, _) = setup();
        // `put` goes through `text`, which is lossy, so the host here stores U+FFFD. The heap side keeps the units it is given.
        assert_eq!(out(&mut it, "var m = make(); put('k\\ud800', 'v'); Object.keys(m)[0].length + ',' + m['k\\ufffd']"), "2,v");
    });
}

#[test]
fn an_array_index_is_a_name_and_a_symbol_is_not() {
    big(|| {
        let (mut it, _) = setup();
        assert_eq!(out(&mut it, "var m = make(); put('7', 'seven'); put('x', 'ex'); [m[7], m['7'], 7 in m, Object.keys(m).join()].join('|')"), "seven|seven|true|7,x");
        assert_eq!(out(&mut it, "m[3] = 'three'; delete m[7]; Object.keys(m).join()"), "x,3");
        // A symbol key is an ordinary property: no request, no host call.
        assert_eq!(out(&mut it, "var w0 = writes(); var sy = Symbol('s'); m[sy] = 5; Object.defineProperty(m, Symbol.iterator, { value: 1 }); delete m[sy]; [writes() - w0, sy in m, Reflect.ownKeys(m).length].join()"), "0,false,3");
    });
}

#[test]
fn writes_go_through_the_host_once_and_convert_with_to_string() {
    big(|| {
        let (mut it, x) = setup();
        assert_eq!(out(&mut it, "var m = make(); m.a = 5; m.b = { toString: function () { return 'obj'; } }; m.c = null; [m.a, m.b, m.c, typeof m.a].join()"), "5,obj,null,string");
        assert_eq!(x.writes.get(), 3, "one host call for each write, not one for the repeat");
        assert_eq!(out(&mut it, "Reflect.set(m, 'd', 1) + '|' + m.d + '|' + Object.keys(m).join()"), "true|1|a,b,c,d");
        assert_eq!(out(&mut it, "Object.assign(m, { e: 1, f: 2 }); m.e + m.f"), "12");
        // The user code of `toString` runs in the evaluator, and its exception is the result.
        assert_eq!(out(&mut it, "try { m.g = { toString: function () { throw new RangeError('r'); } }; 'no' } catch (e) { e.name }"), "RangeError");
        assert_eq!(out(&mut it, "'g' in m"), "false");
    });
}

#[test]
fn a_host_error_and_a_host_false_are_not_swallowed() {
    big(|| {
        let (mut it, _) = setup();
        assert_eq!(out(&mut it, "var m = make(); try { m.badName = 1; 'no' } catch (e) { e.name + ':' + e.message }"), "TypeError:bad name badName");
        // `false`: silent in sloppy mode, a TypeError in strict mode, `false` for Reflect.
        assert_eq!(out(&mut it, "m.noName = 1; ('noName' in m) + '|' + Reflect.set(m, 'noName', 1)"), "false|false");
        assert_eq!(out(&mut it, "(function () { 'use strict'; try { m.noName = 1; return 'no'; } catch (e) { return e.name; } })()"), "TypeError");
        assert_eq!(out(&mut it, "Reflect.defineProperty(m, 'noName', { value: 1 })"), "false");
        assert_eq!(out(&mut it, "try { Object.defineProperty(m, 'noName', { value: 1 }); 'no' } catch (e) { e.name }"), "TypeError");
    });
}

#[test]
fn define_property_ignores_the_flags_and_refuses_accessors() {
    big(|| {
        let (mut it, _) = setup();
        assert_eq!(out(&mut it, "var m = make(); Object.defineProperty(m, 'a', { value: 'v', enumerable: false, writable: false, configurable: false }); [m.a, Object.keys(m).join(), JSON.stringify(Object.getOwnPropertyDescriptor(m, 'a'))].join('|')"), "v|a|{\"value\":\"v\",\"writable\":true,\"enumerable\":true,\"configurable\":true}");
        assert_eq!(out(&mut it, "Reflect.defineProperty(m, 'b', { get: function () { return 1; } }) + '|' + ('b' in m)"), "false|false");
    });
}

#[test]
fn delete_is_true_without_change_for_an_unknown_name() {
    big(|| {
        let (mut it, x) = setup();
        assert_eq!(out(&mut it, "var m = make(); put('a', '1'); put('b', '2'); [delete m.a, 'a' in m, delete m.zz, Object.keys(m).join(), Reflect.deleteProperty(m, 'b'), Object.keys(m).length].join()"), "true,false,true,b,true,0");
        // The unknown name never reached the host.
        assert_eq!(x.writes.get(), 2);
        // A host that refuses the delete: `false` for Reflect, a TypeError in strict mode, and the name stays.
        assert_eq!(out(&mut it, "put('noX', 'v'); [Reflect.deleteProperty(m, 'noX'), 'noX' in m, (function () { 'use strict'; try { delete m.noX; return 'no'; } catch (e) { return e.name; } })()].join()"), "false,true,TypeError");
    });
}

#[test]
fn a_named_object_as_a_prototype_reads_through_and_writes_stay_ordinary() {
    big(|| {
        let (mut it, x) = setup();
        assert_eq!(out(&mut it, "var m = make(); put('a', '1'); var c = Object.create(m); [c.a, 'a' in c, c.hasOwnProperty('a'), Object.keys(c).length].join()"), "1,true,false,0");
        // Another receiver: OrdinarySet makes an own property on the child and the host sees no write.
        assert_eq!(out(&mut it, "c.a = 'mine'; c.n = 'new'; [c.a, m.a, c.n, 'n' in m, Object.keys(c).join()].join()"), "mine,1,new,false,a,n");
        assert_eq!(x.writes.get(), 0);
        // Own names hide the prototype chain (`[LegacyOverrideBuiltIns]`).
        assert_eq!(out(&mut it, "put('toString', 'shadow'); m.toString + '|' + typeof c.toString + '|' + typeof ({}).toString"), "shadow|string|function");
    });
}

#[test]
fn freezing_an_object_that_has_names_is_refused() {
    big(|| {
        let (mut it, _) = setup();
        assert_eq!(out(&mut it, "var m = make(); put('a', '1'); try { Object.freeze(m); 'frozen' } catch (e) { e.name }"), "TypeError");
        assert_eq!(out(&mut it, "m.a = '2'; m.a"), "2");
    });
}

#[test]
fn the_collector_keeps_the_object_and_the_names_are_not_traced() {
    big(|| {
        let (mut it, x) = setup();
        out(&mut it, "var m = make(); put('a', '1');");
        let before = it.heap.stats().live_exotics;
        it.collect(&[]).unwrap();
        assert_eq!(it.heap.stats().live_exotics, before);
        assert_eq!(out(&mut it, "m.a + Object.keys(m).join()"), "1a");
        out(&mut it, "m = null;");
        x.owner.set(None);
        it.collect(&[]).unwrap();
        assert_eq!(it.heap.stats().live_exotics, before - 1, "the payload of a dead named object is released");
    });
}

#[test]
fn a_borrowed_host_is_an_error_not_an_empty_answer() {
    big(|| {
        let (mut it, x) = setup();
        out(&mut it, "var m = make(); put('a', '1');");
        let w = x.pairs.borrow_mut();
        assert_eq!(out(&mut it, "m.a"), "other:Heap(HostUnavailable)");
        assert_eq!(out(&mut it, "'a' in m"), "other:Heap(HostUnavailable)");
        assert_eq!(out(&mut it, "Object.keys(m).length"), "other:Heap(HostUnavailable)");
        drop(w);
        assert_eq!(out(&mut it, "Object.keys(m).length"), "1");
    });
}

#[test]
fn heap_without_a_source_and_host_without_named_write_fail_loudly() {
    let mut h = Heap::default();
    let o = h.new_named(None).unwrap();
    let k = h.key_from_str("a").unwrap();
    assert_eq!(h.virtual_len(o), Err(HeapError::HostUnavailable));
    assert_eq!(h.own_keys(o), Err(HeapError::HostUnavailable));
    assert!(matches!(h.get_own_property(o, k), Err(HeapError::HostUnavailable)));
    assert_eq!(h.has_property(o, k), Err(HeapError::HostUnavailable));
    // A direct write has no evaluator: the heap asks for one.
    assert_eq!(h.set(o, k, Value::Number(1.0)), Err(HeapError::NeedsNamed));
    assert!(h.take_named_request().is_some());
    // A host that does not implement `named_write` stops the script with `Unsupported`.
    big(|| {
        struct Bare;
        impl NamedSource for Bare {
            fn named_has(&self, _: ObjId, _: &[u16]) -> Result<bool, HeapError> { Ok(false) }
            fn named_get(&self, _: ObjId, _: &[u16]) -> Result<Option<Box<[u16]>>, HeapError> { Ok(None) }
            fn named_len(&self, _: ObjId) -> Result<usize, HeapError> { Ok(0) }
            fn named_keys(&self, _: ObjId) -> Result<Vec<Box<[u16]>>, HeapError> { Ok(Vec::new()) }
        }
        impl Host for Bare {
            fn call(&self, it: &mut Interp<'_>, _: u32, _: Value, _: &[Value], _: bool) -> Result<Value, Error> {
                Ok(Value::Object(it.heap.new_named(Some(it.object_prototype())).unwrap()))
            }
        }
        let mut it = Interp::realm(Heap::default(), EvalLimits { max_steps: 100_000, ..Default::default() }).unwrap();
        let b = Rc::new(Bare);
        it.set_host(b.clone()).unwrap();
        it.heap.set_named_source(b);
        let f = it.host_function(0, "make", 0, false).unwrap();
        let (g, k) = (it.global_object(), it.heap.key_from_str("make").unwrap());
        it.define_data(g, k, Value::Object(f), true, false, true).unwrap();
        assert_eq!(out(&mut it, "var m = make(); m.a = 1"), "unsupported:named host object without named_write");
    });
}
