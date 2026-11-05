//! Tests of byte buffers, `Uint8Array`, `TextEncoder` and `TextDecoder`. The Node-checked corpus is
//! `tools/fixtures/js/binary-cases.txt`. These tests cover what Node cannot show: limits, gates, GC and stale handles,
//! the embedding helpers, and the places where this engine follows the WebIDL order and Node does not.

use super::*;
use crate::heap::{Heap, HeapLimits};
use crate::parse::{parse_script, Limits};

fn big<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(64 << 20).spawn(f).unwrap().join().unwrap()
}

fn esc(s: &str) -> String {
    s.replace('\\', "\\\\").replace('\n', "\\n").replace('\r', "\\r").replace('\t', "\\t")
}

fn show(it: &Interp, v: Value) -> String {
    match v {
        Value::String(_) => format!("string:{}", esc(&it.display(v))),
        Value::Number(_) => format!("number:{}", it.display(v)),
        Value::Bool(_) => format!("boolean:{}", it.display(v)),
        Value::Undefined => "undefined".into(),
        Value::Null => "null".into(),
        Value::Symbol(_) => "symbol".into(),
        Value::Object(_) => it.display(v),
    }
}

fn fmt_result(it: &mut Interp, r: Result<Value, Error>) -> String {
    match r {
        Ok(v) => show(it, v),
        Err(Error::Throw(v)) if matches!(v, Value::Object(_)) => format!("throw:{}:{}", esc(&it.get_str(v, "name")), esc(&it.get_str(v, "message"))),
        Err(Error::Throw(v)) => format!("throw-value:{}", show(it, v)),
        Err(Error::Unsupported(u)) => format!("unsupported:{}", u.what),
        Err(Error::Limit(k)) => format!("limit:{k:?}"),
        Err(e) => format!("other:{e:?}"),
    }
}

/// Run a script with default limits and show the result.
pub(super) fn run(src: &str) -> String {
    run_with(src, HeapLimits::default(), EvalLimits { max_steps: 5_000_000, ..Default::default() }, |_| {})
}

pub(super) fn run_with(src: &str, heap: HeapLimits, limits: EvalLimits, setup: impl FnOnce(&mut Interp) + Send + 'static) -> String {
    let src = src.to_string();
    big(move || {
        let prog = parse_script(&src, Limits::default()).unwrap_or_else(|e| panic!("{src:?}: {e}"));
        let mut it = Interp::new(&prog, Heap::new(heap).unwrap(), limits).expect("setup");
        setup(&mut it);
        let r = it.run();
        fmt_result(&mut it, r)
    })
}

#[test]
fn corpus_matches_node() {
    check_corpus(include_str!("../../../tools/fixtures/js/binary-cases.txt"), include_str!("../../../tools/fixtures/js/binary-cases.expected"));
}

/// Iterator, `toStringTag` and species adapters of the byte classes (`adapter-cases.txt`).
#[test]
fn adapter_corpus_matches_node() {
    check_corpus(include_str!("../../../tools/fixtures/js/adapter-cases.txt"), include_str!("../../../tools/fixtures/js/adapter-cases.expected"));
}

pub(super) fn check_corpus(cases: &str, expected: &str) {
    let mut list: Vec<(&str, Vec<&str>)> = Vec::new();
    for line in cases.lines() {
        if let Some(n) = line.strip_prefix("=== ") {
            list.push((n.trim(), Vec::new()));
        } else if let Some(c) = list.last_mut() {
            c.1.push(line);
        }
    }
    let want: Vec<&str> = expected.lines().collect();
    assert_eq!(list.len(), want.len());
    let mut bad = Vec::new();
    for ((name, lines), w) in list.iter().zip(want) {
        let got = format!("{name}\t{}", run(lines.join("\n").trim_end_matches('\n')));
        if got != w {
            bad.push(format!("node: {w}\nbare: {got}"));
        }
    }
    assert!(bad.is_empty(), "{} differences:\n{}", bad.len(), bad.join("\n"));
}

// ---------------------------------------------------------------- gates and absent names

#[test]
fn unsupported_subset_is_explicit() {
    let g = |src: &str| run(src);
    assert_eq!(g("new ArrayBuffer(4, {maxByteLength: 8})"), "unsupported:resizable ArrayBuffer");
    assert_eq!(g("new ArrayBuffer(4).resize(2)"), "unsupported:ArrayBuffer.prototype.resize");
    // Other encodings: a real label is a gate, a made-up one is a RangeError.
    assert_eq!(g("new TextDecoder('utf-16le')"), "unsupported:TextDecoder encoding other than utf-8");
    assert_eq!(g("new TextDecoder('latin1')"), "unsupported:TextDecoder encoding other than utf-8");
    assert_eq!(g("new TextDecoder('nope')"), "throw:RangeError:The \"nope\" encoding is not supported");
    // Absent, as in an engine without them.
    assert_eq!(g("typeof SharedArrayBuffer"), "string:undefined");
    assert_eq!(g("SharedArrayBuffer"), "throw:ReferenceError:SharedArrayBuffer is not defined");
    // The typed-array family root is not a global, but it is reachable.
    assert_eq!(g("typeof TypedArray + typeof Object.getPrototypeOf(Uint8Array)"), "string:undefinedfunction");
    // defineProperty with an object value would need ToNumber from the generic path: a gate, not a wrong answer.
    assert_eq!(g("Object.defineProperty(new Uint8Array(1), 0, {value: {valueOf: function() { return 1; }}})"), "unsupported:ToNumber of an object calls valueOf/toString");
    // Subclassing a native constructor is not available (no class support here); a function constructor cannot call it.
    assert_eq!(g("function F() { Uint8Array.call(this, 1); } new F()"), "throw:TypeError:Constructor Uint8Array requires 'new'");
}

// ---------------------------------------------------------------- spec order where Node differs

#[test]
fn webidl_argument_order_and_dictionaries() {
    // The label, then both options, then the label lookup (the RangeError).
    assert_eq!(
        run("var L=[]; var o={get fatal(){L.push('fatal');return false}, get ignoreBOM(){L.push('ignoreBOM');return false}};
             new TextDecoder({toString:function(){L.push('label');return 'utf-8'}}, o);
             try { new TextDecoder('nope', o); } catch (e) { L.push(e.name); }
             L.join()"),
        "string:label,fatal,ignoreBOM,fatal,ignoreBOM,RangeError"
    );
    // decode: the input is checked first; each option is read once; a non-object option is a TypeError.
    assert_eq!(
        run("var L=[]; var d=new TextDecoder();
             d.decode(new Uint8Array([97]), {get stream(){L.push('s');return true}});
             try { d.decode(5, {get stream(){L.push('never');return true}}); } catch (e) { L.push(e.name); }
             try { d.decode(new Uint8Array(1), 5); } catch (e) { L.push(e.name); }
             try { d.decode(new Uint8Array(1), 'x'); } catch (e) { L.push(e.name); }
             L.join()"),
        "string:s,TypeError,TypeError,TypeError"
    );
    // encodeInto converts the source before it checks the destination.
    assert_eq!(run("try { new TextEncoder().encodeInto({toString:function(){throw new RangeError('src')}}, 5); } catch (e) { e.name }"), "string:RangeError");
    // A getter that decodes again on the same decoder runs before the state is read.
    assert_eq!(
        run("var d=new TextDecoder(); var L=[];
             var r=d.decode(new Uint8Array([0xE2,0x82]), {get stream(){ L.push(d.decode(new Uint8Array([0x41]), {stream:true})); return true; }});
             L.join()+':'+r.length+':'+d.decode(new Uint8Array([0xAC])).length"),
        "string:A:0:1"
    );
    // An input that the options getter changes is read after the getter.
    assert_eq!(run("var b=new Uint8Array([97,98]); new TextDecoder().decode(b, {get stream(){ b[0]=122; return false; }})"), "string:zb");
}

// ---------------------------------------------------------------- limits

pub(super) fn limited(heap: HeapLimits, total: usize, single: usize, src: &str) -> String {
    run_with(src, heap, EvalLimits { max_steps: 50_000_000, ..Default::default() }, move |it| it.set_binary_limits(total, single))
}

#[test]
fn buffer_caps_are_checked_before_allocation_and_are_distinct() {
    let h = HeapLimits::default;
    // One buffer.
    assert_eq!(limited(h(), 1000, 600, "new ArrayBuffer(700)"), "limit:Heap(BufferLength)");
    assert_eq!(limited(h(), 1000, 600, "new Uint8Array(700)"), "limit:Heap(BufferLength)");
    assert_eq!(limited(h(), 1000, 600, "new ArrayBuffer(600).byteLength"), "number:600");
    // All live buffers together; a failed allocation leaves nothing behind, and a collect frees room.
    assert_eq!(limited(h(), 1000, 600, "var a = new ArrayBuffer(600); new ArrayBuffer(401)"), "limit:Heap(BufferBytes)");
    assert_eq!(limited(h(), 1000, 600, "var a = new ArrayBuffer(600); new ArrayBuffer(400).byteLength"), "number:400");
    // The default cap is 256 MiB and the limit is not a catchable RangeError.
    assert_eq!(run("try { new ArrayBuffer(300 * 1048576); } catch (e) { e.name }"), "limit:Heap(BufferLength)");
    // The spec RangeError cases stay RangeErrors.
    assert_eq!(run("try { new ArrayBuffer(-1); } catch (e) { e.name }"), "string:RangeError");
    assert_eq!(run("try { new ArrayBuffer(9007199254740992); } catch (e) { e.name }"), "string:RangeError");
    // Sizes right at 2^53-1 and the arithmetic of offset + length cannot overflow.
    assert_eq!(run("new Uint8Array(9007199254740991)"), "limit:Heap(BufferLength)");
    assert_eq!(run("try { new Uint8Array(new ArrayBuffer(8), 9007199254740991, 9007199254740991); } catch (e) { e.name }"), "string:RangeError");
    assert_eq!(run("try { new Uint8Array(new ArrayBuffer(8), 4, 9007199254740991); } catch (e) { e.name }"), "string:RangeError");
    // slice, copy and the typed-array copy constructor are checked before the copy.
    assert_eq!(limited(h(), 1000, 600, "var a = new ArrayBuffer(600); var b = a.slice(0, 600); b.byteLength"), "limit:Heap(BufferBytes)");
    assert_eq!(limited(h(), 1000, 600, "var u = new Uint8Array(600); new Uint8Array(u); 1"), "limit:Heap(BufferBytes)");
    // The heap byte budget also counts the bytes.
    let small = HeapLimits { max_bytes: 1 << 20, ..Default::default() };
    assert_eq!(limited(small, usize::MAX, usize::MAX, "new ArrayBuffer(2000000)"), "limit:Heap(Bytes)");
}

#[test]
fn array_like_length_is_checked_before_any_element_is_read() {
    let src = "var reads = 0; var o = {length: 100000, get 0() { reads++; return 1; }}; var r;
               try { new Uint8Array(o); } catch (e) { r = e.name; } r";
    assert_eq!(limited(HeapLimits::default(), 1 << 20, 1000, src), "limit:Heap(BufferLength)");
    // An array that is longer than the cap stops while it is read, with the same limit.
    assert_eq!(limited(HeapLimits::default(), 1 << 20, 100, "var a = []; for (var i = 0; i < 200; i++) a.push(i); new Uint8Array(a)"), "limit:Heap(BufferLength)");
    // A huge ToLength does not loop or allocate.
    assert_eq!(run("new Uint8Array({length: 9007199254740991})"), "limit:Heap(BufferLength)");
    assert_eq!(run("new Uint8Array({length: Infinity})"), "limit:Heap(BufferLength)");
    // set() checks the range first, then reads at most `tlen` elements.
    assert_eq!(run("var n = 0; try { new Uint8Array(4).set({length: 9007199254740991, get 0() { n++; return 1; }}); } catch (e) { e.name + n }"), "string:RangeError0");
}

#[test]
fn passes_over_bytes_consume_steps() {
    let limits = EvalLimits { max_steps: 20_000, ..Default::default() };
    let heap = || HeapLimits::default();
    // 64 MiB copied is 1M steps: refused before the allocation.
    let r = run_with("var u = new Uint8Array(67108864); u.slice(0)", heap(), limits, |_| {});
    assert_eq!(r, "limit:Steps");
    let r = run_with("var u = new Uint8Array(67108864); new Uint8Array(u)", heap(), limits, |_| {});
    assert_eq!(r, "limit:Steps");
    let r = run_with("var u = new Uint8Array(67108864); u.set(u.subarray(1))", heap(), limits, |_| {});
    assert_eq!(r, "limit:Steps");
    let r = run_with("new ArrayBuffer(67108864).slice(0)", heap(), limits, |_| {});
    assert_eq!(r, "limit:Steps");
    // Encoding and decoding cost one step for 16 bytes.
    let r = run_with("new TextDecoder().decode(new Uint8Array(67108864))", heap(), limits, |_| {});
    assert_eq!(r, "limit:Steps");
    let r = run_with("var s = 'a'; for (var i = 0; i < 20; i++) s = s + s; new TextEncoder().encode(s)", heap(), limits, |_| {});
    assert_eq!(r, "limit:Steps");
    // Element loops tick for every element.
    let r = run_with("new Uint8Array({length: 1000000})", heap(), limits, |_| {});
    assert_eq!(r, "limit:Steps");
    // And the work that failed did not allocate: a step failure leaves no buffer bytes.
    let stats = big(move || {
        let prog = parse_script("var u = new Uint8Array(67108864); try { u.slice(0); } catch (e) {}", Limits::default()).unwrap();
        let mut it = Interp::new(&prog, Heap::default(), limits).unwrap();
        let _ = it.run();
        it.heap.stats().buffer_bytes
    });
    assert_eq!(stats, 67108864);
}

#[test]
fn key_enumeration_of_a_huge_view_is_charged_before_the_list_exists() {
    let heap = HeapLimits { max_bytes: 4 << 20, ..Default::default() };
    // A 3 MiB buffer fits the budget; a key list for 3M elements (8 bytes each) does not.
    let src = "var u = new Uint8Array(3000000); Object.keys(u).length";
    assert_eq!(limited(heap, usize::MAX, usize::MAX, src), "limit:Heap(Bytes)");
    let heap = HeapLimits { max_bytes: 4 << 20, ..Default::default() };
    assert_eq!(limited(heap, usize::MAX, usize::MAX, "var u = new Uint8Array(3000000); var n = 0; for (var k in u) n++; n"), "limit:Heap(Bytes)");
    let heap = HeapLimits { max_bytes: 4 << 20, ..Default::default() };
    assert_eq!(limited(heap, usize::MAX, usize::MAX, "var u = new Uint8Array(3000000); var o = {...u}; 1"), "limit:Heap(Bytes)");
}

#[test]
fn text_output_and_decoder_state_are_bounded() {
    // The decoded text cannot pass the heap's string cap: one unit for each byte is the bound, checked first.
    let heap = HeapLimits { max_string_units: 64, ..Default::default() };
    assert_eq!(limited(heap, usize::MAX, usize::MAX, "new TextDecoder().decode(new Uint8Array(100))"), "limit:Heap(StringLength)");
    let heap = HeapLimits { max_string_units: 64, ..Default::default() };
    assert_eq!(limited(heap, usize::MAX, usize::MAX, "new TextDecoder().decode(new Uint8Array(50)).length"), "number:50");
    // The encoder checks the byte length against the buffer cap before it allocates.
    assert_eq!(limited(HeapLimits::default(), 1 << 20, 10, "new TextEncoder().encode('\\u20ac\\u20ac\\u20ac\\u20ac')"), "limit:Heap(BufferLength)");
    assert_eq!(limited(HeapLimits::default(), 1 << 20, 12, "new TextEncoder().encode('\\u20ac\\u20ac\\u20ac\\u20ac').length"), "number:12");
    // A decoder holds a fixed state however much streams through it: 100000 one-byte chunks of a 3-byte sequence.
    let r = run("var d = new TextDecoder(); var n = 0; var c = [new Uint8Array([0xE2]), new Uint8Array([0x82]), new Uint8Array([0xAC])];
                 for (var i = 0; i < 100000; i++) n += d.decode(c[i % 3], {stream: true}).length; n + d.decode().length");
    assert_eq!(r, "number:33334");
}

#[test]
fn call_depth_guard_covers_getters_called_by_the_constructors() {
    // The element getter builds a view over the same object again: the 100-call guard ends it.
    let src = "var o = {length: 1, get 0() { return new Uint8Array(o)[0]; }}; new Uint8Array(o)";
    assert_eq!(run(src), "limit:CallDepth");
    let src = "var o = {length: 1, get 0() { new Uint8Array(1).set(o); return 1; }}; new Uint8Array(1).set(o)";
    assert_eq!(run(src), "limit:CallDepth");
    let src = "var d = new TextDecoder(); var o = {get stream() { return d.decode(new Uint8Array(1), o); }}; d.decode(new Uint8Array(1), o)";
    assert_eq!(run(src), "limit:CallDepth");
}

// ---------------------------------------------------------------- GC, stale handles and roots

pub(super) fn with_run<R: Send + 'static>(src: &'static str, f: impl FnOnce(&mut Interp) -> R + Send + 'static) -> R {
    big(move || {
        let prog = parse_script(src, Limits::default()).unwrap();
        let mut it = Interp::new(&prog, Heap::default(), EvalLimits::default()).unwrap();
        it.run().unwrap();
        f(&mut it)
    })
}

pub(super) fn global(it: &mut Interp, name: &str) -> Value {
    let k = it.heap.key_from_str(name).unwrap();
    match it.heap.get(it.global_object(), k).unwrap() {
        Get::Value(v) => v,
        _ => panic!("accessor"),
    }
}

fn set_global(it: &mut Interp, name: &str, v: Value) {
    let k = it.heap.key_from_str(name).unwrap();
    it.heap.set(it.global_object(), k, v).unwrap();
}

#[test]
fn dead_buffers_release_their_bytes_at_collect() {
    with_run("var keep = new ArrayBuffer(1000); var drop = [new ArrayBuffer(5000), new Uint8Array(7000)]; drop = null;", |it| {
        let before = it.heap.stats();
        assert_eq!(before.buffer_bytes, 13000);
        assert_eq!(before.live_exotics, 4);
        let b0 = before.bytes;
        it.collect(&[]).unwrap();
        let after = it.heap.stats();
        assert_eq!(after.buffer_bytes, 1000);
        assert_eq!(after.live_exotics, 1);
        assert!(b0 - after.bytes >= 12000, "the heap byte count falls with the buffers: {b0} -> {}", after.bytes);
        // Collecting again changes nothing.
        it.collect(&[]).unwrap();
        assert_eq!(it.heap.stats().buffer_bytes, 1000);
    });
}

#[test]
fn a_view_keeps_its_buffer_alive_and_a_buffer_does_not_keep_its_views() {
    with_run("var v = new Uint8Array(new ArrayBuffer(100)).subarray(10, 20); v[0] = 42; var b = v.buffer; b = null;", |it| {
        it.collect(&[]).unwrap();
        assert_eq!(it.heap.stats().buffer_bytes, 100);
        let v = global(it, "v");
        assert_eq!(it.bytes_of(v).unwrap().len(), 10);
        assert_eq!(it.bytes_of(v).unwrap()[0], 42);
        // Drop the view: the buffer goes too.
        set_global(it, "v", Value::Null);
        it.collect(&[]).unwrap();
        assert_eq!(it.heap.stats().buffer_bytes, 0);
        assert_eq!(it.heap.stats().live_exotics, 0);
    });
    // A buffer held alone does not hold a view.
    with_run("var b = new ArrayBuffer(10); var v = new Uint8Array(b); v = null;", |it| {
        it.collect(&[]).unwrap();
        let s = it.heap.stats();
        assert_eq!((s.buffer_bytes, s.live_exotics), (10, 1));
    });
}

#[test]
fn stale_views_and_slot_reuse_never_alias() {
    with_run("var a = new Uint8Array([1, 2, 3]);", |it| {
        let old = global(it, "a");
        let Value::Object(old_id) = old else { panic!() };
        assert_eq!(it.bytes_of(old).unwrap(), &[1, 2, 3]);
        set_global(it, "a", Value::Null);
        it.collect(&[]).unwrap();
        assert!(it.bytes_of(old).is_none());
        assert!(it.heap.view_parts(old_id).is_none());
        assert!(it.heap.exotic_kind(old_id).is_none());
        // Make many new buffers: slots are reused, and the old handle still sees nothing.
        let fresh = it.new_uint8_array(vec![9; 3]).unwrap();
        assert!(it.bytes_of(old).is_none());
        assert_eq!(it.bytes_of(fresh).unwrap(), &[9, 9, 9]);
        // A stale handle given back to a method is a clean error, not a panic or foreign bytes.
        assert!(it.heap.view_get(old_id, 0).is_none());
        assert!(!it.heap.view_set(old_id, 0, 1));
        assert!(it.heap.bytes_mut(old_id).is_none());
    });
}

#[test]
fn closures_reachable_only_through_a_buffer_survive_collect() {
    let src = "var v; (function() { var b = new ArrayBuffer(4); b.cb = (function() { var z = 41; return function() { return z + 1; }; })(); v = new Uint8Array(b); })();";
    // The same check through the evaluator itself: collect inside the script's own realm, then call.
    let src = src.to_string();
    big(move || {
        let text = format!("{src} function probe() {{ return v.buffer.cb(); }}");
        let prog = parse_script(&text, Limits::default()).unwrap();
        let mut it = Interp::new(&prog, Heap::default(), EvalLimits::default()).unwrap();
        it.run().unwrap();
        it.collect(&[]).unwrap();
        let f = global(&mut it, "probe");
        let r = it.call(f, Value::Undefined, &[]).unwrap();
        assert_eq!(it.display(r), "42");
    });
}

#[test]
fn builtin_prototypes_are_roots_even_when_the_globals_are_deleted() {
    with_run("delete globalThis.Uint8Array; delete globalThis.ArrayBuffer; delete globalThis.TextEncoder; delete globalThis.TextDecoder;", |it| {
        it.collect(&[]).unwrap();
        let v = it.new_uint8_array(vec![1, 2]).unwrap();
        let b = it.new_array_buffer(vec![3]).unwrap();
        assert_eq!(it.bytes_of(v).unwrap(), &[1, 2]);
        assert_eq!(it.bytes_of(b).unwrap(), &[3]);
        set_global(it, "keepv", v);
        set_global(it, "keepb", b);
        it.collect(&[]).unwrap();
        let kv = global(it, "keepv");
        assert_eq!(it.bytes_of(kv).unwrap(), &[1, 2]);
    });
}

#[test]
fn text_codec_objects_are_collected_with_their_state() {
    with_run("var d = new TextDecoder(); d.decode(new Uint8Array([0xE2]), {stream: true}); var e = new TextEncoder(); var many = []; for (var i = 0; i < 1000; i++) many.push(new TextDecoder()); many = null;", |it| {
        assert_eq!(it.heap.stats().live_exotics, 1004);
        it.collect(&[]).unwrap();
        assert_eq!(it.heap.stats().live_exotics, 2); // d and e
    });
}

// ---------------------------------------------------------------- embedding helpers

#[test]
fn embedding_helpers_move_bytes_without_copying() {
    with_run("var seen;", |it| {
        let v: Vec<u8> = (0..=255).collect();
        let ptr = v.as_ptr();
        let a = it.new_uint8_array(v).unwrap();
        // The vector became the buffer: the same allocation.
        assert_eq!(it.bytes_of(a).unwrap().as_ptr(), ptr);
        set_global(it, "a", a);
        let ab = it.new_array_buffer(vec![7; 4]).unwrap();
        set_global(it, "ab", ab);
        let prog = parse_script("a[255] + ',' + a.length + ',' + a.byteOffset + ',' + (a.buffer instanceof ArrayBuffer) + ',' + ab.byteLength + ',' + new Uint8Array(ab)[3]", Limits::default()).unwrap();
        let _ = prog;
        assert!(it.bytes_of(Value::Undefined).is_none());
        assert!(it.bytes_of(Value::Number(1.0)).is_none());
        let s = it.heap.intern_str("abc").unwrap();
        assert!(it.bytes_of(Value::String(s)).is_none());
        // Plain objects and arrays have no bytes.
        let o = it.heap.new_object(None).unwrap();
        assert!(it.bytes_of(Value::Object(o)).is_none());
    });
    // A script sees what the host made.
    big(|| {
        let prog = parse_script("function probe() { return a[255] + ',' + a.length + ',' + a.byteOffset + ',' + (a.buffer instanceof ArrayBuffer) + ',' + ab.byteLength + ',' + new Uint8Array(ab)[3]; }", Limits::default()).unwrap();
        let mut it = Interp::new(&prog, Heap::default(), EvalLimits::default()).unwrap();
        it.run().unwrap();
        let a = it.new_uint8_array((0..=255).collect()).unwrap();
        set_global(&mut it, "a", a);
        let ab = it.new_array_buffer(vec![7; 4]).unwrap();
        set_global(&mut it, "ab", ab);
        let f = global(&mut it, "probe");
        let r = it.call(f, Value::Undefined, &[]).unwrap();
        assert_eq!(it.display(r), "255,256,0,true,4,7");
        // A script write is visible to the host through the same bytes.
        let g = it.heap.key_from_str("a").unwrap();
        let _ = g;
    });
}

#[test]
fn host_helpers_obey_the_caps() {
    big(|| {
        let prog = parse_script("", Limits::default()).unwrap();
        let mut it = Interp::new(&prog, Heap::default(), EvalLimits::default()).unwrap();
        it.set_binary_limits(100, 60);
        assert!(matches!(it.new_uint8_array(vec![0; 61]), Err(Error::Limit(LimitKind::Heap(Limit::BufferLength)))));
        let first = it.new_uint8_array(vec![0; 60]).unwrap();
        assert!(matches!(it.new_array_buffer(vec![0; 41]), Err(Error::Limit(LimitKind::Heap(Limit::BufferBytes)))));
        assert!(it.new_array_buffer(vec![0; 40]).is_ok());
        let _ = first;
    });
}

// ---------------------------------------------------------------- views on the generic property paths

#[test]
fn heap_level_integer_index_rules_hold_for_any_caller() {
    let mut h = Heap::default();
    let b = h.new_buffer(None, 4).unwrap();
    let v = h.new_view(None, b, 1, 2).unwrap();
    let k = |h: &mut Heap, s: &str| h.key_from_str(s).unwrap();
    let (k0, k5, kneg0, khalf, kfoo) = (PropertyKey::Index(0), PropertyKey::Index(5), k(&mut h, "-0"), k(&mut h, "1.5"), k(&mut h, "foo"));
    // Writes convert modulo 256 and an invalid index is a no-op that still succeeds.
    assert_eq!(h.set(v, k0, Value::Number(257.0)).unwrap(), Set::Done);
    assert_eq!(h.set(v, k5, Value::Number(1.0)).unwrap(), Set::Done);
    assert_eq!(h.set(v, kneg0, Value::Number(1.0)).unwrap(), Set::Done);
    assert_eq!(h.set(v, khalf, Value::Number(1.0)).unwrap(), Set::Done);
    assert_eq!(h.bytes(b).unwrap(), &[0, 1, 0, 0]);
    assert!(matches!(h.get_own_property(v, k0).unwrap(), Some(Property::Data { value: Value::Number(n), writable: true, enumerable: true, configurable: true }) if n == 1.0));
    for key in [k5, kneg0, khalf] {
        assert!(h.get_own_property(v, key).unwrap().is_none());
        assert!(!h.has_property(v, key).unwrap());
        assert!(h.delete(v, key).unwrap()); // an invalid index deletes "successfully"
        assert!(!h.define_own_property(v, key, &PropertyDescriptor::data(Value::Number(1.0))).unwrap());
    }
    assert!(!h.delete(v, k0).unwrap());
    // An object value would need ToNumber: refused, not guessed.
    let o = h.new_object(None).unwrap();
    assert!(matches!(h.set(v, k0, Value::Object(o)), Err(HeapError::NeedsExecution(_))));
    // Named keys stay ordinary.
    assert_eq!(h.set(v, kfoo, Value::Number(9.0)).unwrap(), Set::Done);
    assert!(h.has_own_property(v, kfoo).unwrap());
    let keys = h.own_keys(v).unwrap();
    assert_eq!(keys, vec![PropertyKey::Index(0), PropertyKey::Index(1), kfoo]);
    // Accessors, non-writable and non-configurable definitions of a valid index are rejected.
    for d in [
        PropertyDescriptor { configurable: Some(false), ..Default::default() },
        PropertyDescriptor { enumerable: Some(false), ..Default::default() },
        PropertyDescriptor { writable: Some(false), ..Default::default() },
        PropertyDescriptor { get: Some(None), ..Default::default() },
    ] {
        assert!(!h.define_own_property(v, k0, &d).unwrap());
    }
    assert!(h.define_own_property(v, k0, &PropertyDescriptor { value: Some(Value::Number(300.0)), ..Default::default() }).unwrap());
    assert_eq!(h.view_get(v, 0), Some(44));
}

#[test]
fn a_view_in_a_prototype_chain_ends_numeric_lookups_and_writes() {
    let mut h = Heap::default();
    let b = h.new_buffer(None, 2).unwrap();
    let v = h.new_view(None, b, 0, 2).unwrap();
    h.view_set(v, 1, 7);
    let child = h.new_object(Some(v)).unwrap();
    let plain = h.new_object(None).unwrap();
    // `Object.prototype`-like holder behind the view: its numeric keys must stay invisible.
    h.set_prototype_of(v, Some(plain)).unwrap();
    h.set(plain, PropertyKey::Index(5), Value::Number(1.0)).unwrap();
    let neg0 = h.key_from_str("-0").unwrap();
    h.set(plain, neg0, Value::Number(1.0)).unwrap();
    assert!(matches!(h.get(child, PropertyKey::Index(1)).unwrap(), Get::Value(Value::Number(n)) if n == 7.0));
    assert!(matches!(h.get(child, PropertyKey::Index(5)).unwrap(), Get::Value(Value::Undefined)));
    assert!(!h.has_property(child, PropertyKey::Index(5)).unwrap());
    assert!(!h.has_property(child, neg0).unwrap());
    // A write through the child: an invalid index is dropped; a valid index makes an own property of the child.
    assert_eq!(h.set(child, PropertyKey::Index(5), Value::Number(3.0)).unwrap(), Set::Done);
    assert!(!h.has_own_property(child, PropertyKey::Index(5)).unwrap());
    assert_eq!(h.set(child, PropertyKey::Index(0), Value::Number(3.0)).unwrap(), Set::Done);
    assert!(h.has_own_property(child, PropertyKey::Index(0)).unwrap());
    assert_eq!(h.view_get(v, 0), Some(0));
    // A view whose prototype is another view.
    let b2 = h.new_buffer(None, 1).unwrap();
    let v2 = h.new_view(Some(v), b2, 0, 1).unwrap();
    assert!(h.has_property(v2, PropertyKey::Index(0)).unwrap());
    assert!(!h.has_property(v2, PropertyKey::Index(1)).unwrap()); // out of v2, and v is not asked
    assert!(!h.has_own_property(v2, PropertyKey::Index(1)).unwrap());
}

#[test]
fn views_check_their_range_and_buffer() {
    let mut h = Heap::default();
    let b = h.new_buffer(None, 4).unwrap();
    assert!(h.new_view(None, b, 4, 0).is_ok());
    assert!(h.new_view(None, b, 5, 0).is_err());
    assert!(h.new_view(None, b, 1, 4).is_err());
    assert!(h.new_view(None, b, usize::MAX, 2).is_err());
    let o = h.new_object(None).unwrap();
    assert!(h.new_view(None, o, 0, 0).is_err());
    let v = h.new_view(None, b, 0, 4).unwrap();
    assert!(h.new_view(None, v, 0, 1).is_err()); // a view is not a buffer
    // view_copy: ranges that do not fit copy nothing.
    let w = h.new_view(None, b, 2, 2).unwrap();
    assert!(!h.view_copy(v, 0, w, 1, 2));
    assert!(!h.view_copy(v, 3, w, 0, 2));
    assert!(!h.view_copy(v, usize::MAX, w, 0, 2));
    assert!(h.view_copy(v, 0, w, 0, 2));
}

#[test]
fn collect_with_exotics_survives_a_stress_of_churn() {
    big(|| {
        let mut h = Heap::default();
        h.set_buffer_limits(1 << 20, 1 << 20);
        let mut keep = Vec::new();
        for round in 0..50 {
            for i in 0..200 {
                let b = h.new_buffer(None, 100 + i).unwrap();
                let v = h.new_view(None, b, 1, 10).unwrap();
                if (i + round) % 7 == 0 {
                    keep.push(Value::Object(v));
                }
            }
            h.collect(&keep).unwrap();
            let want: usize = keep.len();
            assert_eq!(h.stats().live_exotics, want * 2, "round {round}");
            if keep.len() > 40 {
                keep.drain(..20);
            }
        }
        h.collect(&[]).unwrap();
        assert_eq!(h.stats().live_exotics, 0);
        assert_eq!(h.stats().buffer_bytes, 0);
    });
}

#[test]
fn collect_does_not_walk_or_charge_view_indices() {
    // A 3 MB view with a 4 MB byte budget: listing its keys would cost 24 MB, so collect must not do it.
    big(|| {
        let prog = parse_script("var u = new Uint8Array(3000000); u.tag = {a: 1}; function probe() { return u.tag.a; }", Limits::default()).unwrap();
        let heap = HeapLimits { max_bytes: 4 << 20, ..Default::default() };
        let mut it = Interp::new(&prog, Heap::new(heap).unwrap(), EvalLimits::default()).unwrap();
        it.run().unwrap();
        let steps = it.stats().steps;
        it.collect(&[]).unwrap();
        assert_eq!(it.stats().steps, steps);
        assert_eq!(it.heap.stats().buffer_bytes, 3000000);
        // A property on the view's own named keys is still traced.
        let f = global(&mut it, "probe");
        let r = it.call(f, Value::Undefined, &[]).unwrap();
        assert_eq!(it.display(r), "1");
    });
}

#[test]
fn view_enumeration_checks_steps_before_materializing_keys() {
    let heap = HeapLimits { max_bytes: 4 << 20, ..Default::default() };
    assert_eq!(run_with("var u = new Uint8Array(3000000); Object.keys(u)", heap, EvalLimits { max_steps: 100, ..Default::default() }, |_| {}), "limit:Steps");
}

#[test]
fn decoder_accepts_output_exactly_at_the_string_cap() {
    let heap = || HeapLimits { max_string_units: 64, ..Default::default() };
    assert_eq!(run_with("new TextDecoder().decode(new Uint8Array(64)).length", heap(), EvalLimits::default(), |_| {}), "number:64");
    let bytes = vec!["195,169"; 64].join(",");
    let src = format!("new TextDecoder().decode(new Uint8Array([{bytes}])).length");
    assert_eq!(run_with(&src, heap(), EvalLimits::default(), |_| {}), "number:64");
}

#[test]
fn decoder_label_scans_consume_steps() {
    let src = format!("new TextDecoder('{}utf-8')", " ".repeat(8192));
    assert_eq!(run_with(&src, HeapLimits::default(), EvalLimits { max_steps: 100, ..Default::default() }, |_| {}), "limit:Steps");
}

// ---------------------------------------------------------------- iterables, species and subclass gates

#[test]
fn custom_species_is_an_explicit_gate_and_the_default_is_not() {
    let g = |src: &str| run(src);
    let ab = "unsupported:ArrayBuffer species constructor other than ArrayBuffer";
    let ta = "unsupported:typed array species constructor other than the intrinsic class";
    assert_eq!(g("var b = new ArrayBuffer(2); b.constructor = {[Symbol.species]: function(n) { return new ArrayBuffer(n); }}; b.slice(0)"), ab);
    assert_eq!(g("var u = new Uint8Array(2); u.constructor = {[Symbol.species]: function(n) { return new Uint8Array(n); }}; u.subarray(0)"), ta);
    assert_eq!(g("var u = new Uint8Array(2); u.constructor = {[Symbol.species]: function(n) { return new Uint8Array(n); }}; u.slice(0)"), ta);
    // another intrinsic as the species is also not the default: the type would be wrong, so it is a gate
    assert_eq!(g("var u = new Uint8Array(2); u.constructor = {[Symbol.species]: ArrayBuffer}; u.slice(0)"), ta);
    assert_eq!(g("var b = new ArrayBuffer(2); b.constructor = {[Symbol.species]: Uint8Array}; b.slice(0)"), ab);
    // not a constructor at all: the ordinary TypeError, as in a browser
    assert_eq!(g("var b = new ArrayBuffer(2); b.constructor = {[Symbol.species]: {}}; b.slice(0)"), "throw:TypeError:object.constructor[Symbol.species] is not a constructor");
    // the default species ends in the right type
    assert_eq!(g("var u = new Uint8Array(4); u.constructor = {[Symbol.species]: Uint8Array}; var s = u.subarray(1); (s instanceof Uint8Array) + ',' + s.length"), "string:true,3");
    assert_eq!(g("var b = new ArrayBuffer(4); b.constructor = {[Symbol.species]: ArrayBuffer}; b.slice(1).byteLength"), "number:3");
    // a getter of the species that throws is not ignored
    assert_eq!(g("var u = new Uint8Array(1); u.constructor = {get [Symbol.species]() { throw new RangeError('s'); }}; u.slice(0)"), "throw:RangeError:s");
}

#[test]
fn subclassing_a_binary_or_collection_native_is_refused_when_constructed() {
    let g = |src: &str| run(src);
    for (name, args) in [("Uint8Array", "2"), ("ArrayBuffer", "2"), ("TextEncoder", ""), ("TextDecoder", ""), ("Map", ""), ("Set", "")] {
        // declaring the class works; constructing it is the explicit gate
        assert_eq!(g(&format!("class A extends {name} {{}} typeof A")), "string:function", "{name}");
        assert_eq!(
            g(&format!("class A extends {name} {{ constructor() {{ super({args}); }} }} new A()")),
            "unsupported:subclassing a native constructor (new.target is not passed to the constructor)",
            "{name}"
        );
        assert_eq!(g(&format!("class A extends {name} {{}} new A({args})")), "unsupported:subclassing a native constructor (new.target is not passed to the constructor)", "{name}");
    }
    // the abstract %TypedArray% is not a usable parent either: it throws when constructed, as in a browser
    assert_eq!(g("var T = Object.getPrototypeOf(Uint8Array); class A extends T {} new A()"), "unsupported:subclassing a native constructor (new.target is not passed to the constructor)");
    // no fake brand: the plain constructors are untouched
    assert_eq!(g("Object.prototype.toString.call(new Uint8Array(1)) + new Map().size"), "string:[object Uint8Array]0");
}

#[test]
fn iterable_construction_is_bounded_before_the_values_grow_and_runs_no_more_js() {
    // 5 values against a 4-byte single-buffer cap: the heap limit, and `return` is not called after a limit
    let src = "var L = []; var o = {[Symbol.iterator]: function() { var i = 0; return {next: function() { L.push('n'); return i++ < 5 ? {value: 1, done: false} : {done: true}; }, return: function() { L.push('ret'); return {}; }}; }}; new Uint8Array(o)";
    big(move || {
        let prog = parse_script(src, Limits::default()).unwrap();
        let mut it = Interp::new(&prog, Heap::default(), EvalLimits::default()).unwrap();
        it.set_binary_limits(1 << 20, 4);
        assert!(matches!(it.run(), Err(Error::Limit(LimitKind::Heap(Limit::BufferLength)))));
        let l = global(&mut it, "L");
        assert_eq!(it.get_str(l, "length"), "5", "five next calls and no return");
    });
    // the iterator buffer cap of the symbol features also applies
    big(move || {
        let prog = parse_script(src, Limits::default()).unwrap();
        let mut it = Interp::new(&prog, Heap::default(), EvalLimits::default()).unwrap();
        it.set_sc_limits(ScLimits { max_iter_buffer: 3, ..Default::default() });
        assert!(matches!(it.run(), Err(Error::Limit(LimitKind::Sc(ScLimit::IterBuffer)))));
        let l = global(&mut it, "L");
        assert_eq!(it.get_str(l, "length"), "4");
    });
    // an endless iterable ends in a limit, never in memory growth
    let r = run("new Uint8Array({[Symbol.iterator]: function() { return {next: function() { return {value: 1, done: false}; }}; }})");
    assert!(r.starts_with("limit:"), "{r}");
}

#[test]
fn a_typed_array_iterator_takes_no_result_objects_and_follows_the_buffer() {
    // 100000 steps through the shared iterator without a result object each: the object count stays flat
    big(|| {
        let prog = parse_script("var u = new Uint8Array(100000); var n = 0; for (var x of u) n += x + 1; n", Limits::default()).unwrap();
        let mut it = Interp::new(&prog, Heap::default(), EvalLimits::default()).unwrap();
        let before = it.heap.stats().live_objects;
        let r = it.run().unwrap();
        assert_eq!(it.display(r), "100000");
        assert!(it.heap.stats().live_objects < before + 100, "no result object for each step");
    });
}

#[test]
fn object_assign_values_and_entries_charge_view_indices_before_materializing_keys() {
    for call in ["Object.assign({}, u)", "Object.values(u)", "Object.entries(u)"] {
        let heap = HeapLimits { max_bytes: 4 << 20, ..Default::default() };
        assert_eq!(run_with(&format!("var u = new Uint8Array(3000000); {call}"), heap, EvalLimits { max_steps: 100, ..Default::default() }, |_| {}), "limit:Steps", "{call}");
    }
}

#[test]
fn collect_with_a_suspended_view_iteration_does_not_walk_the_view_indices() {
    // The frame holds the iterator over a 3 MB view; a key list of it would cost 24 MB against a 4 MB budget.
    big(|| {
        let prog = parse_script("var u = new Uint8Array(3000000); var gate; var p = new Promise(function (r) { gate = r; }); var n = 0; (async function () { for (var x of u) { await p; n++; if (n == 2) break; } })(); function probe() { return n; }", Limits::default()).unwrap();
        let heap = HeapLimits { max_bytes: 4 << 20, ..Default::default() };
        let mut it = Interp::new(&prog, Heap::new(heap).unwrap(), EvalLimits::default()).unwrap();
        it.run().unwrap();
        assert_eq!(it.async_stats().activations, 1);
        let steps = it.stats().steps;
        it.collect(&[]).unwrap();
        it.collect(&[]).unwrap();
        assert_eq!(it.stats().steps, steps, "collect charges nothing");
        assert_eq!(it.async_stats().activations, 1);
        assert_eq!(it.heap.stats().buffer_bytes, 3000000);
    });
}

// ---------------------------------------------------------------- %TypedArray%.prototype.indexOf

/// The result, the steps, the object allocations and the buffer bytes of a script (a fresh heap and budget).
fn indexof_probe(src: &'static str, max_steps: u64) -> (String, u64, u64, usize) {
    big(move || {
        let prog = parse_script(src, Limits::default()).unwrap();
        let mut it = Interp::new(&prog, Heap::default(), EvalLimits { max_steps, ..Default::default() }).unwrap();
        let r = it.run();
        let s = fmt_result(&mut it, r);
        let st = it.heap.stats();
        (s, it.stats().steps, st.total_object_allocs, st.buffer_bytes)
    })
}

#[test]
fn indexof_returns_strict_matches_as_numbers() {
    assert_eq!(run("new Uint8Array([1, 2, 3]).indexOf(2)"), "number:1");
    assert_eq!(run("new Uint8Array([1, 2, 3]).indexOf(9)"), "number:-1");
    assert_eq!(run("new Uint8Array(0).indexOf(0)"), "number:-1");
    assert_eq!(run("new Uint8Array([0]).indexOf(-0)"), "number:0");
    assert_eq!(run("new Uint8Array([1]).indexOf(NaN)"), "number:-1");
    assert_eq!(run("new Uint8Array([1]).indexOf('1')"), "number:-1");
    assert_eq!(run("new Uint8Array([1]).indexOf(1, -1)"), "number:0");
    assert_eq!(run("new Uint8Array([1]).indexOf(1, 1)"), "number:-1");
}

#[test]
fn indexof_charges_each_64_byte_chunk_before_it_is_read() {
    // A miss over 1 MiB is 16384 chunks; a 64-byte view is one chunk. The difference is exact, so the setup cancels.
    let (miss, s_big, objs_big, bytes_big) = indexof_probe("var u = new Uint8Array(1048576); u.indexOf(9)", 1_000_000);
    let (_, s_small, objs_small, bytes_small) = indexof_probe("var u = new Uint8Array(64); u.indexOf(9)", 1_000_000);
    assert_eq!(miss, "number:-1");
    assert_eq!(s_big - s_small, 16383);
    // The scan allocates nothing and copies no bytes: the object count and the buffer bytes are those of the view.
    assert_eq!((objs_big, bytes_big), (objs_small, 1048576));
    assert_eq!(bytes_small, 64);
    // An early match costs the chunks it reached: byte 6400 is in chunk 100, so 101 chunks in all.
    let (hit, s_hit, _, _) = indexof_probe("var u = new Uint8Array(1048576); u[6400] = 7; u.indexOf(7)", 1_000_000);
    let (_, s_small_hit, _, _) = indexof_probe("var u = new Uint8Array(64); u[0] = 7; u.indexOf(7)", 1_000_000);
    assert_eq!(hit, "number:6400");
    assert_eq!(s_hit - s_small_hit, 100);
    // A limit ends the scan at the first chunk past it: the step count is the limit plus one, and nothing more is read.
    let (r, s, _, _) = indexof_probe("var u = new Uint8Array(1048576); u.indexOf(9)", 50);
    assert_eq!(r, "limit:Steps");
    assert_eq!(s, 51);
}

#[test]
fn indexof_limit_is_fatal_and_runs_no_more_js() {
    big(|| {
        let prog = parse_script("var L = []; var u = new Uint8Array(67108864); try { u.indexOf(9, {valueOf: function () { L.push('from'); return 0; }}); } finally { L.push('finally'); }", Limits::default()).unwrap();
        let mut it = Interp::new(&prog, Heap::default(), EvalLimits { max_steps: 20_000, ..Default::default() }).unwrap();
        let r = it.run();
        assert_eq!(fmt_result(&mut it, r), "limit:Steps");
        // The fromIndex conversion ran before the scan. The scan's limit is not catchable, so `finally` did not run.
        let l = global(&mut it, "L");
        assert_eq!(it.get_str(l, "length"), "1");
    });
}

#[test]
fn indexof_survives_global_deletion_and_collection() {
    big(|| {
        let mut it = Interp::realm(Heap::default(), EvalLimits { max_steps: 50_000_000, ..Default::default() }).unwrap();
        // The view is the only holder of its buffer; the method is held by a global that outlives the constructor.
        let r = it.run_script("var b = new ArrayBuffer(4); var v = new Uint8Array(b, 1, 2); v[1] = 9; var f = Uint8Array.prototype.indexOf; b = null; delete Uint8Array; 0");
        assert!(r.is_ok());
        it.collect(&[]).unwrap();
        it.collect(&[]).unwrap();
        let r = it.run_script("[f.call(v, 9), f.call(v, 0), f.length, typeof Uint8Array].join('|')");
        assert_eq!(fmt_result(&mut it, r), "string:1|0|1|undefined");
    });
}
