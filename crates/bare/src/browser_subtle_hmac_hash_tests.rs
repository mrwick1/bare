//! HMAC in `SubtleCrypto` with SHA-1, SHA-384 and SHA-512, not only SHA-256. WhatsApp Web checks its app state sync (syncd)
//! with HMAC SHA-512. The MACs are the RFC 2202 and RFC 4231 test case 2 values; a key longer than the block is hashed first.

use crate::browser_fetch::FetchLimits;
use crate::browser_fetch_tests::{go, mk};

const SCRIPT: &str = r#"var out = []; var c = crypto.subtle;
var hex = function (b) { return Array.from(new Uint8Array(b)).map(function (x) { return x.toString(16).padStart(2, '0'); }).join(''); };
var te = new TextEncoder(), msg = te.encode('what do ya want for nothing?');
(async function () {
  for (var h of ['SHA-1', 'SHA-384', 'SHA-512']) {
    var k = await c.importKey('raw', te.encode('Jefe'), { name: 'HMAC', hash: { name: h } }, false, ['sign', 'verify']);
    var sig = await c.sign('HMAC', k, msg);
    out.push(h + ':' + hex(sig) + ':' + await c.verify('HMAC', k, sig, msg) + ':' + JSON.stringify(k.algorithm));
  }
  var long = new Uint8Array(200); for (var i = 0; i < 200; i++) long[i] = i;
  var lk = await c.importKey('raw', long, { name: 'HMAC', hash: 'SHA-512' }, false, ['sign']);
  out.push('long:' + hex(await c.sign('HMAC', lk, te.encode('x'))).slice(0, 32));
  var g = await c.generateKey({ name: 'HMAC', hash: 'SHA-512' }, true, ['sign']);
  out.push('gen:' + JSON.stringify(g.algorithm) + ':' + (await c.exportKey('raw', g)).byteLength);
  var k256 = await c.importKey('raw', te.encode('Jefe'), { name: 'HMAC', hash: 'SHA-256' }, false, ['sign']);
  out.push('256:' + hex(await c.sign('HMAC', k256, msg)));
})().catch(function (e) { out.push('uncaught:' + e.name + ':' + e.message); });
"#;

#[test]
fn hmac_signs_with_each_sha_hash() {
    std::thread::Builder::new()
        .stack_size(64 << 20)
        .spawn(|| {
            let mut b = mk(Some("https://example.com/".into()), FetchLimits::default());
            let got = go(&mut b, SCRIPT);
            let want = [
                r#"SHA-1:effcdf6ae5eb2fa2d27416d5f184df9c259a7c79:true:{"name":"HMAC","hash":{"name":"SHA-1"},"length":32}"#,
                r#"SHA-384:af45d2e376484031617f78d2b58a6b1b9c7ef464f5a01b47e42ec3736322445e8e2240ca5e69e2c78b3239ecfab21649:true:{"name":"HMAC","hash":{"name":"SHA-384"},"length":32}"#,
                r#"SHA-512:164b7a7bfcf819e2e395fbe73b56e0a387bd64222e831fd610270cd7ea2505549758bf75c05a994a6d034f65f8f0e6fdcaeab1a34d4a6b4b636e070a38bce737:true:{"name":"HMAC","hash":{"name":"SHA-512"},"length":32}"#,
                "long:779a0014910736e8140788feec8feff2",
                r#"gen:{"name":"HMAC","hash":{"name":"SHA-512"},"length":1024}:128"#,
                "256:5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843",
            ];
            let got: Vec<&str> = got.split('|').collect();
            assert_eq!(got, want);
        })
        .unwrap()
        .join()
        .unwrap();
}
