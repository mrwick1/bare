//! Tests of `WebAssembly.Memory` and `WebAssembly.Table` (`browser_wasm.rs`). The modules are hex, made by a small script:
//! - `MEM`: memory `mem` (1 to 4 pages); `load(a)` = `i32.load8_u`, `store(a, v)` = `i32.store8`, `grow(n)` = `memory.grow`, `size()`.
//! - `IMP`: imports `env.mem` (memory) and `env.tab` (funcref table, 2); exports them again as `m` and `t`; `peek(a)` = `i32.load8_u`,
//!   `call(i)` = `call_indirect` of a `() -> i32` in the table.
//! - `CB`: imports `env.cb`; memory `mem` (1 page); `run()` grows the memory by one page, then calls `cb`.
//! - `SEVEN`: `f()` gives 7.

use crate::browser_wasm_tests::go;

const MODS: &str = "var MEM = '0061736d01000000010f0360017f017f60027f7f006000017f03050400010002050401010104072405036d656d0200046c6f616400000573746f726500010467726f7700020473697a6500030a1f04070020002d00000b0900200020013a00000b0600200040000b04003f000b';
    var IMP = '0061736d01000000010a0260017f017f6000017f02180203656e76036d656d02000103656e7603746162017000020303020000071704016d020001740100047065656b00000463616c6c00010a1102070020002d00000b070020001101000b';
    var CB = '0061736d01000000010401600000020a0103656e760263620000030201000503010001070d02036d656d02000372756e00010a0b010900410140001a10000b';
    var SEVEN = '0061736d010000000105016000017f03020100070501016600000a0601040041070b';
    function inst(h, imports) { return new WebAssembly.Instance(new WebAssembly.Module(w(h)), imports).exports; }";

fn run(src: &str) -> String {
    go(&format!("{MODS}\n{src}"))
}

#[test]
fn a_memory_has_a_buffer_that_grow_detaches() {
    let got = run("var m = new WebAssembly.Memory({initial: 1, maximum: 3}); var b = m.buffer;
        out.push(b instanceof ArrayBuffer, b.byteLength, b === m.buffer, m.grow(1), b.byteLength, b.detached, m.buffer.byteLength, m.buffer !== b,
            err(() => m.grow(5)), err(() => new WebAssembly.Memory({initial: 2, maximum: 1})), err(() => new WebAssembly.Memory({})),
            err(() => m.buffer.transfer()), Object.prototype.toString.call(m), m instanceof WebAssembly.Memory, err(() => WebAssembly.Memory({initial: 1})));");
    assert_eq!(got, "true|65536|true|1|0|true|131072|true|RangeError:true|RangeError:true|TypeError:true|TypeError:true|[object WebAssembly.Memory]|true|TypeError:true");
}

#[test]
fn an_exported_memory_shares_its_bytes_with_wasm() {
    let got = run("var e = inst(MEM); var u = new Uint8Array(e.mem.buffer); u[5] = 42; e.store(6, 7);
        out.push(e.load(5), u[6], e.mem === e.mem, e.mem instanceof WebAssembly.Memory, e.size(), e.grow(1), u.length, e.mem.buffer.byteLength, e.size(),
            e.grow(10), new Uint8Array(e.mem.buffer)[5]);");
    assert_eq!(got, "42|7|true|true|1|1|0|131072|2|-1|42");
}

#[test]
fn a_module_imports_a_memory_and_a_table() {
    let got = run("var mem = new WebAssembly.Memory({initial: 1}); new Uint8Array(mem.buffer)[3] = 9;
        var tab = new WebAssembly.Table({element: 'anyfunc', initial: 2}); var seven = inst(SEVEN).f; tab.set(1, seven);
        var e = inst(IMP, {env: {mem, tab}});
        out.push(e.peek(3), e.m === mem, e.t === tab, e.call(1), tab.get(1) === seven, tab.get(0), tab.length, tab.grow(2), tab.length,
            err(() => e.call(0)), err(() => tab.get(10)), err(() => inst(IMP, {env: {mem: 1, tab}})), err(() => tab.set(0, () => 1)));");
    assert_eq!(got, "9|true|true|7|true||2|2|4|RuntimeError:true|RangeError:true|LinkError:true|TypeError:true");
}

#[test]
fn a_grow_in_wasm_detaches_the_buffer_before_an_import_runs() {
    let got = run("var e, old, seen = [];
        e = inst(CB, {env: {cb() { seen.push(old.byteLength, old.detached, e.mem.buffer.byteLength); }}});
        old = e.mem.buffer; e.run(); out.push(seen.join(), old.byteLength);");
    assert_eq!(got, "0,true,131072|0");
}

#[test]
fn a_table_of_externref_holds_js_values() {
    let got = run("var t = new WebAssembly.Table({element: 'externref', initial: 1}, 'x'); out.push(t.get(0), t.length); t.set(0, {a: 1});
        out.push(t.get(0).a, err(() => new WebAssembly.Table({element: 'bad', initial: 1})), Object.prototype.toString.call(t)); t.set(0); out.push(t.get(0));");
    assert_eq!(got, "x|1|1|TypeError:true|[object WebAssembly.Table]|");
}
