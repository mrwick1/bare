//! Tests of `SubtleCrypto` signatures (`browser_subtle.rs`): RSASSA-PKCS1-v1_5, HMAC and ECDSA, the key formats "jwk", "spki" and
//! "pkcs8", the `CryptoKey` global and its structured clone. The fixture `browser_subtle_sign_fixture.js` holds keys and signatures
//! made by Node 22.23 (`crypto.subtle`) for the message below. RSASSA-PKCS1-v1_5 and HMAC are deterministic, so Bare must give the same bytes.

use crate::browser_fetch::FetchLimits;
use crate::browser_fetch_tests::{go, mk};

fn big<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(64 << 20).spawn(f).unwrap().join().unwrap()
}

/// Run `body` (the body of an async function that pushes to `out`) after the fixture and the helpers.
fn run(body: &str) -> Vec<String> {
    let src = format!(
        "{}\nvar out = []; var c = crypto.subtle;
var hex = function (b) {{ return Array.from(new Uint8Array(b)).map(function (x) {{ return x.toString(16).padStart(2, '0'); }}).join(''); }};
var unhex = function (h) {{ var r = new Uint8Array(h.length / 2); for (var i = 0; i < r.length; i++) r[i] = parseInt(h.substr(2 * i, 2), 16); return r; }};
var te = new TextEncoder(), msg = te.encode('The quick brown fox jumps over the lazy dog');
var fail = function (p) {{ return p.then(function () {{ return 'ok'; }}, function (e) {{ return e.name; }}); }};
(async function () {{ {body} }})().catch(function (e) {{ out.push('uncaught:' + e.name + ':' + e.message); }}); 1",
        include_str!("browser_subtle_sign_fixture.js")
    );
    let src = src.to_string();
    big(move || {
        let mut b = mk(Some("https://example.com/".into()), FetchLimits::default());
        go(&mut b, &src).split('|').map(str::to_string).collect()
    })
}

/// What `MSAL.js` (`BrowserCrypto`) does: a 2048-bit RSASSA-PKCS1-v1_5 pair, the public key as JWK, a signature and its check.
#[test]
fn msal_flow() {
    let got = run(r#"
  var kp = await c.generateKey({ name: 'RSASSA-PKCS1-v1_5', modulusLength: 2048, publicExponent: new Uint8Array([1, 0, 1]), hash: 'SHA-256' }, false, ['sign', 'verify']);
  out.push('types:' + kp.publicKey.type + ',' + kp.privateKey.type + ':' + kp.publicKey.extractable + ',' + kp.privateKey.extractable);
  out.push('alg:' + kp.privateKey.algorithm.name + ':' + kp.privateKey.algorithm.modulusLength + ':' + kp.privateKey.algorithm.hash.name + ':' + hex(kp.privateKey.algorithm.publicExponent));
  out.push('usages:' + kp.publicKey.usages.join() + ':' + kp.privateKey.usages.join());
  var jwk = await c.exportKey('jwk', kp.publicKey);
  out.push('jwk:' + jwk.kty + ':' + jwk.e + ':' + jwk.n.length + ':' + jwk.alg + ':' + jwk.ext + ':' + jwk.key_ops.join() + ':' + ('d' in jwk));
  var sig = await c.sign({ name: 'RSASSA-PKCS1-v1_5' }, kp.privateKey, msg);
  out.push('sig:' + sig.byteLength);
  out.push('verify:' + await c.verify('RSASSA-PKCS1-v1_5', kp.publicKey, sig, msg));
  out.push('verify-changed:' + await c.verify('RSASSA-PKCS1-v1_5', kp.publicKey, sig, te.encode('other')));
  var imp = await c.importKey('jwk', jwk, { name: 'RSASSA-PKCS1-v1_5', hash: 'SHA-256' }, true, ['verify']);
  out.push('imported:' + await c.verify('RSASSA-PKCS1-v1_5', imp, sig, msg));
  out.push('private-export:' + await fail(c.exportKey('jwk', kp.privateKey)));
  out.push('digest:' + hex(await c.digest('SHA-256', msg)).length);
"#);
    assert_eq!(
        got,
        [
            "types:public,private:true,false",
            "alg:RSASSA-PKCS1-v1_5:2048:SHA-256:010001",
            "usages:verify:sign",
            "jwk:RSA:AQAB:342:RS256:true:verify:false",
            "sig:256",
            "verify:true",
            "verify-changed:false",
            "imported:true",
            "private-export:InvalidAccessError",
            "digest:64",
        ]
    );
}

#[test]
fn rsa_signature_of_a_fixed_key_equals_node() {
    let got = run(r#"
  var alg = { name: 'RSASSA-PKCS1-v1_5', hash: 'SHA-256' };
  var priv = await c.importKey('jwk', RSA_PRIV, alg, true, ['sign']);
  var pub = await c.importKey('jwk', RSA_PUB, alg, true, ['verify']);
  var sig = await c.sign('RSASSA-PKCS1-v1_5', priv, msg);
  out.push('same:' + (hex(sig) === RSA_SIG));
  out.push('verify-node:' + await c.verify('RSASSA-PKCS1-v1_5', pub, unhex(RSA_SIG), msg));
  var bad = unhex(RSA_SIG); bad[10] ^= 1;
  out.push('verify-bad:' + await c.verify('RSASSA-PKCS1-v1_5', pub, bad, msg));
  out.push('verify-short:' + await c.verify('RSASSA-PKCS1-v1_5', pub, bad.subarray(1), msg));
  var j = await c.exportKey('jwk', priv);
  out.push('jwk:' + ['n', 'e', 'd', 'p', 'q', 'dp', 'dq', 'qi'].every(function (k) { return j[k] === RSA_PRIV[k]; }));
  out.push('spki:' + (hex(await c.exportKey('spki', pub)) === RSA_SPKI));
  out.push('pkcs8:' + (hex(await c.exportKey('pkcs8', priv)) === RSA_PKCS8));
  var p2 = await c.importKey('pkcs8', unhex(RSA_PKCS8), alg, false, ['sign']);
  out.push('pkcs8-sign:' + (hex(await c.sign('RSASSA-PKCS1-v1_5', p2, msg)) === RSA_SIG));
  var s2 = await c.importKey('spki', unhex(RSA_SPKI), alg, true, ['verify']);
  out.push('spki-verify:' + await c.verify('RSASSA-PKCS1-v1_5', s2, unhex(RSA_SIG), msg));
  var s384 = await c.importKey('jwk', RSA_PRIV, { name: 'RSASSA-PKCS1-v1_5', hash: 'SHA-384' }, true, ['sign']);
  out.push('sha384:' + (await c.sign('RSASSA-PKCS1-v1_5', s384, msg)).byteLength);
"#);
    assert_eq!(got, ["same:true", "verify-node:true", "verify-bad:false", "verify-short:false", "jwk:true", "spki:true", "pkcs8:true", "pkcs8-sign:true", "spki-verify:true", "sha384:256"]);
}

#[test]
fn hmac_equals_node() {
    let got = run(r#"
  var seq = function (n) { var r = new Uint8Array(n); for (var i = 0; i < n; i++) r[i] = i; return r; };
  var k = await c.importKey('raw', seq(20), { name: 'HMAC', hash: 'SHA-256' }, true, ['sign', 'verify']);
  var mac = await c.sign('HMAC', k, msg);
  out.push('256:' + (hex(mac) === HMAC256));
  out.push('verify:' + await c.verify('HMAC', k, mac, msg) + ':' + await c.verify('HMAC', k, mac, te.encode('x')));
  var k5 = await c.importKey('raw', seq(100), { name: 'HMAC', hash: { name: 'SHA-512' } }, false, ['sign']);
  out.push('512:' + (hex(await c.sign('HMAC', k5, msg)) === HMAC512));
  out.push('alg:' + JSON.stringify(k5.algorithm) + ':' + k5.type);
  var g = await c.generateKey({ name: 'HMAC', hash: 'SHA-1' }, true, ['sign', 'verify']);
  out.push('gen:' + g.algorithm.length + ':' + (await c.exportKey('raw', g)).byteLength + ':' + (await c.sign('HMAC', g, msg)).byteLength);
  var j = await c.exportKey('jwk', k);
  out.push('jwk:' + j.kty + ':' + j.alg + ':' + j.k);
  var kj = await c.importKey('jwk', j, { name: 'HMAC', hash: 'SHA-256' }, true, ['sign']);
  out.push('jwk-same:' + (hex(await c.sign('HMAC', kj, msg)) === HMAC256));
"#);
    assert_eq!(
        got,
        ["256:true", "verify:true:false", "512:true", r#"alg:{"name":"HMAC","hash":{"name":"SHA-512"},"length":800}:secret"#, "gen:512:64:20", "jwk:oct:HS256:AAECAwQFBgcICQoLDA0ODxAREhM", "jwk-same:true"]
    );
}

#[test]
fn ecdsa_round_trip_and_node_signature() {
    let got = run(r#"
  var a = { name: 'ECDSA', hash: 'SHA-256' };
  var kp = await c.generateKey({ name: 'ECDSA', namedCurve: 'P-256' }, true, ['sign', 'verify']);
  out.push('alg:' + JSON.stringify(kp.publicKey.algorithm));
  var sig = await c.sign(a, kp.privateKey, msg);
  out.push('sig:' + sig.byteLength + ':' + await c.verify(a, kp.publicKey, sig, msg) + ':' + await c.verify(a, kp.publicKey, sig, te.encode('x')));
  var pub = await c.importKey('jwk', EC_PUB, { name: 'ECDSA', namedCurve: 'P-256' }, true, ['verify']);
  out.push('node-sig:' + await c.verify(a, pub, unhex(EC_SIG), msg));
  var priv = await c.importKey('jwk', EC_PRIV, { name: 'ECDSA', namedCurve: 'P-256' }, true, ['sign']);
  out.push('own-sig:' + await c.verify(a, pub, await c.sign(a, priv, msg), msg));
  var j = await c.exportKey('jwk', priv);
  out.push('jwk:' + ['x', 'y', 'd', 'crv'].every(function (k) { return j[k] === EC_PRIV[k]; }));
  out.push('spki:' + (hex(await c.exportKey('spki', pub)) === EC_SPKI));
  var p8 = await c.importKey('pkcs8', unhex(EC_PKCS8), { name: 'ECDSA', namedCurve: 'P-256' }, true, ['sign']);
  var j8 = await c.exportKey('jwk', p8);
  out.push('pkcs8:' + (j8.d === EC_PRIV.d && j8.x === EC_PRIV.x));
  out.push('pkcs8-export:' + ((await c.exportKey('pkcs8', p8)).byteLength > 0));
  out.push('sha384:' + (await c.sign({ name: 'ECDSA', hash: 'SHA-384' }, priv, msg)).byteLength);
"#);
    assert_eq!(
        got,
        [r#"alg:{"name":"ECDSA","namedCurve":"P-256"}"#, "sig:64:true:false", "node-sig:true", "own-sig:true", "jwk:true", "spki:true", "pkcs8:true", "pkcs8-export:true", "sha384:64"]
    );
}

#[test]
fn crypto_key_is_a_global_and_survives_structured_clone() {
    let got = run(r#"
  out.push('global:' + typeof CryptoKey + ':' + Object.prototype.toString.call(CryptoKey.prototype));
  var k = await c.importKey('raw', new Uint8Array(20), { name: 'HMAC', hash: 'SHA-256' }, false, ['sign']);
  out.push('instanceof:' + (k instanceof CryptoKey));
  try { new CryptoKey(); out.push('new:ok'); } catch (e) { out.push('new:' + e.name); }
  var priv = await c.importKey('jwk', RSA_PRIV, { name: 'RSASSA-PKCS1-v1_5', hash: 'SHA-256' }, false, ['sign']);
  var copy = structuredClone(priv);
  out.push('clone:' + (copy !== priv) + ':' + (copy instanceof CryptoKey) + ':' + copy.type + ':' + copy.extractable + ':' + copy.algorithm.name + ':' + copy.usages.join());
  out.push('clone-sign:' + (hex(await c.sign('RSASSA-PKCS1-v1_5', copy, msg)) === RSA_SIG));
  var kp = await c.generateKey({ name: 'ECDSA', namedCurve: 'P-256' }, false, ['sign', 'verify']);
  var pair = structuredClone({ a: kp.publicKey, b: kp.privateKey });
  var sig = await c.sign({ name: 'ECDSA', hash: 'SHA-256' }, pair.b, msg);
  out.push('clone-ec:' + await c.verify({ name: 'ECDSA', hash: 'SHA-256' }, pair.a, sig, msg));
"#);
    assert_eq!(got, ["global:function:[object CryptoKey]", "instanceof:true", "new:TypeError", "clone:true:true:private:false:RSASSA-PKCS1-v1_5:sign", "clone-sign:true", "clone-ec:true"]);
}

#[test]
fn mismatches_and_unsupported_algorithms() {
    let got = run(r#"
  var rs = { name: 'RSASSA-PKCS1-v1_5', hash: 'SHA-256' };
  var pub = await c.importKey('jwk', RSA_PUB, rs, true, ['verify']);
  var priv = await c.importKey('jwk', RSA_PRIV, rs, true, ['sign']);
  var h = await c.importKey('raw', new Uint8Array(20), { name: 'HMAC', hash: 'SHA-256' }, true, ['sign', 'verify']);
  var aes = await c.generateKey({ name: 'AES-GCM', length: 128 }, true, ['encrypt']);
  out.push([
    await fail(c.sign('RSASSA-PKCS1-v1_5', pub, msg)),
    await fail(c.verify('RSASSA-PKCS1-v1_5', priv, new Uint8Array(256), msg)),
    await fail(c.sign('HMAC', priv, msg)),
    await fail(c.sign('RSASSA-PKCS1-v1_5', h, msg)),
    await fail(c.sign('HMAC', aes, msg)),
    await fail(c.sign({ name: 'ECDSA', hash: 'SHA-256' }, priv, msg)),
    await fail(c.sign('HMAC', {}, msg)),
    await fail(c.importKey('jwk', RSA_PRIV, rs, true, ['verify'])),
    await fail(c.importKey('jwk', RSA_PUB, rs, true, ['sign'])),
    await fail(c.importKey('jwk', Object.assign({}, RSA_PUB, { alg: 'RS384' }), rs, true, ['verify'])),
    await fail(c.importKey('jwk', Object.assign({}, RSA_PUB, { ext: false }), rs, true, ['verify'])),
    await fail(c.importKey('jwk', EC_PUB, rs, true, ['verify'])),
    await fail(c.importKey('spki', new Uint8Array(10), rs, true, ['verify'])),
    await fail(c.generateKey({ name: 'RSASSA-PKCS1-v1_5', modulusLength: 2048, publicExponent: new Uint8Array([1, 0, 1]), hash: 'SHA-256' }, true, ['encrypt'])),
    await fail(c.generateKey({ name: 'RSASSA-PKCS1-v1_5', modulusLength: 2048, publicExponent: new Uint8Array([1, 0, 1]), hash: 'MD5' }, true, ['sign'])),
    await fail(c.generateKey({ name: 'ECDSA', namedCurve: 'P-521' }, true, ['sign'])),
    await fail(c.generateKey({ name: 'HMAC', hash: 'SHA-256' }, true, ['encrypt'])),
    await fail(c.sign('RSA-FOO', priv, msg)),
    await fail(c.generateKey({ name: 'Ed25519' }, true, ['sign'])),
  ].join());
"#);
    assert_eq!(
        got,
        ["InvalidAccessError,InvalidAccessError,InvalidAccessError,InvalidAccessError,InvalidAccessError,InvalidAccessError,TypeError,SyntaxError,SyntaxError,DataError,DataError,DataError,DataError,SyntaxError,NotSupportedError,NotSupportedError,SyntaxError,NotSupportedError,NotSupportedError"]
    );
}
