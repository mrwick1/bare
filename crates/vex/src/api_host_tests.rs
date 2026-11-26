//! Tests of owners, host-filled objects and lazy members (`api_host.rs`).
use super::*;
use crate::script;
use bare_js_api::{Abort, Args, Attrs, Engine, Error, Host, HostFn, Local, Persistent, Scope, Tracer, Traced, enter};
use crate::interp_rt::Key;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

fn realm() -> Realm {
    script::load("").unwrap().realm
}

/// A host that keeps `Traced` values for owners and records what the collector asked.
#[derive(Default)]
struct Owners {
    held: RefCell<HashMap<u32, Vec<Traced<Value>>>>,
    traced: RefCell<Vec<u32>>,
    pruned: RefCell<Vec<u32>>,
}

impl Host<Realm> for Owners {
    fn call<'s>(&self, s: &mut Scope<'s, Realm>, _id: u32, _this: Local<'s>, _args: Args<'s>, _new: bool) -> Result<Local<'s>, Error<'s>> {
        Ok(s.undefined())
    }

    fn trace(&self, key: u32, t: &mut Tracer<'_, Value>) {
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
}

fn setup() -> (Realm, Rc<Owners>) {
    let mut r = realm();
    let h = Rc::new(Owners::default());
    r.set_host(h.clone());
    (r, h)
}

/// A new owner object with key `key` that holds `held` through the host. Gives a handle to keep it alive, when `keep`.
fn make_owner(r: &mut Realm, h: &Owners, key: u32, held: &[Value], keep: bool) -> Option<Persistent> {
    enter(r, |s| {
        let o = s.new_object().unwrap();
        s.set_host_owner(o, key).unwrap();
        let mut v = Vec::new();
        for &x in held {
            let l = s.engine().raw_local(&x);
            v.push(s.trace_ref(l));
        }
        h.held.borrow_mut().insert(key, v);
        keep.then(|| s.persist(o))
    })
}

fn marked(r: &mut Realm) -> u64 {
    r.collect(&mut [], |_| {});
    r.last_gc.marked
}

fn object(r: &mut Realm) -> Value {
    enter(r, |s| {
        let o = s.new_object().unwrap();
        let n = s.number(42.0);
        s.set_str(o, "x", n).unwrap();
        s.engine().raw(o)
    })
}

#[test]
fn a_detached_cycle_through_an_owner_is_freed_and_pruned_once() {
    let (mut r, h) = setup();
    let base = marked(&mut r);
    // owner O (key 1) holds V through the host, and V holds O: a cycle that only the owner trace closes.
    let v = object(&mut r);
    let p = make_owner(&mut r, &h, 1, &[v], true).unwrap();
    let o = enter(&mut r, |s| {
        let o = s.local(&p);
        s.engine().raw(o)
    });
    enter(&mut r, |s| {
        let (vl, ol) = (s.engine().raw_local(&v), s.engine().raw_local(&o));
        s.set_str(vl, "back", ol).unwrap();
    });
    let live = marked(&mut r);
    assert!(live >= base + 2, "O and V are alive while the handle holds O: {live} vs {base}");
    assert!(h.traced.borrow().contains(&1));
    assert!(h.pruned.borrow().is_empty());
    drop(p);
    assert_eq!(marked(&mut r), base, "the cycle is freed");
    assert_eq!(*h.pruned.borrow(), [1], "prune ran once, with the key");
    assert_eq!(marked(&mut r), base);
    assert_eq!(*h.pruned.borrow(), [1], "a dead owner is pruned once");
    assert!(r.api.hs.owners.is_empty());
}

#[test]
fn a_live_owner_keeps_the_values_that_it_traces() {
    let (mut r, h) = setup();
    let base = marked(&mut r);
    let v = object(&mut r);
    let p = make_owner(&mut r, &h, 7, &[v], true).unwrap();
    for _ in 0..3 {
        assert!(marked(&mut r) >= base + 2);
    }
    let x = enter(&mut r, |s| {
        let t = h.held.borrow();
        let l = s.traced(&t[&7][0]);
        let x = s.get_str(l, "x").unwrap();
        s.as_number(x)
    });
    assert_eq!(x, Some(42.0), "the traced value still has its property");
    drop(p);
}

#[test]
fn an_owner_found_while_tracing_an_owner_is_traced_in_the_same_collection() {
    let (mut r, h) = setup();
    let base = marked(&mut r);
    let (v, pv) = {
        let v = object(&mut r);
        (v, enter(&mut r, |s| {
            let l = s.engine().raw_local(&v);
            s.persist(l)
        }))
    };
    // C holds V; B holds C; A (kept) holds B. C and B are reached only through other owners.
    let cv = enter(&mut r, |s| {
        let o = s.new_object().unwrap();
        s.set_host_owner(o, 3).unwrap();
        s.engine().raw(o)
    });
    let bv = enter(&mut r, |s| {
        let o = s.new_object().unwrap();
        s.set_host_owner(o, 2).unwrap();
        s.engine().raw(o)
    });
    let keep = enter(&mut r, |s| {
        let (c, b) = (s.engine().raw_local(&cv), s.engine().raw_local(&bv));
        (s.persist(c), s.persist(b))
    });
    h.held.borrow_mut().insert(3, vec![Traced::from_raw(v)]);
    h.held.borrow_mut().insert(2, vec![Traced::from_raw(cv)]);
    let a = make_owner(&mut r, &h, 1, &[bv], true).unwrap();
    drop(keep);
    h.traced.borrow_mut().clear();
    drop(pv);
    let live = marked(&mut r);
    let mut t = h.traced.borrow().clone();
    t.sort();
    assert_eq!(t, [1, 2, 3], "each owner of the chain was traced, once");
    assert_eq!(r.last_gc.owners, 3);
    assert!(live >= base + 4);
    assert!(h.pruned.borrow().is_empty());
    drop(a);
    assert_eq!(marked(&mut r), base);
    let mut d = h.pruned.borrow().clone();
    d.sort();
    assert_eq!(d, [1, 2, 3], "each dead owner was pruned once");
}

#[test]
fn a_new_key_for_the_same_object_replaces_the_old_one() {
    let (mut r, h) = setup();
    let p = make_owner(&mut r, &h, 5, &[], true).unwrap();
    enter(&mut r, |s| {
        let o = s.local(&p);
        s.set_host_owner(o, 6).unwrap();
    });
    h.traced.borrow_mut().clear();
    marked(&mut r);
    assert_eq!(*h.traced.borrow(), [6]);
    drop(p);
    marked(&mut r);
    assert_eq!(*h.pruned.borrow(), [6]);
}

#[test]
fn only_an_object_can_own() {
    let (mut r, _h) = setup();
    let e = enter(&mut r, |s| {
        let n = s.number(1.0);
        matches!(s.set_host_owner(n, 1), Err(Error::Throw(_)))
    });
    assert!(e);
}

#[test]
fn an_owner_with_a_host_function_survives_forced_collections() {
    let (mut r, h) = setup();
    let v = object(&mut r);
    let p = make_owner(&mut r, &h, 9, &[v], true).unwrap();
    let pf = enter(&mut r, |s| {
        let f = s.host_function("probe", 0, 0, HostFn::Method).unwrap();
        s.persist(f)
    });
    let g = r.global.value();
    for _ in 0..5 {
        let f = enter(&mut r, |s| {
            let l = s.local(&pf);
            s.engine().raw(l)
        });
        r.call_held(f, g, &[]).unwrap();
        r.collect(&mut [], |_| {});
    }
    assert!(h.pruned.borrow().is_empty());
    drop(p);
    let _ = Attrs::ALL;
}

// ---- host-filled objects and lazy members

use bare_js_api::{Item, Lazy, Named, Pending};
use std::cell::Cell;

/// Indexed objects (key 10) with the items `items`, named objects (key 20) with `a` and `b`, lazy members.
#[derive(Default)]
struct Filled {
    items: Vec<Value>,
    busy: Cell<bool>,
    asked: RefCell<Vec<String>>,
}

fn units(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}

impl Host<Realm> for Filled {
    fn call<'s>(&self, s: &mut Scope<'s, Realm>, id: u32, this: Local<'s>, args: Args<'s>, _new: bool) -> Result<Local<'s>, Error<'s>> {
        assert_eq!(id, 0);
        s.set_str(this, "written", args.get(0).unwrap())?;
        Ok(s.undefined())
    }

    fn indexed_len(&self, _o: &Value, key: u32) -> Result<usize, Pending> {
        if self.busy.get() {
            return Err(Pending::Busy);
        }
        Ok(if key == 10 { self.items.len() } else { 0 })
    }

    fn indexed_item(&self, _o: &Value, _key: u32, i: usize) -> Result<Item<Value>, Pending> {
        if self.busy.get() {
            return Err(Pending::Busy);
        }
        Ok(self.items.get(i).map_or(Item::Absent, |&v| Item::Value(v)))
    }

    fn indexed_name(&self, _o: &Value, _key: u32, name: &[u16]) -> Result<Option<u32>, Pending> {
        Ok((name == units("first")).then_some(0))
    }

    fn indexed_names(&self, _o: &Value, _key: u32) -> Result<Vec<Vec<u16>>, Pending> {
        Ok(vec![units("first")])
    }

    fn named_has(&self, _o: &Value, _key: u32, name: &[u16]) -> Result<bool, Pending> {
        Ok(name == units("a") || name == units("b") || name == units("0"))
    }

    fn named_get(&self, _o: &Value, _key: u32, name: &[u16]) -> Result<Option<Named<Value>>, Pending> {
        Ok(Some(Named::Text(if name == units("a") { units("alpha") } else { units("beta") })))
    }

    fn named_len(&self, _o: &Value, _key: u32) -> Result<usize, Pending> {
        Ok(2)
    }

    fn named_keys(&self, _o: &Value, _key: u32) -> Result<Vec<Vec<u16>>, Pending> {
        Ok(vec![units("a"), units("b")])
    }

    fn lazy_member<'s>(&self, s: &mut Scope<'s, Realm>, object: Local<'s>, what: Lazy<'_>) -> Result<(), Error<'s>> {
        self.asked.borrow_mut().push(format!("{what:?}"));
        match what {
            Lazy::Name("lazySetter") => {
                let set = s.host_function("set lazySetter", 0, 1, HostFn::Method)?;
                s.define_accessor_str(object, "lazySetter", None, Some(set), Attrs::METHOD)?;
            }
            Lazy::Name("lazyProp") => {
                let v = s.number(7.0);
                s.define_str(object, "lazyProp", v, Attrs::ALL)?;
            }
            Lazy::All => {
                let v = s.number(9.0);
                s.define_str(object, "allProp", v, Attrs::ALL)?;
            }
            _ => {}
        }
        Ok(())
    }
}

/// A realm whose global has `nl` (indexed, items "i0".."i2"), `ds` (named), `op` (open named), `lz` (lazy) and `lzc` (inherits lz).
fn filled() -> (Realm, Rc<Filled>) {
    let mut r = realm();
    let mut f = Filled::default();
    for i in 0..3 {
        let v = r.string(&format!("i{i}"));
        r.keep(v);
        f.items.push(v);
    }
    let h = Rc::new(f);
    r.set_host(h.clone());
    let g = r.global;
    enter(&mut r, |s| {
        let proto = s.new_object().unwrap();
        let pv = s.number(5.0);
        s.set_str(proto, "foo", pv).unwrap();
        let nl = s.new_host_indexed(Some(proto), 10).unwrap();
        let ds = s.new_host_named(Some(proto), 20, false).unwrap();
        let op = s.new_host_named(Some(proto), 21, true).unwrap();
        let lz = s.new_object().unwrap();
        s.set_lazy(lz, false).unwrap();
        let lzc = s.new_object().unwrap();
        s.set_prototype_of(lzc, Some(lz)).unwrap();
        let gl = s.global();
        for (k, v) in [("nl", nl), ("ds", ds), ("op", op), ("lz", lz), ("lzc", lzc)] {
            s.set_str(gl, k, v).unwrap();
        }
        let pk = s.host_key(nl);
        assert_eq!(pk, Some(10));
        assert_eq!(s.host_key(ds), Some(20));
        assert_eq!(s.host_key(proto), None);
    });
    let _ = g;
    (r, h)
}

fn js(r: &mut Realm, src: &str) -> String {
    r.run_text(&format!("var __r = ({src});")).expect("the script runs");
    r.collect(&mut [], |_| {});
    let g = r.global.value();
    let k = r.intern("__r");
    let v = r.get_prop(g, Key::Name(k)).unwrap();
    let s = r.to_str(v).unwrap();
    r.text(s)
}

#[test]
fn an_indexed_object_reads_its_items_from_the_host() {
    let (mut r, _h) = filled();
    assert_eq!(js(&mut r, "nl[0] + nl[2]"), "i0i2");
    assert_eq!(js(&mut r, "String(nl[3])"), "undefined");
    assert_eq!(js(&mut r, "nl.foo"), "5", "an absent index goes on to the prototype");
    assert_eq!(js(&mut r, "(1 in nl) + ',' + (3 in nl)"), "true,false");
    assert_eq!(js(&mut r, "Object.keys(nl).join()"), "0,1,2");
    assert_eq!(js(&mut r, "nl.hasOwnProperty(1) + ',' + nl.hasOwnProperty(5)"), "true,false");
    assert_eq!(js(&mut r, "nl.first"), "i0", "an alias");
    assert_eq!(js(&mut r, "Object.keys(nl).indexOf('first')"), "-1", "an alias is not enumerable");
    assert_eq!(js(&mut r, "Object.getOwnPropertyNames(nl).join()"), "0,1,2,first");
    let d = js(&mut r, "(function (d) { return d.value + d.writable + d.enumerable + d.configurable })(Object.getOwnPropertyDescriptor(nl, 1))");
    assert_eq!(d, "i1falsetruetrue");
}

#[test]
fn a_write_define_or_delete_of_an_index_is_refused() {
    let (mut r, _h) = filled();
    assert_eq!(js(&mut r, "(nl[0] = 'x', nl[0])"), "i0", "a sloppy write does nothing");
    assert_eq!(js(&mut r, "(nl[7] = 'x', String(nl[7]))"), "undefined");
    assert_eq!(js(&mut r, "delete nl[0]"), "false");
    assert_eq!(js(&mut r, "(function () { 'use strict'; try { nl[0] = 1; return 'no' } catch (e) { return e.name } })()"), "TypeError");
    assert_eq!(js(&mut r, "(nl.extra = 3, nl.extra)"), "3", "other keys are ordinary");
}

#[test]
fn a_named_object_answers_its_names_first() {
    let (mut r, _h) = filled();
    assert_eq!(js(&mut r, "ds.a + ds.b"), "alphabeta");
    assert_eq!(js(&mut r, "('a' in ds) + ',' + ('zz' in ds)"), "true,false");
    assert_eq!(js(&mut r, "Object.keys(ds).join()"), "a,b");
    assert_eq!(js(&mut r, "String(ds.zz) + ds.foo"), "undefined5");
    assert_eq!(js(&mut r, "(ds.a = 'q', ds.a)"), "alpha", "a name that the host has cannot be written");
    assert_eq!(js(&mut r, "(ds.other = 'q', String(ds.other))"), "undefined", "a host with no named_write refuses every name; the write goes to the host, not to an ordinary property");
    assert_eq!(js(&mut r, "ds[0]"), "beta", "an index is the name of the same text");
    assert_eq!(js(&mut r, "delete ds.a"), "false");
    assert_eq!(js(&mut r, "ds.hasOwnProperty('b') + ',' + ds.hasOwnProperty('zz')"), "true,false");
}

#[test]
fn an_open_named_object_gives_way_to_an_ordinary_property() {
    let (mut r, _h) = filled();
    assert_eq!(js(&mut r, "op.a"), "alpha");
    assert_eq!(js(&mut r, "op.foo"), "5", "the chain wins over a name only when the host has no name");
    // `b` is a host name and an ordinary property of the prototype: the open object gives way, the closed one does not.
    assert_eq!(js(&mut r, "(Object.getPrototypeOf(op).b = 'pb', op.b)"), "pb");
    assert_eq!(js(&mut r, "(Object.getPrototypeOf(ds).b = 'pb', ds.b)"), "beta");
}

#[test]
fn a_busy_host_stops_the_script() {
    let (mut r, h) = filled();
    h.busy.set(true);
    let e = r.run_text("var x = nl[0];");
    assert!(matches!(e, Err(crate::api::RunError::Abort(Abort::Limit("the host is busy")))), "{e:?}");
    h.busy.set(false);
    assert!(r.run_text("var x = nl[0];").is_ok());
}

#[test]
fn a_lazy_member_is_made_at_the_first_read_and_asked_once() {
    let (mut r, h) = filled();
    assert_eq!(js(&mut r, "lz.lazyProp"), "7");
    assert_eq!(js(&mut r, "lz.lazyProp + lz.lazyProp"), "14");
    assert_eq!(h.asked.borrow().iter().filter(|a| a.contains("lazyProp")).count(), 1, "{:?}", h.asked);
    assert_eq!(js(&mut r, "String(lz.nothing) + String(lz.nothing)"), "undefinedundefined");
    assert_eq!(h.asked.borrow().iter().filter(|a| a.contains("nothing")).count(), 1);
    assert_eq!(js(&mut r, "Object.keys(lz).sort().join()"), "allProp,lazyProp");
    assert_eq!(h.asked.borrow().iter().filter(|a| *a == "All").count(), 1);
    assert_eq!(js(&mut r, "'lazyProp' in lzc"), "true", "the member that the host defined is an ordinary property of lz now");
}

#[test]
fn clear_lazy_stops_the_asks_and_chain_marks_the_prototypes() {
    let (mut r, h) = filled();
    enter(&mut r, |s| {
        let g = s.global();
        let lz = s.get_str(g, "lz").unwrap();
        s.clear_lazy(lz);
    });
    assert_eq!(js(&mut r, "String(lz.lazyProp)"), "undefined");
    assert!(h.asked.borrow().is_empty());
    enter(&mut r, |s| {
        let g = s.global();
        let lzc = s.get_str(g, "lzc").unwrap();
        s.set_lazy(lzc, true).unwrap();
    });
    assert_eq!(js(&mut r, "lzc.lazyProp"), "7", "the prototype was marked");
    assert!(!h.asked.borrow().is_empty());
}

#[test]
fn host_objects_survive_forced_collections_between_reads() {
    let (mut r, _h) = filled();
    for _ in 0..4 {
        assert_eq!(js(&mut r, "nl[1] + ds.a + op.a"), "i1alphaalpha");
        r.collect(&mut [], |_| {});
    }
}

#[test]
fn a_date_is_not_a_host_object_and_a_host_object_is_not_a_date() {
    let (mut r, _h) = filled();
    assert_eq!(js(&mut r, "(function () { var d = new Date(5); d.x = 3; return d.x + ',' + d.getTime() + ',' + Object.keys(d).join() + ',' + d.hasOwnProperty('x'); })()"), "3,5,x,true");
    assert_eq!(js(&mut r, "[nl, ds, new Date(0)].map(function (o) { return Object.prototype.toString.call(o); }).join()"), "[object Object],[object Object],[object Date]");
    assert_eq!(js(&mut r, "(function () { try { Date.prototype.getTime.call(nl); return 'no'; } catch (e) { return e instanceof TypeError; } })()"), "true");
    assert_eq!(js(&mut r, "(function () { try { Date.prototype.getTime.call(ds); return 'no'; } catch (e) { return e instanceof TypeError; } })()"), "true");
}

#[test]
fn a_host_object_is_not_a_date_for_object_prototype_to_string() {
    let (mut r, _h) = filled();
    assert_eq!(js(&mut r, "[ds, nl, new Date(0)].map(x => Object.prototype.toString.call(x)).join()"), "[object Object],[object Object],[object Date]");
    assert_eq!(js(&mut r, "[String(ds), String(op)].join()"), "[object Object],[object Object]");
}

#[test]
fn a_lazy_dictionary_global_caches_its_own_data() {
    let (mut r, _) = filled();
    enter(&mut r, |s| {
        let g = s.global();
        s.set_lazy(g, false).unwrap();
    });
    let source = "var sum = 0; for (var i = 0; i < 1000; i++) sum += i;";
    let ci = r.script_code(source, Default::default()).unwrap();
    r.run_text("implicit = 0; Object.defineProperty(globalThis, 'implicit', { enumerable: false });").unwrap();
    assert!(r.shapes.get(r.global.shape(&r.shapes)).is_dict());
    r.run_code(ci, false).unwrap();
    let tag = r.shapes.watch_tag();
    let caches = &r.pool.last().unwrap().fbs[ci].as_ref().unwrap().caches;
    assert_eq!(caches.iter().filter(|c| c.shape == tag).count(), 6);
    assert_eq!(js(&mut r, "sum + ',' + i"), "499500,1000");
}

#[test]
fn cached_globals_follow_host_names_lazy_members_and_descriptors() {
    let (mut r, h) = filled();
    enter(&mut r, |s| {
        let g = s.global();
        let p = s.get_str(g, "op").unwrap();
        s.set_prototype_of(g, Some(p)).unwrap();
        s.set_lazy(g, false).unwrap();
    });
    r.run_text(r#"
        implicit = 0;
        Object.defineProperty(globalThis, 'implicit', { enumerable: false });
        function read() { return a; }
        function write(v) { 'use strict'; a = v; }
        function lazy() { return lazyProp; }
        var log = read() + ',';
        a = 1;
        write(2); write(3); write(4);
        read(); read(); log += read() + ',';
        Object.defineProperty(globalThis, 'a', { writable: false });
        try { write(5); } catch (e) { log += e.name + ','; }
        log += read() + ',';
        Object.defineProperty(globalThis, 'a', { get: function () { return 6; }, configurable: true });
        log += read() + ',';
        delete a;
        log += read() + ',' + lazy() + lazy() + lazy();
    "#).unwrap();
    assert_eq!(h.asked.borrow().iter().filter(|a| a.contains("lazyProp")).count(), 1);
    assert_eq!(js(&mut r, "log"), "alpha,4,TypeError,4,6,alpha,777");
}

#[test]
fn borrows_the_current_call_arguments() {
    struct ReadArgs;

    impl Host<Realm> for ReadArgs {
        fn call<'s>(&self, s: &mut Scope<'s, Realm>, _id: u32, this: Local<'s>, args: Args<'s>, _new: bool) -> Result<Local<'s>, Error<'s>> {
            let rt = s.engine();
            assert_eq!(rt.raw_args_slice(args, 1, usize::MAX), Some([Value::int(2), Value::int(3)].as_slice()));
            assert_eq!(rt.raw_args_slice(args, 1, 1), None);
            assert_eq!(rt.raw_args_slice(args, 3, 4), None);
            assert_eq!(rt.raw_args_slice(Args::from_slots(this.slot(), 1), 0, 1), None);
            assert_eq!(rt.raw_args_slice(Args::from_slots(args.get(2).unwrap().slot(), 2), 0, 2), None);
            Ok(s.undefined())
        }
    }

    let mut r = realm();
    r.set_host(Rc::new(ReadArgs));
    enter(&mut r, |s| {
        let f = s.host_function("readArgs", 0, 3, HostFn::Method).unwrap();
        let g = s.global();
        s.define_str(g, "readArgs", f, Attrs::METHOD).unwrap();
    });
    r.run_text("readArgs(1, 2, 3)").unwrap();
}

#[test]
fn a_counted_collection_skips_live_cells_and_free_slots() {
    let mut r = realm();
    r.raw_collect(&[]);
    let (keep, dead) = enter(&mut r, |s| {
        let keep = s.new_object().unwrap();
        let dead = s.new_object().unwrap();
        (s.engine().raw(keep), s.engine().raw(dead))
    });
    let text = r.string("a small string");
    let long = r.string(&"x".repeat(16_384));
    let symbol = Str::new_symbol(&mut r.heap, None).unwrap().key_value();

    assert_eq!(r.raw_collect(&[keep, dead, text, long, symbol]), Default::default());
    let freed = r.raw_collect(&[keep]);
    assert_eq!((freed.objects, freed.strings, freed.symbols), (1, 2, 1));
    assert_eq!(r.raw_collect(&[keep]), Default::default());
}

#[test]
fn assignment_materializes_an_inherited_host_setter() {
    for assignment in ["lzc.lazySetter = 42", "(function (key) { lzc[key] = 42; })('lazySetter')"] {
        let (mut r, _) = filled();
        let src = format!("({assignment}, [lzc.written, Object.hasOwn(lzc, 'lazySetter'), Object.hasOwn(lz, 'written')].join())");
        assert_eq!(js(&mut r, &src), "42,false,false", "{assignment}");
    }
    let (mut r, _) = filled();
    assert_eq!(js(&mut r, "(lz.lazySetter = 42, lz.written)"), "42");
}
