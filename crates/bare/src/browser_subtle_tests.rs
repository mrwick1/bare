//! Tests of the `SubtleCrypto` key methods (`browser_subtle.rs`): AES-GCM, AES-CBC and HKDF.
//! The expected values come from Node 22.18 (`crypto.subtle`) for the same script, with two changes to follow the spec:
//! `usages` lists the usages in the order of the `KeyUsage` enum (Node keeps the order of the call), and a non-extractable
//! export is `InvalidAccessError` (Node names it `InvalidAccessException`).

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
  var g = await c.importKey('raw', seq(0, 32), 'aes-gcm', false, ['encrypt', 'decrypt']);
  var ct = await c.encrypt({ name: 'AES-GCM', iv: seq(200, 212), additionalData: te.encode('header') }, g, msg);
  out.push('gcm:' + hex(ct));
  out.push('gcm-back:' + td.decode(await c.decrypt({ name: 'AES-GCM', iv: seq(200, 212), additionalData: te.encode('header') }, g, ct)));
  var ct96 = await c.encrypt({ name: 'AES-GCM', iv: seq(200, 212), tagLength: 96 }, g, msg);
  out.push('gcm96:' + ct96.byteLength + ':' + td.decode(await c.decrypt({ name: 'AES-GCM', iv: seq(200, 212), tagLength: 96 }, g, ct96)));
  var bad = new Uint8Array(ct); bad[0] ^= 1;
  out.push('tamper:' + await fail(c.decrypt({ name: 'AES-GCM', iv: seq(200, 212), additionalData: te.encode('header') }, g, bad)));
  var b = await c.importKey('raw', seq(0, 32), { name: 'AES-CBC' }, true, ['encrypt', 'decrypt']);
  var cb = await c.encrypt({ name: 'AES-CBC', iv: seq(100, 116) }, b, msg);
  out.push('cbc:' + hex(cb) + ':' + td.decode(await c.decrypt({ name: 'AES-CBC', iv: seq(100, 116) }, b, cb)));
  out.push('cbc-export:' + hex(await c.exportKey('raw', b)));
  var h = await c.importKey('raw', seq(0, 22), 'HKDF', false, ['deriveKey', 'deriveBits']);
  var hp = { name: 'HKDF', hash: 'SHA-256', salt: seq(0, 13), info: seq(240, 250) };
  out.push('hkdf:' + hex(await c.deriveBits(hp, h, 336)));
  var dk = await c.deriveKey(hp, h, { name: 'AES-GCM', length: 256 }, true, ['encrypt', 'decrypt']);
  out.push('derived:' + hex(await c.exportKey('raw', dk)) + ':' + JSON.stringify(dk.algorithm) + ':' + dk.usages.join() + ':' + dk.type + ':' + dk.extractable);
  var nk = await c.deriveKey({ name: 'HKDF', hash: { name: 'SHA-256' }, salt: seq(1, 17), info: te.encode('msal') }, h, { name: 'AES-GCM', length: 256 }, false, ['encrypt', 'decrypt']);
  out.push('msal:' + hex(await c.encrypt({ name: 'AES-GCM', iv: new Uint8Array(12) }, nk, te.encode('{"a":1}'))));
  var k = await c.generateKey({ name: 'AES-GCM', length: 128 }, true, ['decrypt', 'encrypt']);
  var raw = await c.exportKey('raw', k);
  out.push('gen:' + raw.byteLength + ':' + JSON.stringify(k.algorithm) + ':' + k.usages.join() + ':' + Object.prototype.toString.call(k));
  var e2 = await c.encrypt({ name: 'AES-GCM', iv: seq(0, 12) }, k, msg);
  out.push('gen-back:' + td.decode(await c.decrypt({ name: 'AES-GCM', iv: seq(0, 12) }, k, e2)));
  out.push('errors:' + [
    await fail(c.exportKey('raw', g)),
    await fail(c.encrypt({ name: 'AES-CBC', iv: seq(0, 16) }, g, msg)),
    await fail(c.generateKey({ name: 'AES-GCM', length: 100 }, true, ['encrypt'])),
    await fail(c.generateKey({ name: 'AES-GCM', length: 128 }, true, [])),
    await fail(c.generateKey({ name: 'AES-GCM', length: 128 }, true, ['sign'])),
    await fail(c.generateKey({ name: 'AES-GCM', length: 128 }, true, ['bogus'])),
    await fail(c.importKey('raw', seq(0, 20), 'AES-GCM', true, ['encrypt'])),
    await fail(c.importKey('raw', seq(0, 20), 'HKDF', true, ['deriveKey'])),
    await fail(c.importKey('raw', seq(0, 16), 'AES-XYZ', true, ['encrypt'])),
    await fail(c.deriveBits(hp, g, 256)),
    await fail(c.deriveBits(hp, h, 13)),
    await fail(c.encrypt({ name: 'AES-GCM', iv: seq(0, 12), tagLength: 100 }, g, msg)),
    await fail(c.decrypt({ name: 'AES-CBC', iv: seq(0, 16) }, b, new Uint8Array(17))),
    await fail(c.encrypt({ name: 'AES-CBC', iv: seq(0, 8) }, b, msg)),
  ].join());
})().catch(function (e) { out.push('uncaught:' + e.name + ':' + e.message); });
"#;

#[test]
fn aes_and_hkdf_keys_match_node() {
    big(|| {
        let mut b = mk(Some("https://example.com/".into()), FetchLimits::default());
        let got = go(&mut b, SCRIPT);
        let want = [
            r#"gcm:451004fbf7b992431f1c7dcc7171853486e894886e1ec5549e895497bfe38b9da4e90acaf365538a56902361e30ef518e5d6510149d632253c42ca"#,
            r#"gcm-back:The quick brown fox jumps over the lazy dog"#,
            r#"gcm96:55:The quick brown fox jumps over the lazy dog"#,
            r#"tamper:OperationError"#,
            r#"cbc:f865a2689e1ff9cfbe3ab97f7f5d3e8513b1d396e276da84a7cbfb9436dc8ccc3971ed4818719ef8a6ffd1ad3ac479a7:The quick brown fox jumps over the lazy dog"#,
            r#"cbc-export:000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f"#,
            r#"hkdf:17549b357089ac14572d4466381640cc27d6e2d88e06c83266de68c60ad55bfa1a68a49ea9c028819fce"#,
            r#"derived:17549b357089ac14572d4466381640cc27d6e2d88e06c83266de68c60ad55bfa:{"name":"AES-GCM","length":256}:encrypt,decrypt:secret:true"#,
            r#"msal:972a187fa7958a669e27e6ed6ddeb5a0779cc05deb2b4c"#,
            r#"gen:16:{"name":"AES-GCM","length":128}:encrypt,decrypt:[object CryptoKey]"#,
            r#"gen-back:The quick brown fox jumps over the lazy dog"#,
            r#"errors:InvalidAccessError,InvalidAccessError,OperationError,SyntaxError,SyntaxError,TypeError,DataError,SyntaxError,NotSupportedError,InvalidAccessError,OperationError,OperationError,OperationError,OperationError"#,
        ];
        let got: Vec<&str> = got.split('|').collect();
        assert_eq!(got, want);
    });
}

/// The methods that are not built still reject with `NotSupportedError`, and `jwk` import is not built.
#[test]
fn the_other_methods_and_formats_reject_with_not_supported() {
    big(|| {
        let mut b = mk(Some("https://example.com/".into()), FetchLimits::default());
        let got = go(&mut b, "var out = []; var c = crypto.subtle; var n = function (p) { return p.then(function () { return 'ok'; }, function (e) { return e.name; }); }; Promise.all([n(c.sign('ECDSA', null, new Uint8Array(1))), n(c.verify('ECDSA', null, new Uint8Array(1), new Uint8Array(1))), n(c.wrapKey('raw', null, null, 'AES-KW')), n(c.unwrapKey('raw', new Uint8Array(1), null, 'AES-KW', 'AES-GCM', true, ['encrypt'])), n(c.importKey('jwk', {}, 'AES-GCM', true, ['encrypt'])), n(c.generateKey({ name: 'RSASSA-PKCS1-v1_5' }, true, ['sign']))]).then(function (r) { out.push(r.join()); }); 1");
        assert_eq!(got, "NotSupportedError,NotSupportedError,NotSupportedError,NotSupportedError,NotSupportedError,NotSupportedError");
    });
}
