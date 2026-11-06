//! Tests of `crypto`, `navigator`, `Blob`, `File` and `FormData` (`browser_web.rs`). The `NODE_*` tables hold the results
//! of Node 22.18 for the same scripts, where Node agrees with the specification. Node differs on the iterator tag and on
//! `lastModified: 1.9` (the specification truncates it), so the tests do not compare those. The rest is by the
//! specifications (WebIDL, File API, XHR `FormData`, RFC 9562), not from a Chrome run.

use crate::browser::*;
use bare_html::tree;
use bare_js::eval::{Error, EvalLimits};
use bare_js::heap::HeapLimits;
use bare_js::value::Value;

fn big<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(64 << 20).spawn(f).unwrap().join().unwrap()
}

fn browser() -> Browser {
    let doc = tree::parse("<!doctype html><html><body><form id=f></form></body></html>");
    Browser::new(doc, BrowserLimits::default(), EvalLimits { max_steps: 50_000_000, ..Default::default() }, HeapLimits::default()).expect("browser")
}

fn show(b: &mut Browser, r: Result<Value, Error>) -> String {
    match r {
        Ok(v) => b.it.display(v),
        Err(Error::Throw(v)) if matches!(v, Value::Object(_)) => format!("throw:{}:{}", b.it.get_str(v, "name"), b.it.get_str(v, "message")),
        Err(Error::Throw(v)) => format!("throw-value:{}", b.it.display(v)),
        Err(Error::Limit(l)) => format!("limit:{l:?}"),
        Err(Error::Unsupported(u)) => format!("unsupported:{}", u.what),
        Err(e) => format!("other:{e:?}"),
    }
}

fn js(b: &mut Browser, src: &str) -> String {
    let r = b.run_script(src);
    show(b, r)
}

/// Run each script in order in one realm. The scripts that read the results of promises run after a microtask checkpoint.
fn run(table: &[(&str, &str)]) {
    big({
        let table: Vec<(String, String)> = table.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect();
        move || {
            let mut b = browser();
            for (src, want) in &table {
                if src.starts_with("out") {
                    b.drain().unwrap();
                }
                assert_eq!(&js(&mut b, src), want, "{src}");
            }
        }
    });
}

const NODE_CRYPTO: &[(&str, &str)] = &[
    (r#"/^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/.test(crypto.randomUUID())"#, r#"true"#),
    (r#"var s = new Set(); var ok = true; for (var i = 0; i < 1000; i++) { var u = crypto.randomUUID(); ok = ok && u.length === 36 && u === u.toLowerCase() && u[14] === '4' && '89ab'.includes(u[19]); s.add(u); } ok + ':' + s.size"#, r#"true:1000"#),
    (r#"var a = new Uint8Array(16); [crypto.getRandomValues(a) === a, a.some(function (x) { return x; })].join()"#, r#"true,true"#),
    (r#"[crypto.getRandomValues(new Uint32Array(4)).length, crypto.getRandomValues(new BigUint64Array(2)).length, crypto.getRandomValues(new Int8Array(3)).length, crypto.getRandomValues(new Uint8ClampedArray(3)).length, crypto.getRandomValues(new Int16Array(3)).length, crypto.getRandomValues(new Uint8Array(0)).length].join()"#, r#"4,2,3,3,3,0"#),
    (r#"var b = new Uint8Array(8); crypto.getRandomValues(b.subarray(2, 6)); [b[0], b[1], b[6], b[7]].join()"#, r#"0,0,0,0"#),
    (r#"crypto.getRandomValues(new Uint8Array(65536)).length"#, r#"65536"#),
    (r#"crypto.getRandomValues(new Uint32Array(16384)).length"#, r#"16384"#),
    (r#"try { crypto.getRandomValues(new Uint8Array(65537)); } catch (e) { e.name + ':' + e.code + ':' + (e instanceof Error) }"#, r#"QuotaExceededError:22:true"#),
    (r#"try { crypto.getRandomValues(new Uint32Array(16385)); } catch (e) { e.name + ':' + e.code }"#, r#"QuotaExceededError:22"#),
    (r#"try { crypto.getRandomValues(new Float32Array(1)); } catch (e) { e.name + ':' + e.code }"#, r#"TypeMismatchError:17"#),
    (r#"try { crypto.getRandomValues(new Float64Array(1)); } catch (e) { e.name + ':' + e.code }"#, r#"TypeMismatchError:17"#),
    (r#"try { crypto.getRandomValues(new DataView(new ArrayBuffer(4))); } catch (e) { e.name + ':' + e.code }"#, r#"TypeMismatchError:17"#),
    (r#"try { crypto.getRandomValues(new ArrayBuffer(4)); } catch (e) { e.name + ':' + e.code }"#, r#"TypeMismatchError:17"#),
    (r#"try { crypto.getRandomValues([1, 2]); } catch (e) { e.name + ':' + e.code }"#, r#"TypeMismatchError:17"#),
    (r#"try { crypto.getRandomValues(); } catch (e) { e.name }"#, r#"TypeError"#),
    (r#"try { var g = crypto.getRandomValues; g(new Uint8Array(1)); } catch (e) { e.name }"#, r#"TypeError"#),
    (r#"[crypto.getRandomValues.length, crypto.randomUUID.length, Object.prototype.toString.call(crypto), crypto === crypto, typeof crypto.getRandomValues, typeof crypto.randomUUID].join()"#, r#"1,0,[object Crypto],true,function,function"#),
];

#[test]
fn crypto_matches_node() {
    run(NODE_CRYPTO);
}

const NODE_BLOB: &[(&str, &str)] = &[
    (r#"var b = new Blob(['h\u00e9llo', new Uint8Array([65]), new Blob(['z'])], { type: 'Text/Plain' }); [b.size, b.type, Object.prototype.toString.call(b), b instanceof Blob].join()"#, r#"8,text/plain,[object Blob],true"#),
    (r#"[new Blob().size, new Blob([]).type, new Blob([], { type: '\u00e9' }).type, new Blob([], { type: 'a\nb' }).type, new Blob([], { type: 'TEXT/HTML;Charset=UTF-8' }).type, new Blob([], undefined).size, new Blob(undefined, null).size].join()"#, r#"0,,,,text/html;charset=utf-8,0,0"#),
    (r#"new Blob([1, null, undefined, {}, true]).size"#, r#"33"#),
    (r#"new Blob([new ArrayBuffer(3), new Uint16Array(2), new DataView(new ArrayBuffer(5))]).size"#, r#"12"#),
    (r#"try { new Blob(5); } catch (e) { e.name }"#, r#"TypeError"#),
    (r#"try { new Blob('abc'); } catch (e) { e.name }"#, r#"TypeError"#),
    (r#"try { new Blob([Symbol('s')]); } catch (e) { e.name }"#, r#"TypeError"#),
    (r#"try { new Blob([], 5); } catch (e) { e.name }"#, r#"TypeError"#),
    (r#"try { Blob(); } catch (e) { e.name }"#, r#"TypeError"#),
    (r#"var hs = new Blob(['hello world']); [hs.slice(6).size, hs.slice(-5, -1).size, hs.slice(3, 1).size, hs.slice(0, 100).size, hs.slice(NaN).size, hs.slice(-100).size, hs.slice(2, 5, 'A/B').type, hs.slice(2, 5).type, hs.slice(0, -Infinity).size, hs.slice(1.9, 3.9).size, hs.slice() instanceof Blob, hs.slice().size].join()"#, r#"5,4,0,11,11,11,a/b,,0,2,true,11"#),
    (r#"var out = []; hs.slice(0, 5).text().then(function (t) { out.push(t); }); hs.slice(6).arrayBuffer().then(function (ab) { out.push(Object.prototype.toString.call(ab) + new Uint8Array(ab).join('.')); }); hs.text().then(function (t) { out.push(t.length); }); typeof hs.text().then"#, r#"function"#),
    (r#"new Blob([new Uint8Array([0xEF, 0xBB, 0xBF, 0x41])]).text().then(function (t) { out.push('bom:' + t + t.length); }); new Blob([new Uint8Array([0xff, 0x41])]).text().then(function (t) { out.push('bad:' + t.charCodeAt(0).toString(16) + t.length); }); new Blob(['\ud800']).text().then(function (t) { out.push('lone:' + t.charCodeAt(0).toString(16) + t.length); }); new Blob(['\u20ac']).arrayBuffer().then(function (ab) { out.push('euro:' + new Uint8Array(ab).join('.')); }); 1"#, r#"1"#),
    (r#"var f = new File(['abc'], 'a.txt', { type: 'Text/Plain', lastModified: 12345 }); [f.name, f.size, f.type, f.lastModified, f instanceof Blob, f instanceof File, Object.prototype.toString.call(f), Object.getPrototypeOf(File) === Blob, Object.getPrototypeOf(File.prototype) === Blob.prototype].join()"#, r#"a.txt,3,text/plain,12345,true,true,[object File],true,true"#),
    (r#"[new File([], 'x').lastModified > 1e12, new File([], 'x').type === '', new File([], 12).name, new File(['ab'], 'x').slice(1).constructor === Blob, new File([new File(['q'], 'n')], 'y').size].join()"#, r#"true,true,12,true,1"#),
    (r#"try { new File(['a']); } catch (e) { e.name }"#, r#"TypeError"#),
    (r#"try { new File(); } catch (e) { e.name }"#, r#"TypeError"#),
    (r#"try { File.prototype.name; } catch (e) { e.name }"#, r#"TypeError"#),
    (r#"try { Object.getOwnPropertyDescriptor(File.prototype, 'name').get.call(new Blob()); } catch (e) { e.name }"#, r#"TypeError"#),
    (r#"try { Object.getOwnPropertyDescriptor(Blob.prototype, 'size').get.call({}); } catch (e) { e.name }"#, r#"TypeError"#),
    (r#"[Blob.length, File.length, Blob.name, File.name, Object.prototype.toString.call(Blob.prototype), Object.prototype.toString.call(File.prototype)].join()"#, r#"0,2,Blob,File,[object Blob],[object File]"#),
    (r#"class MyBlob extends Blob { hi() { return this.size; } } var mb = new MyBlob(['xyz']); [mb instanceof MyBlob, mb instanceof Blob, mb.hi()].join()"#, r#"true,true,3"#),
    (r#"Object.keys(new Blob(['a'])).length"#, r#"0"#),
    (r#"var out2 = []; try { Blob.prototype.text.call({}).catch(function (e) { out2.push(e.name); }); } catch (e) { out2.push('sync:' + e.name); } 1"#, r#"1"#),
    (r#"out.sort().join('|')"#, r#"11|[object ArrayBuffer]119.111.114.108.100|bad:fffd2|bom:A1|euro:226.130.172|hello|lone:fffd1"#),
    (r#"out2.join('|')"#, r#"TypeError"#),
];

#[test]
fn blob_and_file_matches_node() {
    run(NODE_BLOB);
}

const NODE_FORM: &[(&str, &str)] = &[
    (r#"var T = {}; var f = new FormData(); f.append('a', '1'); f.append('b', 2); f.append('a', { toString: function () { return 'x'; } }); [f.get('a'), f.getAll('a').join('|'), f.get('b'), f.get('zz'), f.has('a'), f.has('q'), [...f].join(';'), [...f.keys()].join(), [...f.values()].join(), f instanceof FormData, Object.prototype.toString.call(f)].join('#')"#, r#"1#1|x#2##true#false#a,1;b,2;a,x#a,b,a#1,2,x#true#[object FormData]"#),
    (r#"f.set('a', 'S'); [...f].join(';')"#, r#"a,S;b,2"#),
    (r#"f.set('c', 'new'); f.set('b', 3); [...f].join(';')"#, r#"a,S;b,3;c,new"#),
    (r#"f.delete('b'); f.delete('nope'); [...f].join(';')"#, r#"a,S;c,new"#),
    (r#"var o = []; f.forEach(function (v, k, fd) { o.push(k + '=' + v + (fd === f) + (this === T)); }, T); o.join(';')"#, r#"a=Struetrue;c=newtruetrue"#),
    (r#"[f[Symbol.iterator] === f.entries, typeof f.entries().next, typeof f.keys, FormData.prototype.constructor === FormData, f.append.length, f.set.length, f.get.length, f.delete.length, f.forEach.length].join()"#, r#"true,function,function,true,2,2,1,1,1"#),
    (r#"var fb = new FormData(); fb.append('file', new Blob(['hi'], { type: 'text/plain' })); var v1 = fb.get('file'); [v1 instanceof File, v1.name, v1.size, v1.type, v1.lastModified > 1e12].join()"#, r#"true,blob,2,text/plain,true"#),
    (r#"fb.append('file2', new Blob(['hi']), 'x.txt'); var v2 = fb.get('file2'); [v2 instanceof File, v2.name, v2.type === ''].join()"#, r#"true,x.txt,true"#),
    (r#"var ff = new File(['z'], 'z.txt', { lastModified: 77 }); fb.append('f', ff); [fb.get('f') === ff, fb.getAll('f').length].join()"#, r#"true,1"#),
    (r#"fb.append('g', ff, 'q.txt'); var g = fb.get('g'); [g !== ff, g.name, g.size, g.lastModified, g instanceof File].join()"#, r#"true,q.txt,1,77,true"#),
    (r#"fb.set('f', new Blob(['u']), 'n.bin'); [fb.getAll('f').length, fb.get('f').name, fb.get('f').size].join()"#, r#"1,n.bin,1"#),
    (r#"[...fb.keys()].join()"#, r#"file,file2,f,g"#),
    (r#"try { fb.append('a', 'str', 'name'); } catch (e) { e.name }"#, r#"TypeError"#),
    (r#"try { fb.append('a'); } catch (e) { e.name }"#, r#"TypeError"#),
    (r#"try { fb.set('a'); } catch (e) { e.name }"#, r#"TypeError"#),
    (r#"try { fb.get(); } catch (e) { e.name }"#, r#"TypeError"#),
    (r#"try { fb.getAll(); } catch (e) { e.name }"#, r#"TypeError"#),
    (r#"try { fb.has(); } catch (e) { e.name }"#, r#"TypeError"#),
    (r#"try { fb.delete(); } catch (e) { e.name }"#, r#"TypeError"#),
    (r#"try { fb.forEach(5); } catch (e) { e.name }"#, r#"TypeError"#),
    (r#"try { FormData.prototype.append.call({}, 'a', 'b'); } catch (e) { e.name }"#, r#"TypeError"#),
    (r#"try { FormData(); } catch (e) { e.name }"#, r#"TypeError"#),
    (r#"try { fb.append(Symbol('s'), 'b'); } catch (e) { e.name }"#, r#"TypeError"#),
    (r#"var f2 = new FormData(); f2.append('a', '1'); var it2 = f2.entries(); var r1 = it2.next(); f2.append('b', '2'); var r2 = it2.next(); var r3 = it2.next(); [JSON.stringify(r1), JSON.stringify(r2), JSON.stringify(r3), JSON.stringify(it2.next())].join('')"#, r#"{"value":["a","1"],"done":false}{"value":["b","2"],"done":false}{"done":true}{"done":true}"#),
    (r#"var f3 = new FormData(); f3.append('\ud800', '\udc00'); [f3.get('\ufffd') === '\ufffd', f3.has('\ud800')].join()"#, r#"true,true"#),
    (r#"var f4 = new FormData(); f4.append('x', 'a'); f4.append('y', 'b'); var seen = []; f4.forEach(function (v, k) { seen.push(k); if (k === 'x') f4.append('z', 'c'); }); seen.join()"#, r#"x,y,z"#),
    (r#"var f5 = new FormData(); f5.append('k', undefined); f5.append('n', null); f5.append(undefined, 1); [...f5].join(';')"#, r#"k,undefined;n,null;undefined,1"#),
    (r#"var f6 = new FormData(); f6.append('a', '1'); f6.append('b', '2'); f6.append('a', '3'); f6.set('a', 'z'); [...f6].join(';')"#, r#"a,z;b,2"#),
    (r#"var f7 = new FormData(); f7.append('a', '1'); f7.append('b', '2'); f7.append('a', '3'); f7.set('b', 'z'); [...f7].join(';')"#, r#"a,1;b,z;a,3"#),
    (r#"var f8 = new FormData(); for (var i = 0; i < 5; i++) f8.append('k', i); f8.delete('k'); [...f8].length"#, r#"0"#),
    (r#"var f9 = new FormData(); f9.append('a', 'b'); Array.from(f9).join() + '|' + Array.from(f9.keys()).join() + '|' + Array.from(f9.values()).join()"#, r#"a,b|a|b"#),
];

#[test]
fn formdata_matches_node() {
    run(NODE_FORM);
}

#[test]
fn navigator_has_the_fixed_chrome_like_values() {
    big(|| {
        let mut b = browser();
        let ua = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/155.0.0.0 Safari/537.36";
        assert_eq!(js(&mut b, "navigator.userAgent"), ua);
        assert_eq!(js(&mut b, "[navigator.language, navigator.languages.join(), Array.isArray(navigator.languages), Object.isFrozen(navigator.languages), navigator.languages === navigator.languages, navigator.platform, navigator.onLine, navigator.cookieEnabled].join('|')"), "en-US|en-US,en|true|true|true|Linux x86_64|true|true");
        assert_eq!(js(&mut b, "[Number.isInteger(navigator.hardwareConcurrency), navigator.hardwareConcurrency >= 1].join()"), "true,true");
        assert_eq!(js(&mut b, "var u = navigator.userAgentData; [u === navigator.userAgentData, u.mobile, u.platform, u.brands.length, Object.isFrozen(u.brands), u.brands.map(function (x) { return x.brand + '/' + x.version; }).join()].join('|')"), "true|false|Linux|3|true|Not.A/Brand/8,Chromium/155,Google Chrome/155");
        assert_eq!(js(&mut b, "navigator === navigator && Object.prototype.toString.call(navigator)"), "[object Navigator]");
        // The values are getters on the prototype, not own properties of the object.
        assert_eq!(js(&mut b, "var p = Object.getPrototypeOf(navigator); [Object.keys(navigator).length, typeof Object.getOwnPropertyDescriptor(p, 'userAgent').get, String(Object.getOwnPropertyDescriptor(p, 'userAgent').set), 'mediaDevices' in navigator].join()"), "0,function,undefined,false");
        assert_eq!(js(&mut b, "try { Object.getOwnPropertyDescriptor(p, 'userAgent').get.call({}); } catch (e) { e.name }"), "TypeError");
        // No `crypto.subtle`.
        assert_eq!(js(&mut b, "typeof crypto.subtle"), "undefined");
    });
}

#[test]
fn the_globals_are_lazy_accessors_that_become_data_properties() {
    big(|| {
        let mut b = browser();
        assert_eq!(js(&mut b, "['crypto', 'navigator', 'Blob', 'File', 'FormData'].map(function (n) { var d = Object.getOwnPropertyDescriptor(globalThis, n); return typeof d.get + typeof d.set + d.configurable; }).join()"), "functionfunctiontrue,functionfunctiontrue,functionfunctiontrue,functionfunctiontrue,functionfunctiontrue");
        assert_eq!(js(&mut b, "var B = Blob; var d = Object.getOwnPropertyDescriptor(globalThis, 'Blob'); [d.value === B, d.writable, d.enumerable, d.configurable, typeof File, 'get' in Object.getOwnPropertyDescriptor(globalThis, 'File')].join()"), "true,true,false,true,function,false");
        assert_eq!(js(&mut b, "navigator; var d2 = Object.getOwnPropertyDescriptor(globalThis, 'navigator'); [d2.enumerable, 'get' in d2, d2.value === navigator].join()"), "true,false,true");
        // A write before the first read replaces the accessor with the written value.
        let mut c = browser();
        assert_eq!(js(&mut c, "window.crypto = 5; [crypto, typeof FormData, typeof Object.getOwnPropertyDescriptor(globalThis, 'crypto').get].join()"), "5,function,undefined");
        assert_eq!(js(&mut c, "delete globalThis.File; typeof File"), "undefined");
    });
}

#[test]
fn spec_cases_that_node_gets_wrong() {
    big(|| {
        let mut b = browser();
        // WebIDL `[Clamp] long long`: an infinite offset is the size.
        assert_eq!(js(&mut b, "var h = new Blob(['hello world']); [h.slice(Infinity).size, h.slice(-Infinity, Infinity).size].join()"), "0,11");
        // `[EnforceRange] long long` truncates; an infinite value is a TypeError.
        assert_eq!(js(&mut b, "new File([], 'x', { lastModified: 1.9 }).lastModified"), "1");
        assert_eq!(js(&mut b, "try { new File([], 'x', { lastModified: Infinity }); } catch (e) { e.name }"), "TypeError");
        assert_eq!(js(&mut b, "FormData.length"), "0");
        assert_eq!(js(&mut b, "Object.keys(FormData.prototype).join()"), "append,delete,get,getAll,has,set,forEach,keys,values,entries");
        assert_eq!(js(&mut b, "Object.prototype.toString.call(new Blob()) + Object.prototype.toString.call(new File([], 'a'))"), "[object Blob][object File]");
        // `new FormData(form)` is a gate, not a wrong answer.
        assert_eq!(js(&mut b, "new FormData(document.getElementById('f'))"), "unsupported:FormData from a form element");
        // A Blob or a FormData as a fetch body is a gate (no multipart writer): it never sends "[object FormData]".
        assert_eq!(js(&mut b, "fetch('http://127.0.0.1:9/x', { method: 'POST', body: new FormData() })"), "unsupported:a Blob or FormData as a body");
        assert_eq!(js(&mut b, "fetch('http://127.0.0.1:9/x', { method: 'POST', body: new Blob(['x']) })"), "unsupported:a Blob or FormData as a body");
    });
}

#[test]
fn blobs_are_bounded_and_collected() {
    big(|| {
        let mut b = browser();
        // 256 MiB of Blob bytes are allowed together. One more byte is a limit, and a collect gives the bytes back.
        js(&mut b, "var ab = new ArrayBuffer(32 << 20), keep = []; for (var i = 0; i < 8; i++) keep.push(new Blob([ab]));");
        assert_eq!(js(&mut b, "new Blob([new Uint8Array(1)]).size"), "limit:Host(\"web-objects\")");
        js(&mut b, "keep = null;");
        b.collect(&[]).unwrap();
        assert_eq!(js(&mut b, "new Blob([ab]).size"), "33554432");
        // A form keeps its File alive. A dropped form frees the File.
        js(&mut b, "var fd = new FormData(); fd.append('f', new Blob(['kept']), 'k.txt'); var gone = new FormData(); gone.append('f', new Blob([ab]));");
        js(&mut b, "gone = null;");
        b.collect(&[]).unwrap();
        assert_eq!(js(&mut b, "[fd.get('f').name, fd.get('f').size].join()"), "k.txt,4");
        // The File of the dropped form and the result of the last Blob are free again: 8 more Blobs of 32 MiB fit.
        assert_eq!(js(&mut b, "var k2 = []; for (var i = 0; i < 7; i++) k2.push(new Blob([ab])); k2.length"), "7");
        // An iterator keeps its form alive.
        js(&mut b, "var itr = (function () { var t = new FormData(); t.append('a', 'b'); return t.entries(); })();");
        b.collect(&[]).unwrap();
        assert_eq!(js(&mut b, "JSON.stringify(itr.next().value)"), "[\"a\",\"b\"]");
    });
}
