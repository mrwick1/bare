//! Tests of AES-CTR and HMAC in `SubtleCrypto` (`browser_subtle.rs`). WhatsApp Web makes an AES-CTR key at boot and signs with
//! HMAC SHA-256. The expected values come from Node 22 (`crypto.subtle`) for the same script, with one change to follow the spec:
//! `algorithm` of a generated HMAC key lists `hash` before `length`, as the `HmacKeyAlgorithm` dictionary orders its members
//! (Node lists `length` first for a generated key).

use crate::browser_fetch::FetchLimits;
use crate::browser_fetch_tests::{go, mk};

fn big<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(64 << 20).spawn(f).unwrap().join().unwrap()
}

const SCRIPT: &str = r#"var out = []; var c = crypto.subtle;
var hex = function (b) { return Array.from(new Uint8Array(b)).map(function (x) { return x.toString(16).padStart(2, '0'); }).join(''); };
var seq = function (a, b) { var r = new Uint8Array(b - a); for (var i = 0; i < r.length; i++) r[i] = a + i; return r; };
var te = new TextEncoder(), td = new TextDecoder(), msg = te.encode('The quick brown fox jumps over the lazy dog');
var fail = function (p) { return p.then(function () { return 'ok'; }, function (e) { return e.name; }); };
(async function () {
  var k = await c.importKey('raw', seq(0, 16), 'AES-CTR', true, ['encrypt', 'decrypt']);
  var ct = await c.encrypt({ name: 'AES-CTR', counter: seq(240, 256), length: 64 }, k, msg);
  out.push('ctr:' + hex(ct) + ':' + td.decode(await c.decrypt({ name: 'AES-CTR', counter: seq(240, 256), length: 64 }, k, ct)));
  var wrap = new Uint8Array(16); wrap.fill(255, 8);
  out.push('ctr-wrap8:' + hex(await c.encrypt({ name: 'AES-CTR', counter: wrap, length: 8 }, k, msg)));
  out.push('ctr-alg:' + JSON.stringify(k.algorithm) + ':' + hex(await c.exportKey('raw', k)));
  var g = await c.generateKey({ name: 'AES-CTR', length: 128 }, false, ['encrypt', 'decrypt']);
  out.push('ctr-gen:' + JSON.stringify(g.algorithm) + ':' + td.decode(await c.decrypt({ name: 'AES-CTR', counter: seq(0, 16), length: 128 }, g, await c.encrypt({ name: 'AES-CTR', counter: seq(0, 16), length: 128 }, g, msg))));
  var h = await c.importKey('raw', seq(0, 20), { name: 'HMAC', hash: 'SHA-256' }, true, ['sign', 'verify']);
  var sig = await c.sign('HMAC', h, msg);
  out.push('hmac:' + hex(sig) + ':' + await c.verify({ name: 'HMAC' }, h, sig, msg) + ':' + await c.verify('HMAC', h, new Uint8Array(32), msg));
  out.push('hmac-alg:' + JSON.stringify(h.algorithm) + ':' + h.usages.join() + ':' + hex(await c.exportKey('raw', h)));
  var hg = await c.generateKey({ name: 'HMAC', hash: { name: 'SHA-256' } }, true, ['sign']);
  out.push('hmac-gen:' + JSON.stringify(hg.algorithm) + ':' + (await c.exportKey('raw', hg)).byteLength);
  out.push('errors:' + [
    await fail(c.encrypt({ name: 'AES-CTR', counter: seq(0, 8), length: 64 }, k, msg)),
    await fail(c.encrypt({ name: 'AES-CTR', counter: seq(0, 16), length: 0 }, k, msg)),
    await fail(c.encrypt({ name: 'AES-CTR', counter: seq(0, 16), length: 129 }, k, msg)),
    await fail(c.sign('HMAC', k, msg)),
    await fail(c.importKey('raw', seq(0, 20), { name: 'HMAC', hash: 'SHA-256' }, true, ['encrypt'])),
    await fail(c.importKey('raw', new Uint8Array(0), { name: 'HMAC', hash: 'SHA-256' }, true, ['sign'])),
    await fail(c.importKey('raw', seq(0, 20), { name: 'HMAC' }, true, ['sign'])),
  ].join());
})().catch(function (e) { out.push('uncaught:' + e.name + ':' + e.message); });
"#;

#[test]
fn aes_ctr_and_hmac_match_node() {
    big(|| {
        let mut b = mk(Some("https://example.com/".into()), FetchLimits::default());
        let got = go(&mut b, SCRIPT);
        let want = [
            r#"ctr:32cfa2c84527582bfc71bc755c61c38dd4eeaf20ddeb51ddd78d1ccd0bee3f9eba14b23a1d2192bd9feeec:The quick brown fox jumps over the lazy dog"#,
            r#"ctr-wrap8:6dcf8a2a7b2d3bcbd4f261512bc8fa321daff1b89e2c807ea839c1221e779d03c8d3f684a9fb21e57fff70"#,
            r#"ctr-alg:{"name":"AES-CTR","length":128}:000102030405060708090a0b0c0d0e0f"#,
            r#"ctr-gen:{"name":"AES-CTR","length":128}:The quick brown fox jumps over the lazy dog"#,
            r#"hmac:28e74a432bce0911d9d023ec95420dd77a151bbee3b9513027ca095fc0c80852:true:false"#,
            r#"hmac-alg:{"name":"HMAC","hash":{"name":"SHA-256"},"length":160}:sign,verify:000102030405060708090a0b0c0d0e0f10111213"#,
            r#"hmac-gen:{"name":"HMAC","hash":{"name":"SHA-256"},"length":512}:64"#,
            r#"errors:OperationError,OperationError,OperationError,InvalidAccessError,SyntaxError,DataError,TypeError"#,
        ];
        let got: Vec<&str> = got.split('|').collect();
        assert_eq!(got, want);
    });
}
