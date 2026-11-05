//! Tests of host-indexed objects (`Heap::new_indexed`, `IndexedSource`, `Host::fill_indexed`). The host here is a list of
//! `Option<ObjId>` and a length that the script changes with `grow`/`shrink`. It is the contract of the DOM `NamedNodeMap`,
//! without a DOM. Each test runs on a big stack.

use super::*;
use crate::heap::{Heap, IndexedSource, Slot};
use std::cell::{Cell, RefCell};

fn big<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(64 << 20).spawn(f).unwrap().join().unwrap()
}

#[derive(Default)]
struct X {
    /// The one live indexed object, and its elements. `None` is an element with no JS object yet.
    owner: Cell<Option<ObjId>>,
    items: RefCell<Vec<Option<ObjId>>>,
    fills: Cell<usize>,
    /// When true, `fill_indexed` makes nothing (a broken host).
    lazy: Cell<bool>,
}

impl IndexedSource for X {
    fn indexed_len(&self, owner: ObjId) -> Option<usize> {
        (self.owner.get() == Some(owner)).then(|| self.items.borrow().len())
    }
    fn indexed_item(&self, owner: ObjId, i: usize) -> Slot {
        if self.owner.get() != Some(owner) {
            return Slot::Absent;
        }
        match self.items.borrow().get(i) {
            None => Slot::Absent,
            Some(None) => Slot::Missing,
            Some(Some(o)) => Slot::Ready(*o),
        }
    }
}

impl Host for X {
    fn call(&self, it: &mut Interp<'_>, id: u32, _this: Value, args: &[Value], _new: bool) -> Result<Value, Error> {
        let n = |i: usize| args.get(i).copied().unwrap_or(Value::Undefined);
        match id {
            // make(proto?): the indexed object
            0 => {
                let proto = match n(0) { Value::Object(o) => o, _ => it.object_prototype() };
                let o = it.heap.new_indexed(Some(proto)).unwrap();
                self.owner.set(Some(o));
                Ok(Value::Object(o))
            }
            // grow(n)
            1 => {
                let k = it.to_js_number(n(0))? as usize;
                self.items.borrow_mut().extend((0..k).map(|_| None));
                Ok(Value::Undefined)
            }
            // shrink(n)
            2 => {
                let k = it.to_js_number(n(0))? as usize;
                let len = self.items.borrow().len();
                self.items.borrow_mut().truncate(len.saturating_sub(k));
                Ok(Value::Undefined)
            }
            // length()
            3 => Ok(Value::Number(self.items.borrow().len() as f64)),
            // fills()
            4 => Ok(Value::Number(self.fills.get() as f64)),
            // lazy(): fill makes nothing
            5 => {
                self.lazy.set(true);
                Ok(Value::Undefined)
            }
            _ => unreachable!(),
        }
    }
    fn roots(&self, out: &mut Vec<Value>) {
        out.extend(self.owner.get().map(Value::Object));
    }
    fn trace(&self, owner: ObjId, out: &mut Vec<Value>) {
        if self.owner.get() == Some(owner) {
            out.extend(self.items.borrow().iter().flatten().map(|&o| Value::Object(o)));
        }
    }
    fn fill_indexed(&self, it: &mut Interp<'_>, owner: ObjId, index: u32) -> Result<(), Error> {
        self.fills.set(self.fills.get() + 1);
        if self.lazy.get() || self.owner.get() != Some(owner) {
            return Ok(());
        }
        let o = it.heap.new_object(Some(it.object_prototype())).unwrap();
        let (k, v) = (it.heap.key_from_str("n").unwrap(), Value::Number(index as f64));
        it.define_data(o, k, v, true, true, true)?;
        if let Some(slot) = self.items.borrow_mut().get_mut(index as usize) {
            *slot = Some(o);
        }
        Ok(())
    }
}

fn setup() -> (Interp<'static>, Rc<X>) {
    let mut it = Interp::realm(Heap::default(), EvalLimits { max_steps: 1_000_000, ..Default::default() }).unwrap();
    let x = Rc::new(X::default());
    it.set_host(x.clone()).unwrap();
    it.heap.set_indexed_source(x.clone());
    let g = it.global_object();
    for (i, name) in ["make", "grow", "shrink", "len", "fills", "lazy"].iter().enumerate() {
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
fn reads_follow_the_host_length_every_time() {
    big(|| {
        let (mut it, _) = setup();
        assert_eq!(out(&mut it, "var m = make(); grow(3); [m[0].n, m[2].n, m[3], 0 in m, 3 in m, Object.keys(m).join(), Reflect.ownKeys(m).join()].join('|')"), "0|2||true|false|0,1,2|0,1,2");
        // The length changes with no write through the object: a later read sees the new state, nothing was cached.
        assert_eq!(out(&mut it, "grow(2); shrink(4); [4 in m, 1 in m, 0 in m, Object.keys(m).join(), m[1]].join('|')"), "false|false|true|0|");
        assert_eq!(out(&mut it, "var c = 0; for (var k in m) c++; c"), "1");
    });
}

#[test]
fn the_same_index_gives_the_same_object_and_is_made_once() {
    big(|| {
        let (mut it, x) = setup();
        assert_eq!(out(&mut it, "var m = make(); grow(2); m[0] === m[0] && m[0] !== m[1]"), "true");
        assert_eq!(x.fills.get(), 2);
        // `in`, `delete` and `hasOwnProperty` need no object: no fill for index 1 beyond the one above.
        let before = x.fills.get();
        assert_eq!(out(&mut it, "grow(1); [2 in m, m.hasOwnProperty(2), delete m[5]].join()"), "true,true,true");
        assert_eq!(x.fills.get(), before);
    });
}

#[test]
fn descriptors_and_writes_follow_an_indexed_getter_without_a_setter() {
    big(|| {
        let (mut it, _) = setup();
        out(&mut it, "var m = make(); grow(2);");
        assert_eq!(out(&mut it, "var d = Object.getOwnPropertyDescriptor(m, 0); [d.writable, d.enumerable, d.configurable, d.value === m[0], Object.getOwnPropertyDescriptor(m, 2)].join()"), "false,true,true,true,");
        assert_eq!(out(&mut it, "m[0] = 5; m[7] = 5; typeof m[0] + (7 in m)"), "objectfalse");
        assert_eq!(out(&mut it, "'use strict'; try { m[0] = 1 } catch (e) { e.name }"), "TypeError");
        assert_eq!(out(&mut it, "'use strict'; try { m[9] = 1 } catch (e) { e.name }"), "TypeError");
        assert_eq!(out(&mut it, "'use strict'; try { delete m[0] } catch (e) { e.name }"), "TypeError");
        assert_eq!(out(&mut it, "function t(f) { try { f(); return 'ok'; } catch (e) { return e.name; } } [t(function () { Object.defineProperty(m, 0, { value: 1 }); }), t(function () { Object.defineProperty(m, 4, { value: 1 }); }), t(function () { 'use strict'; delete m[0]; }), t(function () { 'use strict'; delete m[4]; })].join()"), "TypeError,TypeError,TypeError,ok");
        // A string key that is not an array index is an ordinary property.
        assert_eq!(out(&mut it, "m.x = 3; m['01'] = 4; [m.x, m['01'], Reflect.ownKeys(m).join()].join()"), "3,4,0,1,x,01");
    });
}

#[test]
fn an_absent_index_goes_on_to_the_prototype() {
    big(|| {
        let (mut it, _) = setup();
        assert_eq!(out(&mut it, "var p = [10, 11, 12, 13]; var m = make(p); grow(1); [m[0].n, m[1], m[3], 3 in m].join()"), "0,11,13,true");
        // An object that inherits from the indexed object sees it live.
        assert_eq!(out(&mut it, "var c = Object.create(m); [c[0] === m[0], 0 in c, c.hasOwnProperty(0), Object.keys(c).length].join()"), "true,true,false,0");
    });
}

#[test]
fn array_methods_and_spread_read_the_live_object() {
    big(|| {
        let (mut it, _) = setup();
        out(&mut it, "var m = make(); grow(3); m.length = 3;");
        assert_eq!(out(&mut it, "Array.prototype.map.call(m, function (a) { return a.n; }).join()"), "0,1,2");
        assert_eq!(out(&mut it, "Array.prototype.slice.call(m).length + ':' + Array.from(m).length"), "3:3");
    });
}

#[test]
fn a_host_that_makes_nothing_is_an_error_not_a_loop() {
    big(|| {
        let (mut it, x) = setup();
        out(&mut it, "var m = make(); grow(1); lazy();");
        let r = out(&mut it, "m[0]");
        assert!(r.starts_with("other:") || r.starts_with("unsupported:") || r.starts_with("throw:"), "{r}");
        assert!(x.fills.get() <= 3, "{} fills", x.fills.get());
    });
}

#[test]
fn key_lists_are_charged_before_they_are_allocated() {
    big(|| {
        let mut it = Interp::realm(Heap::new(crate::heap::HeapLimits { max_bytes: 8 << 20, ..Default::default() }).unwrap(), EvalLimits { max_steps: 1_000_000, ..Default::default() }).unwrap();
        let x = Rc::new(X::default());
        it.set_host(x.clone()).unwrap();
        it.heap.set_indexed_source(x.clone());
        let g = it.global_object();
        for (i, name) in ["make", "grow"].iter().enumerate() {
            let f = it.host_function(i as u32, name, 0, false).unwrap();
            let k = it.heap.key_from_str(name).unwrap();
            it.define_data(g, k, Value::Object(f), true, false, true).unwrap();
        }
        // 5 million indices would need about 40 MB of keys: refused by the heap budget, not allocated.
        out(&mut it, "var m = make();");
        x.items.borrow_mut().extend((0..5_000_000).map(|_| None));
        let r = out(&mut it, "Reflect.ownKeys(m).length");
        assert!(r.starts_with("limit:"), "{r}");
        let r = out(&mut it, "Object.keys(m).length");
        assert!(r.starts_with("limit:"), "{r}");
    });
}

#[test]
fn the_collector_follows_host_edges_and_drops_dead_objects() {
    big(|| {
        let (mut it, x) = setup();
        out(&mut it, "var m = make(); grow(2); m[0]; m[1];");
        let before = it.heap.stats().live_objects;
        it.collect(&[]).unwrap();
        // The elements are traced from the owner, so they survive while `m` lives.
        assert_eq!(it.heap.stats().live_objects, before);
        assert_eq!(out(&mut it, "m[0].n + m[1].n"), "1");
        x.items.borrow_mut().clear();
        it.collect(&[]).unwrap();
        assert!(it.heap.stats().live_objects < before);
    });
}
