//! Tests of the numeric builtins (`eval_numeric.rs`). `numeric-cases.txt` is made by `tools/js/numeric_oracle.mjs`
//! (Node, our own list only); the rest cover what Node cannot show: GC roots, limits, injected seeds.

use super::*;
use crate::heap::{Heap, HeapLimits};
use crate::parse::{parse_script, Limits};

fn big<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(64 << 20).spawn(f).unwrap().join().unwrap()
}

fn show(it: &Interp, v: Value) -> String {
    match v {
        Value::String(_) => format!("string:{}", it.display(v)),
        Value::Number(_) => format!("number:{}", it.display(v)),
        Value::Bool(_) => format!("boolean:{}", it.display(v)),
        Value::Undefined => "undefined".into(),
        _ => it.display(v),
    }
}

fn outcome(src: &str, limits: EvalLimits, heap: Heap) -> String {
    let prog = parse_script(src, Limits::default()).unwrap_or_else(|e| panic!("{src:?}: {e}"));
    let mut it = Interp::new(&prog, heap, limits).expect("setup");
    match it.run() {
        Ok(v) => show(&it, v),
        Err(Error::Throw(v)) => format!("throw:{}", if matches!(v, Value::Object(_)) { it.get_str(v, "name") } else { show(&it, v) }),
        Err(Error::Limit(l)) => format!("limit:{l:?}"),
        Err(Error::Unsupported(u)) => format!("unsupported:{}", u.what),
        Err(e) => format!("other:{e:?}"),
    }
}

fn run(src: &str) -> String {
    let src = src.to_string();
    big(move || outcome(&src, EvalLimits { max_steps: 2_000_000, ..Default::default() }, Heap::default()))
}

#[test]
fn radix_corpus_matches_node() {
    let cases = include_str!("../../../tools/fixtures/js/numeric-cases.txt");
    big(move || {
        let mut n = 0;
        for line in cases.lines() {
            let (lit, want) = line.split_once('\t').unwrap();
            let src = format!("[2,3,7,8,16,32,36].map(function(r){{ return ({lit}).toString(r); }}).join('|')");
            let got = outcome(&src, EvalLimits { max_steps: 2_000_000, ..Default::default() }, Heap::default());
            assert_eq!(got, format!("string:{want}"), "{lit}");
            n += 1;
        }
        assert!(n > 600);
    });
}

#[test]
fn radix_edges() {
    for (src, want) in [
        ("(255).toString(16)", "string:ff"),
        ("(255).toString()", "string:255"),
        ("(255).toString(undefined)", "string:255"),
        ("(-255).toString(2)", "string:-11111111"),
        ("(-0).toString(2)", "string:0"),
        ("(NaN).toString(2)", "string:NaN"),
        ("(-Infinity).toString(36)", "string:-Infinity"),
    ] {
        assert_eq!(run(src), want, "{src}");
    }
    assert_eq!(run("(10).toString(2.9)"), "string:1010");
    assert_eq!(run("(10).toString(null)"), "throw:RangeError");
    assert_eq!(run("(10).toString(NaN)"), "throw:RangeError"); // ToIntegerOrInfinity(NaN) = 0
    for r in ["1", "37", "Infinity", "-1", "0"] {
        assert_eq!(run(&format!("(10).toString({r})")), "throw:RangeError", "{r}");
    }
    assert_eq!(run("Number.prototype.toString.call('x')"), "throw:TypeError");
    assert_eq!(run("Number.prototype.toString.call({}, 99)"), "throw:TypeError"); // receiver is checked first
    assert_eq!(run("(10).toString(Symbol())"), "throw:TypeError");
    assert_eq!(run("Number.prototype.toString.length + ':' + Number.prototype.toString.name"), "string:1:toString");
}

#[test]
fn radix_coercion_order_and_callbacks() {
    assert_eq!(run("var l=[]; try { (10).toString({valueOf(){l.push('v'); return 99}}) } catch(e) { l.push(e.name) } l.join()"), "string:v,RangeError");
    assert_eq!(run("(10).toString({valueOf(){ return (5).toString(2).length }})"), "string:101"); // reentrant, radix 3
    assert_eq!(run("var t=0; try { (10).toString({valueOf(){ throw 7 }}) } catch(e) { t=e } t"), "number:7");
    assert_eq!(run("(255).toString({toString(){ return '16' }})"), "string:ff");
}

#[test]
fn number_prototype_shape() {
    assert_eq!(run("var d=Object.getOwnPropertyDescriptor(Number,'prototype'); [d.writable,d.enumerable,d.configurable].join()"), "string:false,false,false");
    assert_eq!(run("Number.prototype.constructor === Number && Object.getPrototypeOf(5) === Number.prototype"), "boolean:true");
    assert_eq!(run("Number.prototype.hasOwnProperty('toString') && (5).toString === Number.prototype.toString"), "boolean:true");
    assert_eq!(run("Number.prototype.foo = function(){ return typeof this }; (5).foo.call(1)"), "string:number"); // primitive `this` in sloppy functions is not boxed here
    assert_eq!(run("Object.prototype.zz = 3; (5).zz"), "number:3");
}

#[test]
fn math_clz32_log_ln2() {
    for (src, want) in [
        ("Math.clz32(1)", "number:31"), ("Math.clz32(0)", "number:32"), ("Math.clz32(-0)", "number:32"), ("Math.clz32()", "number:32"),
        ("Math.clz32(NaN)", "number:32"), ("Math.clz32(Infinity)", "number:32"), ("Math.clz32(-1)", "number:0"),
        ("Math.clz32(4294967296)", "number:32"), ("Math.clz32(4294967295)", "number:0"), ("Math.clz32(2147483648)", "number:0"),
        ("Math.clz32(0.9)", "number:32"), ("Math.clz32(-0.9)", "number:32"), ("Math.clz32(4294967297.5)", "number:31"),
        ("Math.clz32(-4294967295)", "number:31"), ("Math.clz32(1e300)", "number:32"), ("Math.clz32('8')", "number:28"), ("Math.clz32({valueOf(){return 255}})", "number:24"),
        ("Math.log(1)", "number:0"), ("1/Math.log(1)", "number:Infinity"), ("Math.log(0)", "number:-Infinity"), ("Math.log(-0)", "number:-Infinity"),
        ("Math.log(-1)", "number:NaN"), ("Math.log(-Infinity)", "number:NaN"), ("Math.log(Infinity)", "number:Infinity"), ("Math.log(NaN)", "number:NaN"),
        ("Math.log()", "number:NaN"), ("Math.log(1e-320)", "number:-736.8272408909739"), ("Math.log('1')", "number:0"),
        ("Math.LN2", "number:0.6931471805599453"), ("var d=Object.getOwnPropertyDescriptor(Math,'LN2'); [d.writable,d.enumerable,d.configurable].join()", "string:false,false,false"),
        ("Math.LN2 = 1; Math.LN2", "number:0.6931471805599453"), ("Math.clz32.length + ',' + Math.log.length + ',' + Math.random.length", "string:1,1,0"),
        ("var l=[]; try { Math.clz32({valueOf(){ l.push(1); throw 5 }}) } catch(e) { l.push(e) } l.join()", "string:1,5"),
        ("Math.clz32(Symbol())", "throw:TypeError"),
    ] {
        assert_eq!(run(src), want, "{src}");
    }
}

/// Run `src` with a fixed seed.
fn seeded(src: &str, seed: [u64; 2]) -> String {
    let src = src.to_string();
    big(move || {
        let prog = parse_script(&src, Limits::default()).unwrap();
        let mut it = Interp::new(&prog, Heap::default(), EvalLimits { max_steps: 5_000_000, ..Default::default() }).unwrap();
        it.ao.rng = Some(seed);
        match it.run() {
            Ok(v) => show(&it, v),
            Err(e) => format!("{e:?}"),
        }
    })
}

#[test]
fn random_range_and_determinism() {
    let src = "var lo=1,hi=0,seen={}; for (var i=0;i<20000;i++){ var r=Math.random(); if(!(r>=0&&r<1)) throw 1; if(r<lo)lo=r; if(r>hi)hi=r; seen[r]=1 } Object.keys(seen).length + ':' + (lo<0.01) + ':' + (hi>0.99)";
    assert_eq!(seeded(src, [1, 2]), "string:20000:true:true");
    let a = seeded("Math.random()+','+Math.random()", [3, 5]);
    assert_eq!(a, seeded("Math.random()+','+Math.random()", [3, 5]));
    assert_ne!(a, seeded("Math.random()+','+Math.random()", [3, 6]));
    // The first draw of seed [1,2]: s1=1,s0=2 -> checked against an independent xorshift128+ below.
    let (mut s1, s0) = (1u64, 2u64);
    s1 ^= s1 << 23; s1 ^= s1 >> 17; s1 ^= s0; s1 ^= s0 >> 26;
    let want = (s0.wrapping_add(s1) >> 11) as f64 / 9007199254740992.0;
    assert_eq!(seeded("Math.random()", [1, 2]), format!("number:{}", number_to_string(want)));
    // Args are ignored, and a coercion-throwing argument is never touched.
    assert_eq!(seeded("Math.random({valueOf(){throw 1}}) < 1", [1, 2]), "boolean:true");
    // toString(36) of a random number is a bounded, valid text.
    assert_eq!(seeded("var ok=true; for (var i=0;i<500;i++){ var t=Math.random().toString(36); if(!/^0(\\.[0-9a-z]+)?$/.test(t)) ok=t } ok", [9, 9]), "boolean:true");
}

#[test]
fn random_seeds_from_the_os_once_and_is_per_realm() {
    assert!(os_seed().is_some());
    let a = run("Math.random()");
    let b = run("Math.random()");
    assert_ne!(a, b); // two realms, two seeds (fails with probability 2^-53)
    assert_eq!(run("var a=Math.random(), b=Math.random(); a !== b"), "boolean:true");
}

#[test]
fn number_proto_survives_gc_and_deleting_number() {
    big(|| {
        let prog = parse_script("delete globalThis.Number; (255).toString(16)", Limits::default()).unwrap();
        let mut it = Interp::new(&prog, Heap::default(), EvalLimits::default()).unwrap();
        let v = it.run().unwrap();
        assert_eq!(show(&it, v), "string:ff");
        let extra = [v];
        it.collect(&extra).unwrap();
        it.collect(&extra).unwrap();
        let np = it.ao.number_proto.unwrap();
        assert!(it.heap.get(np, it.k_constructor).is_ok());
        // A later script in the same realm still sees the prototype.
        let f = it.get_prop(Value::Number(255.0), it.k_to_string).unwrap();
        let r = it.call_fn(f, Value::Number(4095.0), &[Value::Number(16.0)]).unwrap();
        assert_eq!(show(&it, r), "string:fff");
    });
}

#[test]
fn limits_bound_the_work() {
    // Steps: a loop of toString calls stops at the step limit.
    let out = big(|| outcome("for (;;) (1/3).toString(2)", EvalLimits { max_steps: 20_000, ..Default::default() }, Heap::default()));
    assert_eq!(out, "limit:Steps");
    // Heap: the longest text (~1100 chars) fits a small heap budget or fails with a heap limit, never an abort.
    let out = big(|| {
        let heap = Heap::new(HeapLimits { max_bytes: 1 << 20, ..Default::default() }).unwrap();
        outcome("var a=[]; for (var i=0;i<1000;i++) a.push((5e-324 * (i+1)).toString(2)); a.length", EvalLimits { max_steps: 2_000_000, ..Default::default() }, heap)
    });
    assert!(out.starts_with("limit:Heap") || out == "number:1000", "{out}");
    assert_eq!(run("(5e-324).toString(2).length + ':' + (1.7976931348623157e308).toString(2).length"), "string:1076:1024");
}

#[test]
fn number_prototype_itself_represents_zero() {
    assert_eq!(run("Number.prototype.toString(2)"), "string:0");
}

#[test]
fn radix_conversion_charges_long_output_work() {
    big(|| {
        let prog = parse_script("0", Limits::default()).unwrap();
        let mut it = Interp::new(&prog, Heap::default(), EvalLimits { max_steps: 20, ..Default::default() }).unwrap();
        let f = it.get_prop(Value::Number(1.0), it.k_to_string).unwrap();
        assert!(matches!(it.call(f, Value::Number(f64::from_bits(1)), &[Value::Number(2.0)]), Err(Error::Limit(LimitKind::Steps))));
    });
}


#[test]
fn seed_failure_never_uses_a_fallback() {
    use std::io::{self, Cursor, Read};
    struct Fails;
    impl Read for Fails {
        fn read(&mut self, _: &mut [u8]) -> io::Result<usize> { Err(io::Error::other("unavailable")) }
    }
    assert_eq!(seed_from(&mut Fails), None);
    assert_eq!(seed_from(&mut Cursor::new([1; 15])), None);
    assert_eq!(seed_from(&mut Cursor::new([0; 16])), None);
    assert_eq!(seed_from(&mut Cursor::new([1; 16])), Some([0x0101010101010101; 2]));
}

#[test]
fn global_parse_int() {
    for (src, want) in [
        ("typeof parseInt", "string:function"), ("parseInt.length", "number:2"), ("parseInt.name", "string:parseInt"),
        ("parseInt('42')", "number:42"), ("parseInt('  -17px')", "number:-17"), ("parseInt('+8')", "number:8"),
        ("parseInt('0x1F')", "number:31"), ("parseInt('0X1f', 16)", "number:31"), ("parseInt('0x1F', 10)", "number:0"),
        ("parseInt('11', 2)", "number:3"), ("parseInt('zz', 36)", "number:1295"), ("parseInt('7', 2)", "number:NaN"),
        ("parseInt('9', 37)", "number:NaN"), ("parseInt('9', 1)", "number:NaN"), ("parseInt('12', 0)", "number:12"),
        ("parseInt('')", "number:NaN"), ("parseInt('-')", "number:NaN"), ("parseInt('abc')", "number:NaN"),
        ("parseInt()", "number:NaN"), ("parseInt(undefined)", "number:NaN"), ("parseInt(null, 36)", "number:1112745"),
        ("1 / parseInt('-0')", "number:-Infinity"), ("parseInt('3.9')", "number:3"), ("parseInt(12.9)", "number:12"),
        ("parseInt('12', 4294967298)", "number:1"), ("parseInt('123456789012345678901234567890')", "number:1.2345678901234568e+29"),
        ("parseInt('\\u00a0\\n 5')", "number:5"), ("parseInt({toString(){return '9'}})", "number:9"),
    ] {
        assert_eq!(run(src), want, "{src}");
    }
}

#[test]
fn global_encode_uri_component() {
    for (src, want) in [
        ("typeof encodeURIComponent", "string:function"), ("encodeURIComponent.length", "number:1"), ("encodeURIComponent.name", "string:encodeURIComponent"),
        ("encodeURIComponent('abc-_.!~*\\'()XYZ019')", "string:abc-_.!~*'()XYZ019"),
        ("encodeURIComponent('a b&c=d/e?f#g;h:i@j,k+l$')", "string:a%20b%26c%3Dd%2Fe%3Ff%23g%3Bh%3Ai%40j%2Ck%2Bl%24"),
        ("encodeURIComponent('\\u00e9')", "string:%C3%A9"), ("encodeURIComponent('\\u20ac')", "string:%E2%82%AC"),
        ("encodeURIComponent('\\ud83d\\ude00')", "string:%F0%9F%98%80"), ("encodeURIComponent('\\0')", "string:%00"),
        ("encodeURIComponent('')", "string:"), ("encodeURIComponent()", "string:undefined"), ("encodeURIComponent(null)", "string:null"),
        ("encodeURIComponent(12.5)", "string:12.5"), ("encodeURIComponent({toString(){return 'x y'}})", "string:x%20y"),
        ("var m; try{ encodeURIComponent('\\ud800') }catch(e){ m = e.name + ':' + e.message + ':' + (e instanceof URIError) + ':' + (e instanceof Error) } m", "string:URIError:URI malformed:true:true"),
        ("var m; try{ encodeURIComponent('\\udc00x') }catch(e){ m = e.name } m", "string:URIError"),
        ("new URIError('q').message + Object.prototype.toString.call(new URIError('q'))", "string:q[object Object]"),
    ] {
        assert_eq!(run(src), want, "{src}");
    }
}
