//! The old engine behind `bare-js-api` (plan step S0.3): host functions, handles, rooting and errors through the interface.

use super::*;
use crate::heap::Heap;
use bare_js_api::{Args, Attrs, Engine, Local, Persistent, Scope, Type, enter};
use std::cell::RefCell;

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
