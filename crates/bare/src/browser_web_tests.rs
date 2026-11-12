//! Tests of `crypto`, `navigator`, `Blob`, `File` and `FormData` (`browser_web.rs`). The `NODE_*` tables hold the results
//! of Node 22.18 for the same scripts, where Node agrees with the specification. Node differs on the iterator tag and on
//! `lastModified: 1.9` (the specification truncates it), so the tests do not compare those. The rest is by the
//! specifications (WebIDL, File API, XHR `FormData`, RFC 9562), not from a Chrome run.

use crate::browser::*;
use crate::browser_fetch::FetchLimits;
use crate::browser_fetch_tests::{go, mk};
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

/// `crypto.subtle` exists in a secure context only: https, and a loopback host (`browser_basics::secure`).
#[test]
fn subtle_exists_only_in_a_secure_context() {
    big(|| {
        for (base, want) in [("https://example.com/", "object"), ("http://127.0.0.1:8080/", "object"), ("http://localhost/", "object"), ("http://example.com/", "undefined")] {
            let mut b = mk(Some(base.into()), FetchLimits::default());
            assert_eq!(js(&mut b, "typeof crypto.subtle"), want, "{base}");
        }
    });
}

/// `digest` with the four SHA names, a `{name}` object, mixed case, and the FIPS 180-4 two-block message. Expected from NIST and hashlib.
#[test]
fn subtle_digest_hashes_with_the_sha_names() {
    big(|| {
        let mut b = mk(Some("https://example.com/".into()), FetchLimits::default());
        let got = go(&mut b, "var out = []; var hex = function (b) { return Array.from(new Uint8Array(b)).map(function (x) { return x.toString(16).padStart(2, '0'); }).join(''); }; var abc = new Uint8Array([97, 98, 99]); var sha = function (alg, data) { return crypto.subtle.digest(alg, data).then(function (h) { out.push(hex(h)); }); }; sha('SHA-1', abc); sha('sha-256', abc); sha('SHA-384', abc); sha('Sha-512', abc); sha({ name: 'SHA-1' }, abc); sha({ name: 'sha-512' }, abc); sha('SHA-256', new Uint8Array(0)); sha('SHA-256', new Uint8Array(Array.from('abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq', function (c) { return c.charCodeAt(0); }))); 1");
        assert_eq!(
            got,
            [
                "a9993e364706816aba3e25717850c26c9cd0d89d",
                "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
                "cb00753f45a35e8bb5a03d699ac65007272c32ab0eded1631a8b605a43ff5bed8086072ba1e7cc2358baeca134c825a7",
                "ddaf35a193617abacc417349ae20413112e6fa4e89a97ea20a9eeee64b55d39a2192992a274fc1a836ba3c23a3feebbd454d4423643ce80e2a9ac94fa54ca49f",
                "a9993e364706816aba3e25717850c26c9cd0d89d",
                "ddaf35a193617abacc417349ae20413112e6fa4e89a97ea20a9eeee64b55d39a2192992a274fc1a836ba3c23a3feebbd454d4423643ce80e2a9ac94fa54ca49f",
                "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
                "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1",
            ]
            .join("|")
        );
    });
}

/// The data forms, and that the bytes are copied when `digest` is called.
#[test]
fn subtle_digest_copies_the_bytes_of_any_buffer_source() {
    big(|| {
        let mut b = mk(Some("https://example.com/".into()), FetchLimits::default());
        let got = go(&mut b, "var out = []; var hex = function (b) { return Array.from(new Uint8Array(b)).map(function (x) { return x.toString(16).padStart(2, '0'); }).join(''); }; var sha = function (data) { return crypto.subtle.digest('SHA-256', data).then(function (h) { out.push(hex(h)); }); }; var u = new Uint8Array([0, 97, 98, 99, 0]); sha(u.subarray(1, 4)); sha(new DataView(u.buffer, 1, 3)); var ab = new ArrayBuffer(3); new Uint8Array(ab).set([97, 98, 99]); sha(ab); var m = new Uint8Array([97]); var p = crypto.subtle.digest('SHA-256', m); m[0] = 98; p.then(function (h) { out.push(hex(h)); }); 1");
        let abc = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
        assert_eq!(got, [abc, abc, abc, "ca978112ca1bbdcafac231b39a23dc4da786eff8147c4e72b9807785afee48bb"].join("|"));
    });
}

/// The promise, the result type, and the rejections: `NotSupportedError` for an unknown name, `TypeError` for bad arguments.
#[test]
fn subtle_digest_rejects_with_the_web_crypto_errors() {
    big(|| {
        let mut b = mk(Some("https://example.com/".into()), FetchLimits::default());
        let got = go(&mut b, "var out = []; var p = crypto.subtle.digest('SHA-256', new ArrayBuffer(0)); out.push(p instanceof Promise); p.then(function (h) { out.push(h instanceof ArrayBuffer, h.byteLength); }); crypto.subtle.digest('MD5', new ArrayBuffer(0)).catch(function (e) { out.push(e.name + ':' + e.code + ':' + (e instanceof DOMException)); }); crypto.subtle.digest('SHA-256', 'abc').catch(function (e) { out.push('string:' + e.name); }); crypto.subtle.digest('SHA-256', 5).catch(function (e) { out.push('number:' + e.name); }); crypto.subtle.digest('SHA-256').catch(function (e) { out.push('args:' + e.name); }); crypto.subtle.digest({}, new ArrayBuffer(0)).catch(function (e) { out.push('noname:' + e.name); }); 1");
        assert_eq!(got, "true|true|32|NotSupportedError:9:true|string:TypeError|number:TypeError|args:TypeError|noname:TypeError");
    });
}

/// The other methods exist and reject with `NotSupportedError`. `SubtleCrypto` has an illegal constructor.
#[test]
fn subtle_has_every_method_and_an_illegal_constructor() {
    big(|| {
        let mut b = mk(Some("https://example.com/".into()), FetchLimits::default());
        let got = go(&mut b, "var out = []; var s = crypto.subtle; out.push(Object.prototype.toString.call(s), Object.getPrototypeOf(s) === SubtleCrypto.prototype, [s.digest.length, s.encrypt.length, s.sign.length, s.verify.length, s.generateKey.length, s.importKey.length, s.exportKey.length, s.deriveKey.length, s.deriveBits.length, s.wrapKey.length, s.unwrapKey.length].join()); ['encrypt', 'decrypt', 'sign', 'verify', 'generateKey', 'importKey', 'exportKey', 'deriveKey', 'deriveBits', 'wrapKey', 'unwrapKey'].forEach(function (k) { s[k]().catch(function (e) { out.push(k + ':' + e.name); }); }); try { new SubtleCrypto(); } catch (e) { out.push(e.name + ':' + e.message); } 1");
        assert_eq!(got, "[object SubtleCrypto]|true|2,3,3,4,3,5,2,5,2,4,7|TypeError:Illegal constructor|encrypt:NotSupportedError|decrypt:NotSupportedError|sign:NotSupportedError|verify:NotSupportedError|generateKey:NotSupportedError|importKey:NotSupportedError|exportKey:NotSupportedError|deriveKey:NotSupportedError|deriveBits:NotSupportedError|wrapKey:NotSupportedError|unwrapKey:NotSupportedError");
    });
}

#[test]
fn wire_client_hints_match_user_agent_data() {
    big(|| {
        let mut b = browser();
        let brands = js(&mut b, "navigator.userAgentData.brands.map(function (x) { return '\"' + x.brand + '\";v=\"' + x.version + '\"'; }).join(', ')");
        let id = crate::browser_web::wire_identity();
        assert_eq!(id.hints[0], ("sec-ch-ua", brands));
        assert_eq!(&id.hints[1..], &[("sec-ch-ua-mobile", "?0".to_string()), ("sec-ch-ua-platform", "\"Linux\"".to_string())]);
        assert_eq!(js(&mut b, "navigator.userAgentData.platform"), "Linux");
        assert_eq!(id.user_agent, js(&mut b, "navigator.userAgent"));
    });
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
        // No secure context here (no URL), so no `crypto.subtle`.
        assert_eq!(js(&mut b, "typeof crypto.subtle"), "undefined");
    });
}

/// WICG UA Client Hints: `getHighEntropyValues(hints)` and `toJSON()` of `NavigatorUAData`.
#[test]
fn user_agent_data_has_high_entropy_values_and_to_json() {
    run(&[
        ("var out = []; var u = navigator.userAgentData; var p = u.getHighEntropyValues(['architecture', 'bitness', 'model', 'platformVersion', 'uaFullVersion', 'fullVersionList', 'wow64', 'formFactors']); out.push(p instanceof Promise); p.then(function (v) { out.push(Object.keys(v).join()); out.push([v.architecture, v.bitness, v.model, v.platformVersion, v.uaFullVersion, v.wow64, v.formFactors.join(), v.mobile, v.platform, v.brands.length].join('|')); out.push(v.fullVersionList.map(function (x) { return x.brand + '/' + x.version; }).join()); }); 1", "1"),
        ("out.join(';')", "true;brands,mobile,platform,architecture,bitness,model,platformVersion,uaFullVersion,fullVersionList,wow64,formFactors;x86|64|||155.0.0.0|false|Desktop|false|Linux|3;Not.A/Brand/8.0.0.0,Chromium/155.0.0.0,Google Chrome/155.0.0.0"),
        ("out.length = 0; navigator.userAgentData.getHighEntropyValues([]).then(function (v) { out.push(Object.keys(v).join()); }); navigator.userAgentData.getHighEntropyValues(['bitness', 'nope']).then(function (v) { out.push(Object.keys(v).join()); }); 1", "1"),
        ("out.join(';')", "brands,mobile,platform;brands,mobile,platform,bitness"),
        ("out.length = 0; navigator.userAgentData.getHighEntropyValues().catch(function (e) { out.push(e.name); }); navigator.userAgentData.getHighEntropyValues(5).catch(function (e) { out.push(e.name); }); 1", "1"),
        ("out.join()", "TypeError,TypeError"),
        ("var j = navigator.userAgentData.toJSON(); [Object.keys(j).join(), j.brands.length, j.brands[0].brand, j.mobile, j.platform, JSON.stringify(navigator.userAgentData) === JSON.stringify(j)].join('|')", "brands,mobile,platform|3|Not.A/Brand|false|Linux|true"),
        ("var d = Object.getOwnPropertyDescriptor(Object.getPrototypeOf(navigator.userAgentData), 'getHighEntropyValues'); [typeof d.value, d.value.length, Object.getPrototypeOf(navigator.userAgentData).toJSON.length].join()", "function,1,0"),
        ("out.length = 0; try { navigator.userAgentData.getHighEntropyValues.call({}, []).catch(function (e) { out.push('p:' + e.name); }); } catch (e) { out.push('s:' + e.name); } 1", "1"),
        ("out.join()", "p:TypeError"),
    ]);
}

/// W3C File API, Blob URL Store: `URL.createObjectURL` and `URL.revokeObjectURL`, read by `fetch`.
#[test]
fn object_urls_are_made_read_by_fetch_and_revoked() {
    run(&[
        ("var out = []; var b = new Blob(['hello'], { type: 'text/plain' }); var u = URL.createObjectURL(b); var u2 = URL.createObjectURL(b); out.push(/^blob:null\\/[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/.test(u), u !== u2, typeof URL.createObjectURL + URL.createObjectURL.length + URL.revokeObjectURL.length); fetch(u).then(function (r) { out.push(r.status + ',' + r.ok + ',' + r.headers.get('content-type') + ',' + (r.url === u)); return r.text(); }).then(function (t) { out.push(t); }); 1", "1"),
        ("out.join(';')", "true;true;function11;200,true,text/plain,true;hello"),
        ("out.length = 0; fetch(new URL(u)).then(function (r) { return r.arrayBuffer(); }).then(function (a) { out.push(a.byteLength); }); fetch(u + '#frag').then(function (r) { out.push(r.status); }); new File(['xy'], 'f.txt').size", "2"),
        ("out.sort().join()", "200,5"),
        ("out.length = 0; URL.revokeObjectURL(u); URL.revokeObjectURL(u); URL.revokeObjectURL('blob:null/none'); URL.revokeObjectURL(); 1", "throw:TypeError:Failed to execute 'revokeObjectURL' on 'URL': 1 argument required, but only 0 present."),
        ("out.length = 0; fetch(u).catch(function (e) { out.push(e.name + ':' + e.message); }); fetch(u2).then(function (r) { out.push('u2:' + r.status); }); 1", "1"),
        ("out.sort().join()", "TypeError:Failed to fetch,u2:200"),
        ("[(function () { try { URL.createObjectURL({}); } catch (e) { return e.name; } })(), (function () { try { URL.createObjectURL(); } catch (e) { return e.name; } })(), (function () { try { URL.createObjectURL('blob:x'); } catch (e) { return e.name; } })()].join()", "TypeError,TypeError,TypeError"),
        ("var f = new File(['abc'], 'a.png', { type: 'image/png' }); var fu = URL.createObjectURL(f); out.length = 0; fetch(fu).then(function (r) { out.push(r.headers.get('content-type')); return r.blob(); }).then(function (x) { out.push(x.size + x.type); }); URL.revokeObjectURL(fu + '#x'); 1", "1"),
        ("out.join()", "image/png,3image/png"),
        ("out.length = 0; var e0 = URL.createObjectURL(new Blob()); fetch(e0).then(function (r) { out.push(String(r.headers.get('content-type')) + r.headers.has('content-type')); }); 1", "1"),
        ("out.join()", "nullfalse"),
        ("var p = new URL(u2); [p.protocol, p.origin, p.pathname === u2.slice(5)].join()", "blob:,null,true"),
    ]);
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
        // `new FormData(form)` reads the form (tests in browser_elements_tests.rs); another argument is a TypeError.
        assert_eq!(js(&mut b, "[...new FormData(document.getElementById('f'))].length"), "0");
        assert_eq!(js(&mut b, "new FormData({})"), "throw:TypeError:Failed to construct 'FormData': parameter 1 is not of type 'HTMLFormElement'.");
        // A Blob or a FormData body is sent (tests in browser_fetch_tests.rs). Without a fetch configuration it stops at the origin gate.
        assert_eq!(js(&mut b, "fetch('http://127.0.0.1:9/x', { method: 'POST', body: new FormData() })"), "unsupported:fetch without a configured document origin (Browser::configure_fetch)");
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
