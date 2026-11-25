//! `deriveKey` from an HKDF key to an HMAC key. The WhatsApp Web search worker derives its index HMAC key this way. The
//! derived bytes are the RFC 5869 test case 1 OKM. Without `length`, the key is one block of the hash long, and a shorter
//! HKDF output is a prefix of a longer one.

use crate::browser_fetch::FetchLimits;
use crate::browser_fetch_tests::{go, mk};

const SCRIPT: &str = r#"var out = []; var c = crypto.subtle;
var hex = function (b) { return Array.from(new Uint8Array(b)).map(function (x) { return x.toString(16).padStart(2, '0'); }).join(''); };
var ikm = new Uint8Array(22).fill(11), salt = new Uint8Array(13), info = new Uint8Array(10);
for (var i = 0; i < 13; i++) salt[i] = i;
for (var i = 0; i < 10; i++) info[i] = 0xf0 + i;
var p = { name: 'HKDF', hash: 'SHA-256', salt: salt, info: info };
(async function () {
  var k = await c.importKey('raw', ikm, { name: 'HKDF' }, false, ['deriveKey']);
  var a = await c.deriveKey(p, k, { name: 'HMAC', hash: 'SHA-256', length: 336 }, true, ['sign']);
  out.push('336:' + hex(await c.exportKey('raw', a)) + ':' + JSON.stringify(a.algorithm) + ':' + a.usages);
  var b = await c.deriveKey(p, k, { name: 'HMAC', hash: { name: 'SHA-256' } }, true, ['sign', 'verify']);
  var raw = await c.exportKey('raw', b);
  out.push('default:' + raw.byteLength + ':' + hex(raw).slice(0, 84) + ':' + b.algorithm.length);
  var w = await c.deriveKey(p, k, { name: 'HMAC', hash: 'SHA-256' }, false, ['sign']);
  var m = new TextEncoder().encode('index');
  var plain = await c.importKey('raw', raw, { name: 'HMAC', hash: 'SHA-256' }, false, ['sign']);
  out.push('sign:' + (hex(await c.sign('HMAC', w, m)) === hex(await c.sign('HMAC', plain, m))));
  var e = await c.deriveKey(p, k, { name: 'HMAC', hash: 'SHA-512' }, true, ['sign']);
  out.push('512:' + JSON.stringify(e.algorithm));
  try { await c.deriveKey(p, k, { name: 'HMAC', hash: 'SHA-256' }, false, ['encrypt']); out.push('bad:none'); } catch (x) { out.push('bad:' + x.name); }
})().catch(function (e) { out.push('uncaught:' + e.name + ':' + e.message); });
"#;

#[test]
fn hkdf_derives_hmac_key() {
    std::thread::Builder::new()
        .stack_size(64 << 20)
        .spawn(|| {
            let mut b = mk(Some("https://example.com/".into()), FetchLimits::default());
            let got = go(&mut b, SCRIPT);
            let okm = "3cb25f25faacd57a90434f64d0362f2a2d2d0a90cf1a5a4c5db02d56ecc4c5bf34007208d5b887185865";
            let want = [
                format!(r#"336:{okm}:{{"name":"HMAC","hash":{{"name":"SHA-256"}},"length":336}}:sign"#),
                format!("default:64:{okm}:512"),
                "sign:true".to_string(),
                r#"512:{"name":"HMAC","hash":{"name":"SHA-512"},"length":1024}"#.to_string(),
                "bad:SyntaxError".to_string(),
            ];
            let got: Vec<&str> = got.split('|').collect();
            assert_eq!(got, want);
        })
        .unwrap()
        .join()
        .unwrap();
}
