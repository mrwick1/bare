//! Tests of `WebAssembly` (`browser_wasm.rs`). The modules are written in hex here: `w('0061736d...')` gives the bytes.

use crate::browser::*;
use crate::eng_new::Peek;
use crate::eng_new::NewFault as Error;
use crate::js_limits::{EvalLimits, HeapLimits};
use bare_html::tree;

fn big<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(64 << 20).spawn(f).unwrap().join().unwrap()
}

/// Run `src` after the helpers, then the microtasks. Gives `out.join('|')`.
fn go(src: &str) -> String {
    let src = src.to_string();
    big(move || {
        let mut b = Browser::new(tree::parse("<body></body>"), BrowserLimits::default(), EvalLimits { max_steps: 20_000_000, ..Default::default() }, HeapLimits::default()).expect("browser");
        let help = "var out = []; function w(h) { h = h.replace(/ /g, ''); var a = new Uint8Array(h.length / 2); for (var i = 0; i < a.length; i++) a[i] = parseInt(h.substr(2 * i, 2), 16); return a; }
            function err(f) { try { f(); return 'none'; } catch (e) { return e.name + ':' + (e instanceof Error); } }
            var HEAD = '0061736d 01000000 ';";
        let r = b.run_script(&format!("{help}\n{src}"));
        if let Err(Error::Throw(v)) = r {
            return format!("throw:{}:{}", b.it.get_str(v, "name"), b.it.get_str(v, "message"));
        }
        b.drain().unwrap();
        let v = b.run_script("out.join('|')").unwrap();
        b.it.display(v)
    })
}

/// `add(i32, i32) -> i32`.
const ADD: &str = "var ADD = w(HEAD + '0107 0160027f7f017f 030201 00 070701 03616464 0000 0a0901 0700 2000 2001 6a0b');";

#[test]
fn the_namespace_is_lazy_and_has_its_members() {
    let got = go("out.push(typeof WebAssembly, Object.prototype.toString.call(WebAssembly), typeof WebAssembly.Module, typeof WebAssembly.CompileError,
        Object.keys(WebAssembly).join(), WebAssembly === WebAssembly, new WebAssembly.CompileError('x') instanceof Error, WebAssembly.RuntimeError.prototype.name);");
    assert_eq!(got, "object|[object WebAssembly]|function|function|validate,compile,instantiate|true|true|RuntimeError");
}

#[test]
fn a_module_compiles_and_an_export_runs() {
    let got = go(&format!("{ADD} var m = new WebAssembly.Module(ADD); var i = new WebAssembly.Instance(m); var e = i.exports;
        out.push(e.add(2, 3), e.add(2147483647, 1), e.add('4', 1.9), e.add.length, e.add.name, e.add === i.exports.add, Object.isFrozen(e), Object.getPrototypeOf(e),
        JSON.stringify(WebAssembly.Module.exports(m)), JSON.stringify(WebAssembly.Module.imports(m)), WebAssembly.validate(ADD), WebAssembly.validate(new Uint8Array([1, 2, 3])));"));
    assert_eq!(got, "5|-2147483648|5|2|0|true|true||[{\"name\":\"add\",\"kind\":\"function\"}]|[]|true|false");
}

#[test]
fn bad_bytes_are_a_compile_error() {
    let got = go("out.push(err(() => new WebAssembly.Module(new Uint8Array([0, 97, 115, 109, 9, 0, 0, 0]))), err(() => new WebAssembly.Module(5)), err(() => WebAssembly.Module(new Uint8Array(8))));
        WebAssembly.compile(new Uint8Array(4)).catch(e => out.push('async ' + e.name));");
    assert_eq!(got, "CompileError:true|TypeError:true|TypeError:true|async CompileError");
}

#[test]
fn compile_and_instantiate_give_promises() {
    let got = go(&format!("{ADD} WebAssembly.instantiate(ADD).then(r => {{ out.push(r.module instanceof WebAssembly.Module, r.instance instanceof WebAssembly.Instance, r.instance.exports.add(1, 1));
        return WebAssembly.compile(ADD); }}).then(m => WebAssembly.instantiate(m)).then(i => out.push(i instanceof WebAssembly.Instance, i.exports.add(3, 4)));"));
    assert_eq!(got, "true|true|2|true|7");
}

#[test]
fn an_import_is_called_and_its_exception_passes_through() {
    // `g(x) = env.f(x) + 1`.
    let got = go("var M = w(HEAD + '0106 01 60017f017f 0209 01 03656e76 0166 0000 030201 00 070501 0167 0001 0a0b01 0900 2000 1000 4101 6a0b');
        var i = new WebAssembly.Instance(new WebAssembly.Module(M), { env: { f: x => x * 10 } }); out.push(i.exports.g(4));
        var boom = new Error('boom'); var j = new WebAssembly.Instance(new WebAssembly.Module(M), { env: { f: () => { throw boom; } } });
        try { j.exports.g(1) } catch (e) { out.push(e === boom) }
        out.push(err(() => new WebAssembly.Instance(new WebAssembly.Module(M))), err(() => new WebAssembly.Instance(new WebAssembly.Module(M), { env: { f: 1 } })));");
    assert_eq!(got, "41|true|TypeError:true|LinkError:true");
}

#[test]
fn an_import_can_call_an_export_again() {
    // `a() = env.f()`, `b() = 7`; `f` calls `b`.
    let got = go("var M = w(HEAD + '0105 01 6000017f 0209 01 03656e76 0166 0000 030302 0000 070902 0161 0001 0162 0002 0a0b02 0400 1000 0b 0400 4107 0b');
        var i = new WebAssembly.Instance(new WebAssembly.Module(M), { env: { f: () => i.exports.b() + 1 } }); out.push(i.exports.a(), i.exports.b());");
    assert_eq!(got, "8|7");
}

#[test]
fn a_trap_is_a_runtime_error() {
    let got = go("var M = w(HEAD + '0104 01 600000 030201 00 070501 0174 0000 0a0501 0300 000b');
        var e = new WebAssembly.Instance(new WebAssembly.Module(M)).exports; out.push(err(() => e.t()), err(() => e.t()));");
    assert_eq!(got, "RuntimeError:true|RuntimeError:true");
}

#[test]
fn i64_is_a_bigint() {
    let got = go("var M = w(HEAD + '0106 01 60017e017e 030201 00 070501 0169 0000 0a0601 0400 2000 0b');
        var e = new WebAssembly.Instance(new WebAssembly.Module(M)).exports; out.push(e.i(5n), e.i(-(2n ** 63n)), e.i(2n ** 64n + 3n), typeof e.i(0n), err(() => e.i(5)));");
    assert_eq!(got, "5|-9223372036854775808|3|bigint|TypeError:true");
}

#[test]
fn a_global_is_shared_with_the_module() {
    // A mutable i32 global, 42, exported as `g`.
    let got = go("var M = w(HEAD + '0606 01 7f01 412a 0b 070501 0167 0300');
        var g = new WebAssembly.Instance(new WebAssembly.Module(M)).exports.g; out.push(g instanceof WebAssembly.Global, g.value); g.value = 7; out.push(g.valueOf());
        var c = new WebAssembly.Global({ value: 'i64' }, 9n); out.push(c.value, err(() => { c.value = 1n }), err(() => new WebAssembly.Global({ value: 'x' })));");
    assert_eq!(got, "true|42|7|9|TypeError:true|TypeError:true");
}

#[test]
fn whatsapp_feature_checks_pass() {
    // The three checks of `WAWebKaleidoscopeWasmFeatureSupport`: bulk memory, reference types, and i64 to BigInt.
    let got = go("out.push(WebAssembly.validate(new Uint8Array([0,97,115,109,1,0,0,0,1,4,1,96,0,0,3,2,1,0,5,3,1,0,1,10,14,1,12,0,65,0,65,0,65,0,252,10,0,0,11])),
        WebAssembly.validate(new Uint8Array([0,97,115,109,1,0,0,0,1,4,1,96,0,0,3,2,1,0,10,7,1,5,0,208,112,26,11])));
        WebAssembly.instantiate(new Uint8Array([0,97,115,109,1,0,0,0,1,6,1,96,1,126,1,126,3,2,1,0,7,5,1,1,98,0,0,10,6,1,4,0,32,0,11])).then(r => out.push(r.instance.exports.b(BigInt(0)) === BigInt(0)));");
    assert_eq!(got, "true|true|true");
}

#[test]
fn a_memory_export_is_a_link_error_for_now() {
    let got = go("var M = w(HEAD + '0503 01 0001 070701 036d656d 0200'); out.push(err(() => new WebAssembly.Instance(new WebAssembly.Module(M))));");
    assert_eq!(got, "LinkError:true");
}

#[test]
fn func_handle_has_no_padding() {
    assert_eq!(size_of::<wasmi::Func>(), 8);
}
