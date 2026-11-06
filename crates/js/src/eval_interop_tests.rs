//! Tests of the seams between the Array/Object methods, Symbol, Map, Set, the iterator protocol, String, RegExp and
//! Promise. The corpus (`tools/fixtures/js/seam-cases.txt`) is checked against what Node gave (`seam-cases.expected`,
//! made by `tools/js/promise_oracle.mjs`; `tools/js/seam_check.sh` re-runs Node). The tests below cover what Node
//! cannot show: gates, limits, several scripts with colliding node ids, collection and metadata reclamation.

use super::*;
use crate::heap::Heap;
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

fn corpus() -> Vec<(&'static str, String)> {
    let mut list: Vec<(&str, Vec<&str>)> = Vec::new();
    for line in include_str!("../../../tools/fixtures/js/seam-cases.txt").lines() {
        if let Some(n) = line.strip_prefix("=== ") {
            list.push((n.trim(), Vec::new()));
        } else if let Some(c) = list.last_mut() {
            c.1.push(line);
        }
    }
    list.into_iter().map(|(n, l)| (n, l.join("\n").trim_end_matches('\n').to_string())).collect()
}

/// A case that ends with a reader function, read before and after the microtask checkpoint. `mid` runs in between.
fn run_reader(src: &str, mid: impl FnOnce(&mut Interp, Value)) -> String {
    let prog = parse_script(src, Limits::default()).unwrap_or_else(|e| panic!("{src:?}: {e}"));
    let mut it = Interp::new(&prog, Heap::default(), EvalLimits { max_steps: 5_000_000, ..Default::default() }).expect("setup");
    let reader = match it.run() {
        Ok(v) => v,
        Err(e) => return fail(&mut it, e),
    };
    let read = |it: &mut Interp| match it.call(reader, Value::Undefined, &[]) {
        Ok(v) => Ok(esc(&it.display(v))),
        Err(e) => Err(fail(it, e)),
    };
    let pre = match read(&mut it) {
        Ok(s) => s,
        Err(s) => return s,
    };
    mid(&mut it, reader);
    if let Err(e) = it.drain_microtasks() {
        return fail(&mut it, e);
    }
    match read(&mut it) {
        Ok(post) => format!("{pre} ## {post}"),
        Err(s) => s,
    }
}

#[test]
fn corpus_matches_node() {
    big(|| {
        let want: Vec<&str> = include_str!("../../../tools/fixtures/js/seam-cases.expected").lines().collect();
        let cases = corpus();
        assert_eq!(cases.len(), want.len());
        assert!(cases.len() >= 12);
        let mut bad = Vec::new();
        for ((name, src), w) in cases.iter().zip(want) {
            let got = format!("{name}\t{}", run_reader(src, |_, _| {}));
            if got != w {
                bad.push(format!("node: {w}\nbare: {got}"));
            }
        }
        assert!(bad.is_empty(), "{} differences:\n{}", bad.len(), bad.join("\n"));
    });
}

/// Every case again with a collection between the two reads. What the case keeps must stay valid and read the same.
#[test]
fn corpus_survives_collect() {
    big(|| {
        let want: Vec<&str> = include_str!("../../../tools/fixtures/js/seam-cases.expected").lines().collect();
        for ((name, src), w) in corpus().iter().zip(want) {
            let got = format!("{name}\t{}", run_reader(src, |it, reader| {
                it.collect(&[reader]).unwrap();
            }));
            assert_eq!(got, w, "{name}");
        }
    });
}

fn run_with(src: &str, steps: u64, heap: Heap) -> (String, Interp<'static>) {
    let mut it = Interp::realm(heap, EvalLimits { max_steps: steps, ..Default::default() }).expect("realm");
    let r = it.run_script(src);
    let s = match r {
        Ok(v) => format!("ok:{}", esc(&it.display(v))),
        Err(e) => fail(&mut it, e),
    };
    (s, it)
}

fn run(src: &str) -> String {
    let src = src.to_string();
    big(move || run_with(&src, 2_000_000, Heap::default()).0)
}

#[test]
fn custom_hooks_that_are_out_of_scope_are_explicit_gates() {
    let gate = |src: &str, what: &str| assert_eq!(run(src), format!("unsupported:{what}"), "{src}");
    let species = "Array species constructor (subclass or custom constructor)";
    gate("var a=[1]; a.constructor={[Symbol.species]: function F(){}}; a.map(function(x){return x})", species);
    gate("var a=[1]; a.constructor={[Symbol.species]: function F(){}}; a.concat()", species);
    gate("var a=[1]; function C(){} C[Symbol.species]=function G(){}; a.constructor=C; a.slice()", species);
    gate("var a=[1]; function C(){} Object.defineProperty(C, Symbol.species, {get(){ return function H(){}; }}); a.constructor=C; a.filter(function(){return true})", species);
    gate("var p=Promise.resolve(1); p.constructor={[Symbol.species]: function F(){}}; p.then(function(){})", "Promise species constructor other than Promise");
    gate("var p=Promise.resolve(1); p.constructor={[Symbol.species]: function F(){}}; p.finally(function(){})", "Promise species constructor other than Promise");
    gate("new RegExp({[Symbol.match]: true, source: 'a', flags: ''})", "RegExp constructor with a regexp-like object (Symbol.match)");
    gate("var r=/a/; r.exec=function(){ return null; }; 'a'.match(r)", "RegExp with a replaced exec method");
    gate("var r=/a/; r.exec=function(){ return null; }; 'a'.replace(r, 'b')", "RegExp with a replaced exec method");
    gate("var r=/a/; r.exec=function(){ return null; }; 'a'.split(r)", "RegExp with a replaced exec method");
    gate("function F(){} Array.from.call(F, new Set([1]))", "Array.from with a non-Array this");
    // What is not a gate: no species, a null species, the intrinsic, and ordinary iterables.
    assert_eq!(run("var a=[1,2]; a.constructor={}; a.map(function(x){return x}).length"), "ok:2");
    assert_eq!(run("var a=[1,2]; a.constructor=function F(){}; a.slice().length"), "ok:2");
    assert_eq!(run("var a=[1,2]; a.constructor={[Symbol.species]: null}; a.filter(function(){return true}).length"), "ok:2");
    assert_eq!(run("var a=[1,2]; a.constructor={[Symbol.species]: Array}; a.concat([3]).length"), "ok:3");
    assert_eq!(run("Array.from(new Map([[1,2]])).length"), "ok:1");
    // `'x'.search` is bound since batch 1: it finds the match at index 0. matchAll exists: it returns the iterator.
    assert_eq!(run("try { 'x'.search(/x/) } catch (e) { e.name }"), "ok:0");
    assert_eq!(run("typeof 'x'.matchAll(/x/g).next().value.index"), "ok:number");
}

/// Two scripts with the same shape (the same node ids), iterators, hooks, comparators and map functions crossing
/// between them, a collection between every step.
#[test]
fn scripts_with_colliding_node_ids_cross_call_through_natives() {
    big(|| {
        let tpl = include_str!("../../../tools/fixtures/js/seam-multiscript.js");
        let mut it = Interp::realm(Heap::default(), EvalLimits { max_steps: 2_000_000, ..Default::default() }).unwrap();
        for tag in ["A", "B"] {
            let r = it.run_script(&tpl.replace('@', tag));
            assert_eq!(it.display(r.unwrap()), tag);
            it.collect(&[]).unwrap();
        }
        let before = it.stats();
        let steps = [
            ("Array.from(srcA, mapB).join()", "A1/0,A2/1,A3/2,A4/3"),
            ("Array.from(srcB, mapA).join()", "B1/0,B2/1,B3/2,B4/3"),
            ("arrA.map(mapB).join()", "3/0,1/1,2/2"),
            ("arrB.slice().sort(cmpA).join()", "1,2,3"),
            ("[0].concat(spreadB, spreadA).join()", "0,B0,B1,A0,A1"),
            ("JSON.stringify(Object.fromEntries(entA)) + JSON.stringify(Object.fromEntries(entB))", "{\"k1\":\"A\",\"k2\":\"A\"}{\"k1\":\"B\",\"k2\":\"B\"}"),
            ("(function(){ for (var x of srcA) { if (x === 'A2') break; } for (var y of srcB) { break; } return closedA + ':' + closedB; })()", "1:1"),
            ("[...srcA].length + ':' + [...new Set(srcB)].length + ':' + Array.from(new Map([[1, 'x']]), mapA)", "4:4:1,x/0"),
            ("var [p1, p2] = srcB; p1 + p2 + ':' + closedB", "B1B2:2"),
        ];
        for (src, want) in steps {
            let r = it.run_script(src);
            assert_eq!(show(&mut it, r), want, "{src}");
            it.collect(&[]).unwrap();
        }
        // a throw from the other script's callback closes the iterator that script owns, once
        let r = it.run_script("try { Array.from(srcA, function (x) { if (x === 'A2') throw new RangeError('stop'); return x; }); } catch (e) { e.name + ':' + closedA }");
        assert_eq!(show(&mut it, r), "RangeError:2");
        it.collect(&[]).unwrap();
        let after = it.stats();
        assert!(after.functions <= before.functions + 2, "{before:?} {after:?}");
        assert!(after.live_envs <= before.live_envs + 2, "{before:?} {after:?}");
        let (colls, iters) = it.sc_tracked();
        assert!(colls <= 1 && iters <= 1, "{colls} {iters}");
    });
}

fn show(it: &mut Interp, r: Result<Value, Error>) -> String {
    match r {
        Ok(v) => it.display(v),
        Err(e) => fail(it, e),
    }
}

/// A limit ends the run: no `return` runs, `catch` and `finally` do not see it, and a later drain repeats it.
#[test]
fn a_limit_in_an_iterator_loop_runs_no_more_javascript() {
    big(|| {
        let src = "var closed = 0, fin = 0, caught = 0;
                   var inf = { [Symbol.iterator]() { return { next() { return { value: 1, done: false }; }, return() { closed++; return {}; } }; } };
                   var infe = { [Symbol.iterator]() { return { next() { return { value: [1, 2], done: false }; }, return() { closed++; return {}; } }; } };";
        for body in [
            "try { Array.from(inf); } catch (e) { caught++; } finally { fin++; }",
            "try { Object.fromEntries(infe); } catch (e) { caught++; } finally { fin++; }",
            "try { Array.from(inf, function (x) { return x; }); } catch (e) { caught++; } finally { fin++; }",
            "try { new Set(inf); } catch (e) { caught++; } finally { fin++; }",
        ] {
            let (r, mut it) = run_with(&format!("{src} {body}"), 200_000, Heap::default());
            assert_eq!(r, "limit:Steps", "{body}");
            let g = Value::Object(it.global_object());
            for name in ["closed", "fin", "caught"] {
                assert_eq!(it.get_str(g, name), "0", "{body}: {name}");
            }
        }
        // a limit of the iterator cap inside a fast path loop close does not run JS either
        let mut it = Interp::realm(Heap::default(), EvalLimits { max_steps: 2_000_000, ..Default::default() }).unwrap();
        it.set_sc_limits(ScLimits { max_iterators: 0, ..Default::default() });
        let r = it.run_script("var AIP = Object.getPrototypeOf([][Symbol.iterator]()); AIP.return = function () { throw 1; }; try { for (var x of [1, 2]) { break; } } catch (e) { 'caught' }");
        assert_eq!(show(&mut it, r), "limit:Sc(Iterators)");
    });
}

/// `concat` of a spreadable object with a huge length and `Array.from` of an array-like are charged before they work.
#[test]
fn spreadable_and_array_like_ranges_are_limits_before_work() {
    let lim = |src: &str| assert_eq!(run(src), "limit:Steps", "{src}");
    lim("var o = { length: 4294967294 }; o[Symbol.isConcatSpreadable] = true; [].concat(o)");
    lim("var o = { length: 4294967294 }; o[Symbol.isConcatSpreadable] = true; try { [].concat(o) } catch (e) { 'caught' }");
    lim("Array.from({ length: 4294967295 }, function () {})");
    lim("var big = []; big.length = 4294967294; big[Symbol.isConcatSpreadable] = true; [].concat(big)");
    // a result longer than 2^32-1 is a RangeError, which `try` can catch
    assert_eq!(run("var o = { length: 4294967295 }; o[Symbol.isConcatSpreadable] = true; try { [1].concat(o) } catch (e) { e.name }"), "ok:RangeError");
}

#[test]
fn array_iterator_metadata_is_reclaimed_and_stale_slots_never_alias() {
    big(|| {
        let mut it = Interp::realm(Heap::default(), EvalLimits { max_steps: 20_000_000, ..Default::default() }).unwrap();
        let before = it.stats();
        let r = it.run_script(
            "var keep = [1, 2].values(); var kept = new Set([1]).values(); var ks = 'ab'[Symbol.iterator]();
             for (var i = 0; i < 3000; i++) { var t = [i].entries(); t.next(); var s = ''[Symbol.iterator](); var m = new Map([[i, i]]).keys(); Array.from(new Set([i, i + 1])); }
             var AIP = Object.getPrototypeOf(keep), SIP = Object.getPrototypeOf(ks); 'ok'",
        );
        assert_eq!(show(&mut it, r), "ok");
        let (c0, i0) = it.sc_tracked();
        assert!(c0 > 1000 && i0 > 3000, "{c0} {i0}");
        it.collect(&[]).unwrap();
        let (c1, i1) = it.sc_tracked();
        assert!(c1 <= 2 && i1 <= 6, "{c1} {i1}");
        assert!(it.stats().functions <= before.functions + 2, "{:?}", it.stats());
        // the survivors still work, and a new object in a reused slot is not an iterator
        let r = it.run_script(
            "var out = [keep.next().value, kept.next().value, ks.next().value].join();
             var bad = 0; var objs = []; for (var j = 0; j < 4000; j++) objs.push({});
             for (var j = 0; j < objs.length; j++) { try { AIP.next.call(objs[j]); } catch (e) { bad++; } try { SIP.next.call(objs[j]); } catch (e) { bad++; } }
             out + ':' + bad",
        );
        assert_eq!(show(&mut it, r), "1,1,a:8000");
        // the only metadata still tracked is what the script can reach
        it.collect(&[]).unwrap();
        let (c2, i2) = it.sc_tracked();
        assert!(c2 <= 2 && i2 <= 6, "{c2} {i2}");
    });
}

/// The Promise checkpoint keeps reporting a failed job, also when the job ran the new Array, Symbol and String code.
#[test]
fn a_failed_job_in_the_new_code_stays_failed() {
    big(|| {
        let mut it = Interp::realm(Heap::default(), EvalLimits { max_steps: 300_000, ..Default::default() }).unwrap();
        it.run_script(
            "var inf = { [Symbol.iterator]() { return { next() { return { value: 1, done: false }; } }; } };
             var order = '';
             Promise.resolve(1).then(function () { order += 'a'; return Array.from(inf); }).then(function () { order += 'never'; });
             Promise.resolve(2).then(function () { order += 'b'; });",
        )
        .unwrap();
        let first = it.drain_microtasks();
        assert!(matches!(first, Err(Error::Limit(LimitKind::Steps))), "{first:?}");
        for _ in 0..2 {
            assert!(matches!(it.drain_microtasks(), Err(Error::Limit(LimitKind::Steps))));
        }
        let g = Value::Object(it.global_object());
        let order = it.get_str(g, "order");
        assert_eq!(order, "a");
    });
}

#[test]
fn promise_then_still_works_after_symbols_are_installed() {
    let r = run_with(
        "var log = []; var p = Promise.resolve(1);
         p.then(function (v) { log.push('t' + v); return Promise.resolve(v + 1); }).then(function (v) { log.push('u' + v); });
         p.catch(function () {}); p.finally(function () { log.push('f'); });
         Promise.reject(new Error('x')).catch(function (e) { log.push(e.message); });
         'started'",
        2_000_000,
        Heap::default(),
    );
    let (s, mut it) = r;
    assert_eq!(s, "ok:started");
    it.drain_microtasks().unwrap();
    let g = Value::Object(it.global_object());
    let log = it.get_str(g, "log");
    assert_eq!(log, "t1,f,x,u2");
}

/// `Heap::splice_dense` answers `None` for a non-extensible array: a move into a hole would add a property.
#[test]
fn the_dense_splice_fast_path_leaves_a_non_extensible_array_to_the_generic_algorithm() {
    let mut h = Heap::default();
    let a = h.new_array(None).unwrap();
    h.set(a, PropertyKey::Index(1), Value::Number(2.0)).unwrap();
    let len = 2;
    let ok = h.splice_dense(a, len, 0, 1, &[]).unwrap();
    assert!(ok.is_some(), "an extensible dense array takes the fast path");
    let b = h.new_array(None).unwrap();
    h.set(b, PropertyKey::Index(1), Value::Number(2.0)).unwrap();
    h.prevent_extensions(b).unwrap();
    assert!(h.splice_dense(b, 2, 0, 1, &[]).unwrap().is_none());
    let c = h.new_array(None).unwrap();
    h.set(c, PropertyKey::Index(0), Value::Number(1.0)).unwrap();
    h.prevent_extensions(c).unwrap();
    assert!(h.splice_dense(c, 1, 0, 1, &[]).unwrap().is_none());
}

#[test]
fn symbol_keys_and_primitive_prototypes_are_intact() {
    let s = |src: &str| run(src);
    assert_eq!(s("Object.getPrototypeOf(Symbol()) === Symbol.prototype"), "ok:true");
    assert_eq!(s("Object.getPrototypeOf('a') === String.prototype"), "ok:true");
    assert_eq!(s("String(Symbol('d')) + Symbol('e').toString() + Symbol('f').description"), "ok:Symbol(d)Symbol(e)f");
    assert_eq!(s("var o = {}; var k = Symbol('k'); o[k] = 1; Object.keys(o).length + ':' + JSON.stringify(o) + ':' + Object.assign({}, o)[k]"), "ok:0:{}:1");
    assert_eq!(s("var a = []; a[Symbol.iterator] === Array.prototype.values && Array.prototype.keys !== Array.prototype.entries"), "ok:true");
    assert_eq!(s("Map.prototype[Symbol.species]; Object.getOwnPropertyDescriptor(Map, Symbol.species).get.call(5)"), "ok:5");
}

/// A view prototype has virtual indices that are not in `elems`. The dense splice must leave such an array to the
/// generic algorithm, for any view length (a length-0 view stops an indexed write that reaches it).
#[test]
fn the_dense_splice_fast_path_refuses_an_array_whose_prototype_is_a_view() {
    let mut h = Heap::default();
    let buf = h.new_buffer(None, 4).unwrap();
    let v2 = h.new_view(None, buf, 0, 2).unwrap();
    h.view_set(v2, 1, 7);
    let a = h.new_array(Some(v2)).unwrap();
    h.set(a, PropertyKey::Index(0), Value::Number(1.0)).unwrap();
    h.set(a, PropertyKey::Index(1), Value::Number(2.0)).unwrap();
    assert!(h.splice_dense(a, 2, 0, 1, &[]).unwrap().is_none(), "virtual indices of the prototype are visible");
    assert!(matches!(h.get(a, PropertyKey::Index(1)).unwrap(), Get::Value(Value::Number(n)) if n == 2.0), "nothing changed");
    // the removed slot would show the inherited virtual byte through the hole it leaves
    let b = h.new_array(Some(v2)).unwrap();
    assert!(matches!(h.get(b, PropertyKey::Index(1)).unwrap(), Get::Value(Value::Number(n)) if n == 7.0));
    // a zero-length view: an indexed write is rejected at the prototype, so the array stays empty
    let v0 = h.new_view(None, buf, 0, 0).unwrap();
    let c = h.new_array(Some(v0)).unwrap();
    h.set(c, PropertyKey::Index(3), Value::Number(1.0)).unwrap();
    assert!(h.get_own_property(c, PropertyKey::Index(3)).unwrap().is_none(), "the write was rejected by the view prototype");
    assert!(h.splice_dense(c, 0, 0, 0, &[Value::Number(9.0)]).unwrap().is_none());
    assert!(h.get_own_property(c, PropertyKey::Index(0)).unwrap().is_none());
}
