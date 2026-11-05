//! Symbol, iterator protocol, Map and Set tests. The corpus (`tools/fixtures/js/sc-cases.txt`) is checked against what Node
//! gave (`sc-cases.expected`, made by `tools/js/promise_oracle.mjs`; `tools/js/sc_check.sh` re-runs Node).
//! The tests below cover what Node cannot show: caps, gates, work accounting, roots, GC and slot reuse.

use super::*;
use crate::heap::{Heap, HeapLimits};
use crate::parse::{parse_script, Limits};

fn big<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(64 << 20).spawn(f).unwrap().join().unwrap()
}

fn esc(s: &str) -> String {
    s.replace('\\', "\\\\").replace('\n', "\\n").replace('\r', "\\r").replace('\t', "\\t")
}

fn fail(it: &mut Interp, e: Error) -> String {
    match e {
        Error::Throw(v) if matches!(v, Value::Object(_)) => format!("throw:{}:{}", esc(&it.get_str(v, "name")), esc(&it.get_str(v, "message"))),
        Error::Throw(v) => format!("throw-value:{}", esc(&it.display(v))),
        Error::Unsupported(u) => format!("unsupported:{}", u.what),
        Error::Limit(k) => format!("limit:{k:?}"),
        e => format!("internal-error:{e:?}"),
    }
}

/// Run a case that ends with a reader function. `mid` runs between the first and the second read.
fn run_reader(src: &str, mid: impl FnOnce(&mut Interp, Value)) -> (String, String) {
    let prog = parse_script(src, Limits::default()).unwrap_or_else(|e| panic!("{src:?}: {e}"));
    let mut it = Interp::new(&prog, Heap::default(), EvalLimits { max_steps: 20_000_000, ..Default::default() }).expect("setup");
    let reader = match it.run() {
        Ok(v) => v,
        Err(e) => {
            let s = fail(&mut it, e);
            return (s.clone(), s);
        }
    };
    let read = |it: &mut Interp| match it.call(reader, Value::Undefined, &[]) {
        Ok(v) => esc(&it.display(v)),
        Err(e) => fail(it, e),
    };
    let a = read(&mut it);
    mid(&mut it, reader);
    (a, read(&mut it))
}

fn corpus() -> Vec<(&'static str, String)> {
    let mut list: Vec<(&str, Vec<&str>)> = Vec::new();
    for line in include_str!("../../../tools/fixtures/js/sc-cases.txt").lines() {
        if let Some(n) = line.strip_prefix("=== ") {
            list.push((n.trim(), Vec::new()));
        } else if let Some(c) = list.last_mut() {
            c.1.push(line);
        }
    }
    list.into_iter().map(|(n, l)| (n, l.join("\n").trim_end_matches('\n').to_string())).collect()
}

#[test]
fn corpus_matches_node() {
    big(|| {
        let want: Vec<&str> = include_str!("../../../tools/fixtures/js/sc-cases.expected").lines().collect();
        let cases = corpus();
        assert_eq!(cases.len(), want.len());
        assert!(cases.len() >= 35);
        let mut bad = Vec::new();
        for ((name, src), w) in cases.iter().zip(want) {
            let (a, b) = run_reader(src, |_, _| {});
            let got = format!("{name}\t{a} ## {b}");
            if got != w {
                bad.push(format!("node: {w}\nbare: {got}"));
            }
        }
        assert!(bad.is_empty(), "{} differences:\n{}", bad.len(), bad.join("\n"));
    });
}

/// Every case again, but with an explicit collection between two reads. Whatever the case keeps reachable
/// (its log, its Maps, Sets, iterators, symbols) must still be valid and give the same text.
#[test]
fn corpus_survives_collect() {
    big(|| {
        for (name, src) in corpus() {
            let (a, b) = run_reader(&src, |it, reader| {
                it.collect(&[reader]).unwrap();
            });
            assert_eq!(a, b, "{name}");
            assert!(!a.contains("internal-error"), "{name}: {a}");
        }
    });
}

fn with_limits(src: &str, sc: ScLimits, heap: HeapLimits) -> String {
    let src = src.to_string();
    big(move || {
        let prog = parse_script(&src, Limits::default()).unwrap();
        let mut it = Interp::new(&prog, Heap::new(heap).unwrap(), EvalLimits { max_steps: 2_000_000, ..Default::default() }).unwrap();
        it.set_sc_limits(sc);
        match it.run() {
            Ok(v) => format!("ok:{}", esc(&it.display(v))),
            Err(e) => fail(&mut it, e),
        }
    })
}

fn small_heap() -> HeapLimits {
    HeapLimits { max_bytes: 256 << 20, ..Default::default() }
}

fn run(src: &str) -> String {
    with_limits(src, ScLimits::default(), small_heap())
}

#[test]
fn every_cap_is_a_distinct_limit_and_not_catchable() {
    let c = |f: &dyn Fn(&mut ScLimits)| {
        let mut l = ScLimits::default();
        f(&mut l);
        l
    };
    let reg = c(&|l| l.max_registry = 3);
    assert_eq!(with_limits("Symbol.for('a'); Symbol.for('b'); Symbol.for('c'); Symbol.for('a') === Symbol.for('a')", reg, HeapLimits::default()), "ok:true");
    assert_eq!(with_limits("Symbol.for('a'); Symbol.for('b'); Symbol.for('c'); try { Symbol.for('d'); } catch (e) { 'caught'; }", reg, HeapLimits::default()), "limit:Sc(Registry)");
    let colls = c(&|l| l.max_collections = 2);
    assert_eq!(with_limits("new Map(); new Set(); try { new Map(); } catch (e) {}", colls, HeapLimits::default()), "limit:Sc(Collections)");
    let ent = c(&|l| l.max_entries = 4);
    assert_eq!(with_limits("var m = new Map(); for (var i = 0; i < 4; i++) m.set(i, i); m.set(0, 'overwrite ok'); m.size", ent, HeapLimits::default()), "ok:4");
    assert_eq!(with_limits("var m = new Map(); for (var i = 0; i < 5; i++) m.set(i, i);", ent, HeapLimits::default()), "limit:Sc(Entries)");
    assert_eq!(with_limits("var s = new Set([1, 2, 3, 4, 5]);", ent, HeapLimits::default()), "limit:Sc(Entries)");
    // a full collection with holes compacts instead of failing
    assert_eq!(with_limits("var m = new Map(); for (var i = 0; i < 4; i++) m.set(i, i); m.delete(0); m.delete(1); m.set(9, 9); m.set(10, 10); m.size", ent, HeapLimits::default()), "ok:4");
    let total = c(&|l| l.max_total_entries = 5);
    assert_eq!(with_limits("var a = new Map([[1, 1], [2, 2], [3, 3]]); var b = new Set([1, 2, 3]);", total, HeapLimits::default()), "limit:Sc(TotalEntries)");
    let its = c(&|l| l.max_iterators = 3);
    assert_eq!(with_limits("var m = new Map(); m.keys(); m.values(); [][Symbol.iterator](); 1", its, HeapLimits::default()), "ok:1");
    assert_eq!(with_limits("var m = new Map(); m.keys(); m.values(); m.entries(); m.keys();", its, HeapLimits::default()), "limit:Sc(Iterators)");
    // for-of and spread over a Map, Set or array make no iterator object (fast path)
    assert_eq!(with_limits("var m = new Map([[1, 2]]); for (var i = 0; i < 100; i++) { for (var x of m) {} [...m]; var [a] = m; }; 1", its, HeapLimits::default()), "ok:1");
    let buf = c(&|l| l.max_iter_buffer = 10);
    assert_eq!(with_limits("function f() { return arguments.length; } f(...[1, 2, 3, 4, 5, 6, 7, 8, 9, 10])", buf, HeapLimits::default()), "ok:10");
    assert_eq!(with_limits("function f() {} f(...[1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11])", buf, HeapLimits::default()), "limit:Sc(IterBuffer)");
    assert_eq!(with_limits("var [a, ...r] = new Set([1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12]);", buf, HeapLimits::default()), "limit:Sc(IterBuffer)");
    // heap symbol cap is the heap's own limit
    let heap = HeapLimits { max_symbols: 40, ..Default::default() };
    assert!(with_limits("for (var i = 0; i < 100; i++) Symbol('x');", ScLimits::default(), heap).starts_with("limit:Heap(Symbols)"));
}

#[test]
fn an_infinite_iterator_ends_with_a_limit_and_runs_no_js_afterwards() {
    // for-of: a limit ends the run; the iterator's return() must not run after it, and try/finally cannot catch it
    big(|| {
        let src = "var closed = 0, fin = 0; var it = { [Symbol.iterator]() { return this; }, next() { return { value: 1, done: false }; }, return() { closed++; return {}; } };
            try { for (var x of it) {} } catch (e) { fin = 1; } finally { fin += 2; }";
        let prog = parse_script(src, Limits::default()).unwrap();
        let mut it = Interp::new(&prog, Heap::new(small_heap()).unwrap(), EvalLimits { max_steps: 500_000, ..Default::default() }).unwrap();
        assert!(matches!(it.run(), Err(Error::Limit(LimitKind::Steps))));
        assert!(matches!(global(&mut it, "closed"), Value::Number(n) if n == 0.0));
        assert!(matches!(global(&mut it, "fin"), Value::Number(n) if n == 0.0));
    });
    // spread of an infinite iterator stops at the buffer cap (arguments, rest) or the heap cap (array literal)
    let src = "var it = { [Symbol.iterator]() { return this; }, next() { return { value: 1, done: false }; } }; var a = [...it];";
    let heap = HeapLimits { max_bytes: 1 << 20, ..Default::default() };
    assert!(with_limits(src, ScLimits::default(), heap).starts_with("limit:Heap(Bytes)"));
    let src = "var it = { [Symbol.iterator]() { return this; }, next() { return { value: 1, done: false }; } }; function f() {} f(...it);";
    let lim = ScLimits { max_iter_buffer: 1000, ..Default::default() };
    assert_eq!(with_limits(src, lim, small_heap()), "limit:Sc(IterBuffer)");
    let src = "var it = { [Symbol.iterator]() { return this; }, next() { return { value: 1, done: false }; } }; var [a, ...r] = it;";
    assert_eq!(with_limits(src, lim, small_heap()), "limit:Sc(IterBuffer)");
}

#[test]
fn native_walks_consume_work_including_holes_and_sparse_lengths() {
    // an array of length 2^32-1 with no elements: for-of and an array iterator pay a step per index
    assert_eq!(run("var a = []; a.length = 4294967295; var n = 0; for (var x of a) n++;"), "limit:Steps");
    let r = run("var a = []; a.length = 4294967295; var it = a[Symbol.iterator](); while (!it.next().done) {}");
    assert!(r == "limit:Steps" || r == "limit:Heap(Bytes)", "{r}");
    // a Map iterated after most entries were deleted: holes are charged
    let src = "var m = new Map(); for (var i = 0; i < 100000; i++) m.set(i, i); m.clear(); 1";
    let prog_src = src.to_string();
    big(move || {
        let prog = parse_script(&prog_src, Limits::default()).unwrap();
        let mut it = Interp::new(&prog, Heap::default(), EvalLimits { max_steps: 5_000_000, ..Default::default() }).unwrap();
        let before = it.stats().steps;
        it.run().unwrap();
        // 100000 sets plus the clear (100000 slots) plus the loop's own steps
        assert!(it.stats().steps - before > 200_000, "{}", it.stats().steps);
    });
    // a forEach that keeps adding entries ends by a cap or the step limit, not by running forever
    let r = run("var m = new Map([[0, 0]]); m.forEach(function (v, k) { m.set(k + 1, 0); });");
    assert!(r == "limit:Sc(Entries)" || r == "limit:Steps", "{r}");
    // clear is charged by size: a tiny budget cannot clear a large map for free
    let big_src = "var m = new Map(); for (var i = 0; i < 3000; i++) m.set(i, i); var n = 0; for (var j = 0; j < 100; j++) { for (var i = 0; i < 3000; i++) m.set(i, i); m.clear(); }";
    let prog = big_src.to_string();
    big(move || {
        let p = parse_script(&prog, Limits::default()).unwrap();
        let mut it = Interp::new(&p, Heap::default(), EvalLimits { max_steps: 300_000, ..Default::default() }).unwrap();
        assert!(matches!(it.run(), Err(Error::Limit(LimitKind::Steps))));
    });
}

#[test]
fn constructor_limits_and_errors_are_exact() {
    assert_eq!(run("Map()"), "throw:TypeError:Constructor Map requires 'new'");
    assert_eq!(run("Set()"), "throw:TypeError:Constructor Set requires 'new'");
    assert_eq!(run("new Symbol()"), "throw:TypeError:Symbol is not a constructor");
    assert_eq!(run("Symbol() + ''"), "throw:TypeError:Cannot convert a Symbol value to a string");
    assert_eq!(run("Map.prototype.get.call({}, 1)"), "throw:TypeError:Method Map.prototype.get called on incompatible receiver object");
    assert_eq!(run("Object(Symbol())"), "unsupported:Object(primitive) wrapper object");
    assert_eq!(run("new Map(5)"), "throw:TypeError:5 is not iterable");
    assert_eq!(run("new Map([1])"), "throw:TypeError:Iterator value 1 is not an entry object");
}

#[test]
fn caps_are_checked_before_a_throwing_constructor_leaves_garbage_that_collect_removes() {
    big(|| {
        let src = "var bad = { [Symbol.iterator]() { return { next() { return { value: 1, done: false }; } }; } };
            for (var i = 0; i < 50; i++) { try { new Map(bad); } catch (e) {} }";
        let prog = parse_script(src, Limits::default()).unwrap();
        let mut it = Interp::new(&prog, Heap::default(), EvalLimits::default()).unwrap();
        it.run().unwrap();
        assert_eq!(it.sc_tracked().0, 50);
        it.collect(&[]).unwrap();
        assert_eq!(it.sc_tracked(), (0, 0));
        assert_eq!(it.sc_entry_slots(), 0);
    });
}

fn global(it: &mut Interp, name: &str) -> Value {
    let k = it.heap.key_from_str(name).unwrap();
    match it.heap.get(it.global_object(), k).unwrap() {
        Get::Value(v) => v,
        _ => panic!("accessor"),
    }
}

#[test]
fn collect_prunes_dead_collections_and_iterators_and_keeps_live_ones_with_their_contents() {
    big(|| {
        let src = "var keep = new Map(); var keepSet = new Set();
            var payload = { text: 'payload' }; keep.set(payload, { inner: 'v' }); keep.set('s', new Set([payload])); keepSet.add({ deep: 1 }); keepSet.add(keep);
            for (var i = 0; i < 300; i++) { var t = new Map([[i, { i: i }], [{ o: i }, 'x']]); var u = new Set([t, i]); t.keys(); u.values(); [][Symbol.iterator](); }
            var held = new Map([[1, { alive: 'yes' }]]).entries();
            var hs = new Set(['only-in-iterator']).values();
            var sym = Symbol.for('registered'); var wk = Symbol.iterator; t = u = undefined; 0;";
        let prog = parse_script(src, Limits::default()).unwrap();
        let mut it = Interp::new(&prog, Heap::default(), EvalLimits::default()).unwrap();
        it.run().unwrap();
        let (c0, i0) = it.sc_tracked();
        assert!(c0 > 600 && i0 > 800, "{c0} {i0}");
        let before = it.heap.stats().live_objects;
        let freed = it.collect(&[]).unwrap();
        let (c1, i1) = it.sc_tracked();
        // live: keep, its inner Set, keepSet, held's Map, hs's Set; iterators: held and hs
        assert_eq!((c1, i1), (5, 2), "freed {freed:?}");
        assert!(it.heap.stats().live_objects < before / 4);
        assert_eq!(it.sc_entry_slots(), 2 + 1 + 2 + 1 + 1);
        // contents reachable only through the Maps, Sets and pending iterators are intact
        let src2 = global(&mut it, "keep");
        assert!(matches!(src2, Value::Object(_)));
        let f = {
            // call helpers through the interpreter: look the values up and probe them
            let get = |it: &mut Interp, obj: Value, key: &str| {
                let k = it.heap.key_from_str(key).unwrap();
                it.get_prop(obj, k).unwrap()
            };
            let hs = global(&mut it, "hs");
            let next = get(&mut it, hs, "next");
            let r = it.call(next, hs, &[]).unwrap();
            let v = get(&mut it, r, "value");
            it.display(v)
        };
        assert_eq!(f, "only-in-iterator");
    });
}

#[test]
fn slot_reuse_cannot_alias_a_dead_collection() {
    big(|| {
        // After collect the object slots are reused. A new plain object must not look like a Map or Set.
        let src = "var olds = []; for (var i = 0; i < 200; i++) olds.push(new Map([[i, i]]));
            function probe(o) { try { return Map.prototype.has.call(o, 1) + '/' + Set.prototype.has.call(o, 1); } catch (e) { return e.name; } }";
        let prog = parse_script(src, Limits::default()).unwrap();
        let mut it = Interp::new(&prog, Heap::default(), EvalLimits::default()).unwrap();
        it.run().unwrap();
        // drop the array: every Map is dead
        let k = it.heap.key_from_str("olds").unwrap();
        it.heap.set(it.global_object(), k, Value::Undefined).unwrap();
        it.collect(&[]).unwrap();
        assert_eq!(it.sc_tracked(), (0, 0));
        let probe = global(&mut it, "probe");
        for _ in 0..400 {
            let o = it.call(probe, Value::Undefined, &[Value::Undefined]).unwrap();
            assert_eq!(it.display(o), "TypeError");
        }
        // and a real object from a reused slot
        let make = {
            let k = it.heap.key_from_str("Object").unwrap();
            let Get::Value(c) = it.heap.get(it.global_object(), k).unwrap() else { panic!() };
            c
        };
        for _ in 0..300 {
            let o = it.call(make, Value::Undefined, &[]).unwrap();
            let r = it.call(probe, Value::Undefined, &[o]).unwrap();
            assert_eq!(it.display(r), "TypeError");
        }
    });
}

#[test]
fn deleted_intrinsic_globals_do_not_break_the_realm() {
    big(|| {
        let src = "var it = new Map([[1, 'one']]).keys(); var sit = new Set([2]).values(); var ait = [7][Symbol.iterator](); var regs = Symbol.for('r'); var M = new Map([[5, 6]]);
            function f() { return [it.next().value, sit.next().value, ait.next().value, Symbol.for('r') === regs, [...M].length, String(Symbol.iterator), M.size, typeof Symbol.toPrimitive].join(); }
            delete globalThis.Map; delete globalThis.Set; f";
        let prog = parse_script(src, Limits::default()).unwrap();
        let mut it = Interp::new(&prog, Heap::default(), EvalLimits::default()).unwrap();
        let f = it.run().unwrap();
        it.collect(&[f]).unwrap();
        it.collect(&[f]).unwrap();
        let r = it.call(f, Value::Undefined, &[]).unwrap();
        assert_eq!(it.display(r), "1,2,7,true,1,Symbol(Symbol.iterator),1,symbol");
        // the host collects again with everything still reachable: nothing is stale
        it.collect(&[f, r]).unwrap();
        let g = global(&mut it, "M");
        assert!(matches!(g, Value::Object(_)));
    });
}

#[test]
fn a_pending_iterator_keeps_only_its_own_collection_alive() {
    big(|| {
        let src = "var iters = []; for (var i = 0; i < 100; i++) { var m = new Map([[i, { big: i }]]); if (i % 10 === 0) iters.push(m.values()); }";
        let prog = parse_script(src, Limits::default()).unwrap();
        let mut it = Interp::new(&prog, Heap::default(), EvalLimits::default()).unwrap();
        it.run().unwrap();
        it.collect(&[]).unwrap();
        // 10 iterators, each keeps its Map; the 90 other Maps are gone; `m` (the last) is also live
        assert_eq!(it.sc_tracked(), (11, 10));
        assert_eq!(it.sc_entry_slots(), 11);
        let k = it.heap.key_from_str("iters").unwrap();
        it.heap.set(it.global_object(), k, Value::Undefined).unwrap();
        it.collect(&[]).unwrap();
        assert_eq!(it.sc_tracked(), (1, 0));
    });
}

#[test]
fn cycles_through_maps_and_sets_are_collected() {
    big(|| {
        let src = "var a = new Map(); var b = new Set(); a.set('b', b); b.add(a); a.set(a, a); var keep = 1; 0;";
        let prog = parse_script(src, Limits::default()).unwrap();
        let mut it = Interp::new(&prog, Heap::default(), EvalLimits::default()).unwrap();
        it.run().unwrap();
        it.collect(&[]).unwrap();
        assert_eq!(it.sc_tracked().0, 2);
        for n in ["a", "b"] {
            let k = it.heap.key_from_str(n).unwrap();
            it.heap.set(it.global_object(), k, Value::Undefined).unwrap();
        }
        it.collect(&[]).unwrap();
        assert_eq!(it.sc_tracked(), (0, 0));
        assert_eq!(it.sc_entry_slots(), 0);
    });
}

#[test]
fn compaction_keeps_live_iterators_correct_and_frees_deleted_values() {
    big(|| {
        let src = "var m = new Map(); var objs = 0; for (var i = 0; i < 5000; i++) m.set(i, { i: i });
            var it = m.keys(); var seen = []; for (var i = 0; i < 10; i++) seen.push(it.next().value);
            for (var i = 0; i < 4990; i++) m.delete(i);
            for (var i = 0; i < 20; i++) m.set(100000 + i, i);
            var rest = []; for (var r = it.next(); !r.done; r = it.next()) rest.push(r.value);
            function out() { return seen.join() + '/' + rest.join() + '/' + m.size; } out";
        let prog = parse_script(src, Limits::default()).unwrap();
        let mut it = Interp::new(&prog, Heap::default(), EvalLimits::default()).unwrap();
        let f = it.run().unwrap();
        let r = it.call(f, Value::Undefined, &[]).unwrap();
        let text = it.display(r);
        assert!(text.starts_with("0,1,2,3,4,5,6,7,8,9/4990,4991,"), "{text}");
        assert!(text.ends_with(",100019/30"), "{text}");
        assert!(it.sc_entry_slots() < 200, "holes were not compacted: {}", it.sc_entry_slots());
        let before = it.heap.stats().live_objects;
        it.collect(&[f]).unwrap();
        // the 4990 deleted values are free
        assert!(it.heap.stats().live_objects + 4000 < before, "{} -> {}", before, it.heap.stats().live_objects);
    });
}

#[test]
fn work_is_charged_for_compaction() {
    big(|| {
        let src = "var m = new Map(); for (var i = 0; i < 20000; i++) m.set(i, i); for (var i = 0; i < 19990; i++) m.delete(i);";
        let prog = parse_script(src, Limits::default()).unwrap();
        let mut it = Interp::new(&prog, Heap::default(), EvalLimits::default()).unwrap();
        it.run().unwrap();
        // 20000 sets, 19990 deletes, compactions moved more than the number of calls alone
        assert!(it.stats().steps > 2 * 20000 + 19990 * 2, "{}", it.stats().steps);
    });
}

#[test]
fn well_known_symbols_and_registry_survive_collect_and_are_stable() {
    big(|| {
        let src = "var before = [Symbol.iterator, Symbol.toPrimitive, Symbol.hasInstance, Symbol.toStringTag, Symbol.for('x')];
            function check() { return [before[0] === Symbol.iterator, before[1] === Symbol.toPrimitive, before[2] === Symbol.hasInstance, before[3] === Symbol.toStringTag, before[4] === Symbol.for('x'), Symbol.keyFor(before[4]), Symbol.keyFor(before[0])].join(); } check";
        let prog = parse_script(src, Limits::default()).unwrap();
        let mut it = Interp::new(&prog, Heap::default(), EvalLimits::default()).unwrap();
        let f = it.run().unwrap();
        for _ in 0..3 {
            it.collect(&[f]).unwrap();
            let r = it.call(f, Value::Undefined, &[]).unwrap();
            assert_eq!(it.display(r), "true,true,true,true,true,x,");
        }
    });
}

#[test]
fn registry_entries_are_rooted_for_the_realm_lifetime() {
    big(|| {
        let src = "for (var i = 0; i < 1000; i++) Symbol.for('key' + i);  var probe = function (i) { return Symbol.keyFor(Symbol.for('key' + i)); }; probe";
        let prog = parse_script(src, Limits::default()).unwrap();
        let mut it = Interp::new(&prog, Heap::default(), EvalLimits::default()).unwrap();
        let f = it.run().unwrap();
        let before = it.heap.stats().live_symbols;
        it.collect(&[f]).unwrap();
        assert_eq!(it.heap.stats().live_symbols, before, "registered symbols must survive");
        let r = it.call(f, Value::Undefined, &[Value::Number(999.0)]).unwrap();
        assert_eq!(it.display(r), "key999");
    });
}

#[test]
fn unregistered_symbols_are_collected() {
    big(|| {
        let src = "var keep = Symbol('keep'); for (var i = 0; i < 1000; i++) { var o = {}; o[Symbol('tmp' + i)] = 1; }";
        let prog = parse_script(src, Limits::default()).unwrap();
        let mut it = Interp::new(&prog, Heap::default(), EvalLimits::default()).unwrap();
        it.run().unwrap();
        let before = it.heap.stats().live_symbols;
        it.collect(&[]).unwrap();
        let after = it.heap.stats().live_symbols;
        assert!(before > after + 900, "{before} -> {after}");
    });
}

#[test]
fn host_can_call_symbol_and_collection_functions_and_read_symbol_text() {
    big(|| {
        let src = "var m = new Map([['k', 'v']]); var s = Symbol('shown'); var o = {}; o[s] = 1; function bad() { return null.x; } var g = function () { return m.get('k'); }; g";
        let prog = parse_script(src, Limits::default()).unwrap();
        let mut it = Interp::new(&prog, Heap::default(), EvalLimits::default()).unwrap();
        let g = it.run().unwrap();
        let r = it.call(g, Value::Undefined, &[]).unwrap();
        assert_eq!(it.display(r), "v");
        let s = global(&mut it, "s");
        assert_eq!(it.display(s), "Symbol(shown)");
        it.collect(&[]).unwrap();
        let r = it.call(g, Value::Undefined, &[]).unwrap();
        assert_eq!(it.display(r), "v");
    });
}

#[test]
fn the_symbol_protocols_that_are_only_identities_are_reported_not_faked() {
    // asyncIterator, matchAll and search exist as identities only. `for await` and async iteration are not supported.
    assert_eq!(run("typeof Symbol.asyncIterator + typeof Symbol.isConcatSpreadable + typeof Symbol.match"), "ok:symbolsymbolsymbol");
    assert_eq!(run("typeof WeakMap + typeof WeakSet + typeof Reflect + typeof Proxy"), "ok:functionfunctionobjectundefined");
    assert_eq!(run("typeof ''[Symbol.iterator]"), "ok:function");
    assert_eq!(run("[...'ab'].length"), "ok:2");
}

#[test]
fn recursion_through_the_new_natives_ends_at_the_depth_guards() {
    let cases = [
        "var m = new Map([[1, 1]]); function r() { m.forEach(function () { r(); }); } r();",
        "function r() { for (var x of { [Symbol.iterator]() { return { next() { r(); return { done: true }; } }; } }) {} } r();",
        "function r() { var o = { [Symbol.toPrimitive]() { r(); return 1; } }; +o; } r();",
        "function r() { 1 instanceof { [Symbol.hasInstance]() { r(); return true; } }; } r();",
        "function r() { new Map({ [Symbol.iterator]() { r(); return [][Symbol.iterator](); } }); } r();",
        "function r() { var [a] = { [Symbol.iterator]() { return { next() { return { value: 1, done: false }; }, return() { r(); return {}; } }; } }; } r();",
    ];
    for src in cases {
        let r = run(src);
        assert!(r == "limit:CallDepth" || r == "limit:TraversalDepth", "{src}: {r}");
    }
}
