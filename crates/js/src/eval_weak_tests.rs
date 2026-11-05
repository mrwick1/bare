//! WeakMap and WeakSet tests. The corpus (`tools/fixtures/js/weak-cases.txt`) is checked against what Node gave
//! (`weak-cases.expected`; `tools/js/weak_check.sh` re-runs Node). The tests below cover what Node cannot show:
//! gates, caps and the collector (ephemerons, cycles, chains, host edges, slot reuse).

use super::*;
use crate::heap::Heap;
use std::cell::RefCell;
use std::collections::HashMap;

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

fn realm() -> Interp<'static> {
    Interp::realm(Heap::default(), EvalLimits { max_steps: 50_000_000, ..Default::default() }).unwrap()
}

#[test]
fn corpus_matches_node() {
    big(|| {
        let want: Vec<&str> = include_str!("../../../tools/fixtures/js/weak-cases.expected").lines().collect();
        let mut cases: Vec<(&str, Vec<&str>)> = Vec::new();
        for line in include_str!("../../../tools/fixtures/js/weak-cases.txt").lines() {
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
fn symbol_keys_are_gated_not_faked() {
    big(|| {
        let mut it = realm();
        // reads and deletes of a symbol are false: nothing can be stored under it
        assert_eq!(out(&mut it, "var w = new WeakMap(), s = new WeakSet(), y = Symbol('y'); [w.has(y), w.get(y), w.delete(y), s.has(y), s.delete(Symbol.iterator)].join()"), "false,,false,false,false");
        // a registered symbol is not a valid weak key, as in the spec
        assert_eq!(out(&mut it, "try { w.set(Symbol.for('r'), 1); } catch (e) { e.name }"), "TypeError");
        assert_eq!(out(&mut it, "try { s.add(Symbol.for('r')); } catch (e) { e.name }"), "TypeError");
        // a non-registered symbol is valid in Node 22. bare refuses it and cannot be caught by script
        assert!(out(&mut it, "try { w.set(y, 1); } catch (e) { 'caught' }").starts_with("unsupported:"));
        let mut it = realm();
        assert!(out(&mut it, "try { new WeakSet([Symbol.iterator]); } catch (e) { 'caught' }").starts_with("unsupported:"));
        let mut it = realm();
        assert!(out(&mut it, "try { new WeakSet().add(Symbol()); } catch (e) { 'caught' }").starts_with("unsupported:"));
    });
}

#[test]
fn native_subclass_is_a_gate_and_reflect_free_construct_works() {
    big(|| {
        let mut it = realm();
        assert!(out(&mut it, "class W extends WeakMap {} try { new W(); } catch (e) { 'caught' }").starts_with("unsupported:"));
        let mut it = realm();
        assert!(out(&mut it, "class W extends WeakSet {} try { new W(); } catch (e) { 'caught' }").starts_with("unsupported:"));
        assert_eq!(out(&mut it, "WeakMap.prototype.constructor === WeakMap"), "true");
    });
}

#[test]
fn caps_are_uncatchable_limits() {
    big(|| {
        let l = ScLimits { max_weak_collections: 2, ..Default::default() };
        let mut it = realm();
        it.set_sc_limits(l);
        assert_eq!(out(&mut it, "new WeakMap(); new WeakSet(); try { new WeakMap(); } catch (e) { 'caught' }"), "limit:Sc(WeakCollections)");
        let l = ScLimits { max_weak_entries: 3, ..Default::default() };
        let mut it = realm();
        it.set_sc_limits(l);
        assert_eq!(out(&mut it, "var w = new WeakMap(), a = [{}, {}, {}, {}]; w.set(a[0], 1); w.set(a[1], 1); w.set(a[2], 1); w.set(a[2], 2); w.delete(a[0]); w.set(a[3], 1); 'ok'"), "ok");
        assert_eq!(out(&mut it, "try { w.set({}, 1); w.set({}, 1); } catch (e) { 'caught' }"), "limit:Sc(WeakEntries)");
        let l = ScLimits { max_total_weak_entries: 4, ..Default::default() };
        let mut it = realm();
        it.set_sc_limits(l);
        assert_eq!(out(&mut it, "var w = new WeakMap(), s = new WeakSet(); w.set({}, 1); w.set({}, 1); s.add({}); s.add({}); try { s.add({}); } catch (e) { 'caught' }"), "limit:Sc(WeakEntries)");
        // dead entries free the budget
        let mut it = realm();
        let l = ScLimits { max_total_weak_entries: 4, ..Default::default() };
        it.set_sc_limits(l);
        out(&mut it, "var w = new WeakMap(); for (var i = 0; i < 4; i++) w.set({}, i); 0");
        it.collect(&[]).unwrap();
        assert_eq!(it.weak_tracked(), (1, 0));
        assert_eq!(out(&mut it, "for (var i = 0; i < 4; i++) w.set({}, i); 'ok'"), "ok");
    });
}

#[test]
fn unreachable_cycles_die_and_reachable_keys_keep_values() {
    big(|| {
        let mut it = realm();
        out(&mut it, "var keep = {}; var wm = new WeakMap(); wm.set(keep, { tag: 'kept' });
            (function () { var k = {}; var v = { back: k, wm: wm }; k.v = v; wm.set(k, v); var k2 = {}; wm.set(k2, k2); var m2 = new WeakMap(); var k3 = {}; m2.set(k3, { m2: m2, k3: k3 }); })(); 0");
        let (c0, e0) = it.weak_tracked();
        assert_eq!((c0, e0), (2, 4));
        let live0 = it.heap.stats().live_objects;
        it.collect(&[]).unwrap();
        // the dead key/value cycle, the self-keyed entry and the whole dead map are gone; `keep` stays.
        // 7 objects: k, v, k2, m2, k3, its value, and the function (no prototype object was read).
        assert_eq!(it.weak_tracked(), (1, 1));
        assert!(it.heap.stats().live_objects + 7 <= live0, "{} {}", it.heap.stats().live_objects, live0);
        assert_eq!(out(&mut it, "wm.get(keep).tag"), "kept");
        // a second collection changes nothing
        it.collect(&[]).unwrap();
        assert_eq!(it.weak_tracked(), (1, 1));
        // dropping the key frees the value
        out(&mut it, "keep = null; 0");
        it.collect(&[]).unwrap();
        assert_eq!(it.weak_tracked(), (1, 0));
    });
}

#[test]
fn a_reachable_value_retains_keys_of_other_maps_and_cycles_across_maps_die() {
    big(|| {
        let mut it = realm();
        out(&mut it, "var m1 = new WeakMap(), m2 = new WeakMap(), s = new WeakSet(); var root = {};
            (function () { var a = {}, b = {}, c = {}, d = {}; m1.set(root, a); m2.set(a, b); s.add(b); m1.set(b, c); m2.set(c, 'end');
              var x = {}, y = {}; m1.set(x, y); m2.set(y, x); s.add(x); s.add(y); })(); 0");
        it.collect(&[]).unwrap();
        // chain root -> a -> b -> c is alive; the x/y cycle only through weak edges is not
        assert_eq!(it.weak_tracked(), (3, 5));
        assert_eq!(out(&mut it, "var a = m1.get(root), b = m2.get(a), c = m1.get(b); [s.has(b), m2.get(c)].join()"), "true,end");
        out(&mut it, "a = b = c = null; root = {}; 0");
        it.collect(&[]).unwrap();
        assert_eq!(it.weak_tracked(), (3, 0));
    });
}

fn chain(n: usize, reverse: bool) -> (usize, usize, usize) {
    let mut it = realm();
    let setup = format!(
        "var N = {n}, ms = [], head = {{}}; for (var i = 0; i < N; i++) ms.push(new WeakMap());
         (function () {{ var prev = head; for (var i = 0; i < N; i++) {{ var nx = {{ i: i }}; ms[{idx}].set(prev, nx); prev = nx; }} }})(); 0",
        idx = if reverse { "N - 1 - i" } else { "i" }
    );
    out(&mut it, &setup);
    it.collect(&[]).unwrap();
    let alive = it.weak_tracked();
    assert_eq!(alive, (n, n));
    let tail = out(&mut it, &format!("var p = head, c = 0; for (var i = 0; i < N; i++) {{ var m = ms[{}]; var nx = m.get(p); if (!nx) break; p = nx; c++; }} c", if reverse { "N - 1 - i" } else { "i" }));
    assert_eq!(tail, n.to_string());
    out(&mut it, "p = nx = m = null; head = null; 0");
    it.collect(&[]).unwrap();
    assert_eq!(it.weak_tracked(), (n, 0));
    (alive.0, alive.1, 0)
}

#[test]
fn long_ephemeron_chains_resolve_in_either_order() {
    big(|| {
        chain(3000, false);
        chain(3000, true);
    });
}

/// A collection of a million-entry chain must stay linear: each entry is looked at a bounded number of times.
#[test]
fn ephemeron_work_is_linear_in_chain_length() {
    big(|| {
        let mut work = Vec::new();
        for n in [1000usize, 4000, 16000] {
            let mut it = realm();
            out(&mut it, &format!("var N = {n}, ms = [], head = {{}}; for (var i = 0; i < N; i++) ms.push(new WeakMap());
                (function () {{ var prev = head; for (var i = 0; i < N; i++) {{ var nx = {{}}; ms[N - 1 - i].set(prev, nx); prev = nx; }} }})(); 0"));
            it.collect(&[]).unwrap();
            work.push(it.weak_visits());
        }
        // 4x the chain gives about 4x the visits (never 16x)
        assert!(work[1] <= work[0] * 5 && work[2] <= work[1] * 5, "{work:?}");
        assert!(work[2] >= work[1] * 3, "{work:?}");
    });
}

#[test]
fn values_with_closures_classes_and_async_state_survive_through_a_live_key() {
    big(|| {
        let mut it = realm();
        out(&mut it, "var k = {}, w = new WeakMap(), res, out = [];
            (function () {
              var secret = { n: 41 };
              w.set(k, function () { return secret.n + 1; });
              var kc = {}; w.set(kc, class Q { static s = secret; m() { return secret.n; } });
              var ka = {}; w.set(ka, (async function () { var local = { x: 42 }; await new Promise(function (r) { res = r; }); return local.x; })());
              globalThis.kc = kc; globalThis.ka = ka;
            })(); 0");
        it.collect(&[]).unwrap();
        assert_eq!(out(&mut it, "w.get(k)()"), "42");
        assert_eq!(out(&mut it, "var Q = w.get(kc); new Q().m() + Q.s.n"), "82");
        it.collect(&[]).unwrap();
        out(&mut it, "res(0); w.get(ka).then(function (v) { out.push(v); }); 0");
        it.drain_microtasks().unwrap();
        assert_eq!(out(&mut it, "out.join()"), "42");
        // the same values die with their keys
        out(&mut it, "k = kc = ka = Q = null; globalThis.kc = globalThis.ka = null; 0");
        it.collect(&[]).unwrap();
        assert_eq!(it.weak_tracked(), (1, 0));
    });
}

#[test]
fn deleted_globals_do_not_kill_the_intrinsics() {
    big(|| {
        let mut it = realm();
        let (mp, sp) = (it.weak.map_proto, it.weak.set_proto);
        out(&mut it, "delete globalThis.WeakMap; delete globalThis.WeakSet; 0");
        for _ in 0..2 {
            it.collect(&[]).unwrap();
        }
        let ck = it.k_constructor;
        for p in [mp, sp] {
            let Ok(Some(Property::Data { value: Value::Object(c), .. })) = it.heap.get_own_property(p, ck) else { panic!("constructor lost") };
            assert!(it.heap.get_prototype_of(c).is_ok());
        }
        // a value made before the collection still works with its brand
        let mut it = realm();
        out(&mut it, "var w = new WeakMap(), s = new WeakSet(), o = {}; w.set(o, 1); s.add(o); delete globalThis.WeakMap; delete globalThis.WeakSet; 0");
        it.collect(&[]).unwrap();
        assert_eq!(out(&mut it, "[w.get(o), s.has(o), Object.prototype.toString.call(w), w.constructor.name].join()"), "1,true,[object WeakMap],WeakMap");
    });
}

#[test]
fn slot_reuse_cannot_alias_a_dead_key() {
    big(|| {
        let mut it = realm();
        out(&mut it, "var w = new WeakMap(), s = new WeakSet(); (function () { for (var i = 0; i < 500; i++) { var o = {}; w.set(o, i); s.add(o); } })(); 0");
        assert_eq!(it.weak_tracked(), (2, 1000));
        it.collect(&[]).unwrap();
        assert_eq!(it.weak_tracked(), (2, 0));
        // fresh objects take the freed slots. None may look like a stored key
        assert_eq!(out(&mut it, "var hits = 0, fresh = []; for (var i = 0; i < 2000; i++) { var o = {}; fresh.push(o); if (w.has(o) || s.has(o) || w.get(o) !== undefined) hits++; } hits"), "0");
        // and a dead WeakMap object slot reused by a plain object is not a WeakMap
        out(&mut it, "var dead = []; for (var i = 0; i < 100; i++) dead.push(new WeakMap()); dead = null; 0");
        it.collect(&[]).unwrap();
        assert_eq!(it.weak_tracked(), (2, 0));
        assert_eq!(out(&mut it, "var bad = 0; for (var i = 0; i < 300; i++) { try { WeakMap.prototype.has.call({}, {}); bad++; } catch (e) {} } bad"), "0");
    });
}

/// A host that owns a value through an object: the collector asks it for edges of every object it reaches.
#[derive(Default)]
struct H {
    owned: RefCell<HashMap<ObjId, Value>>,
}

impl Host for H {
    fn call(&self, it: &mut Interp<'_>, id: u32, _this: Value, args: &[Value], _is_new: bool) -> Result<Value, Error> {
        let a = |i: usize| args.get(i).copied().unwrap_or(Value::Undefined);
        match id {
            0 => {
                let Value::Object(o) = a(0) else { return Err(it.type_error("owner")) };
                self.owned.borrow_mut().insert(o, a(1));
                Ok(Value::Undefined)
            }
            1 => {
                let Value::Object(o) = a(0) else { return Ok(Value::Undefined) };
                Ok(self.owned.borrow().get(&o).copied().unwrap_or(Value::Null))
            }
            _ => Ok(Value::Number(self.owned.borrow().len() as f64)),
        }
    }
    fn trace(&self, owner: ObjId, out: &mut Vec<Value>) {
        out.extend(self.owned.borrow().get(&owner).copied());
    }
    fn prune(&self, live: &dyn Fn(ObjId) -> bool) {
        self.owned.borrow_mut().retain(|o, _| live(*o));
    }
}

#[test]
fn host_owned_edges_work_through_keys_and_values() {
    big(|| {
        let mut it = realm();
        let h = Rc::new(H::default());
        it.set_host(h.clone()).unwrap();
        let g = it.global_object();
        for (i, n) in ["attach", "owned", "count"].iter().enumerate() {
            let f = it.host_function(i as u32, n, 0, false).unwrap();
            let k = it.heap.key_from_str(n).unwrap();
            it.define_data(g, k, Value::Object(f), true, false, true).unwrap();
        }
        // key -> (host) -> value of a weak entry in another map; value object -> (host) -> another key's value
        out(&mut it, "var k = {}, w = new WeakMap(), w2 = new WeakMap();
            (function () { var owner = {}, inner = { tag: 'host-value' }; var k2 = {}; attach(owner, k2); w2.set(k2, inner); w.set(k, owner); })(); 0");
        it.collect(&[]).unwrap();
        // k alive -> owner alive (value) -> host edge keeps k2 alive -> w2 entry (key k2) keeps `inner`
        assert_eq!(out(&mut it, "var owner = w.get(k); w2.get(owned(owner)).tag"), "host-value");
        assert_eq!(out(&mut it, "count()"), "1");
        // a host-owned key: the key's own host edge is traced when the key is reached, not when the map is
        out(&mut it, "var hk = {}; (function () { var hv = { x: 'hv' }; attach(hk, hv); w.set(hk, 1); w2.set(hv, 'via-host'); })(); owner = null; 0");
        it.collect(&[]).unwrap();
        assert_eq!(out(&mut it, "w2.get(owned(hk))"), "via-host");
        // drop both keys: the host metadata of the dead owners is pruned
        out(&mut it, "k = hk = null; 0");
        it.collect(&[]).unwrap();
        assert_eq!(out(&mut it, "count()"), "0");
        assert_eq!(it.weak_tracked(), (2, 0));
    });
}

#[test]
fn the_old_failed_checkpoint_and_limit_contract_still_holds() {
    big(|| {
        // a step limit inside a weak call is a limit, never a catchable error
        let mut it = Interp::realm(Heap::default(), EvalLimits { max_steps: 300, ..Default::default() }).unwrap();
        assert!(out(&mut it, "var w = new WeakMap(); try { for (var i = 0; i < 100000; i++) w.set({}, i); } catch (e) { 'caught' }").starts_with("limit:"));
    });
}

#[test]
fn weak_values_keep_cross_script_closures_and_view_buffers_without_retaining_cycles() {
    big(|| {
        let mut it = realm();
        assert_eq!(out(&mut it, "var key = {}, wm = new WeakMap(); function sameId() { return 1; } 0"), "0");
        assert_eq!(out(&mut it, "(function () { var bytes = new Uint8Array([31, 47]); bytes.buffer.back = key; wm.set(key, function sameId() { return bytes; }); })(); 0"), "0");
        it.collect(&[]).unwrap();
        let view = it.run_script("wm.get(key)()").unwrap();
        assert_eq!(it.bytes_of(view), Some(&[31, 47][..]));
        // The buffer's ordinary property points back to the weak key. It must not turn this cycle into a root.
        assert_eq!(out(&mut it, "key = null; 0"), "0");
        it.collect(&[]).unwrap();
        assert_eq!(it.weak_tracked(), (1, 0));
        assert!(it.bytes_of(view).is_none());
    });
}
