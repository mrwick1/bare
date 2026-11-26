//! Tests of a media upload from a dedicated worker, in the shape that WhatsApp Web uses (`WAWebHttpExtendedFetch`,
//! `WAWebMmsClientMmsUpload`). The page posts a Blob to the worker. The worker reads it, encrypts it with AES-CBC, cuts the IV, adds
//! 10 bytes of an HMAC, and hashes the result with SHA-256. Then it sends that `ArrayBuffer` with `XMLHttpRequest`: method `post`, an
//! upload `progress` listener, no request headers, `responseType = "arraybuffer"`. The URL path carries the hash. The server answers
//! 415 when the SHA-256 of the body is not that hash. Expectations come from the Fetch and XHR Standards ("extract a body": no
//! `Content-Type` for a buffer source; an `Origin` header for a POST).

use crate::browser_fetch_tests::{Rec, big, reply, serve};
use crate::browser_session::*;
use crate::js_limits::EvalLimits;
use std::time::Duration;

const WORKER: &str = r#"
function cat(parts) { var n = 0; parts.forEach(function (p) { n += p.length }); var o = new Uint8Array(n), at = 0; parts.forEach(function (p) { o.set(p, at); at += p.length }); return o }
function hex(u) { return Array.from(u, function (b) { return (b < 16 ? '0' : '') + b.toString(16) }).join('') }
onmessage = async function (e) {
  try {
    var plain = new Uint8Array(await e.data.arrayBuffer());
    var iv = new Uint8Array(16), enc = new Uint8Array(32), mac = new Uint8Array(32);
    for (var i = 0; i < 32; i++) { enc[i] = i + 1; mac[i] = 100 + i; if (i < 16) iv[i] = 200 + i }
    var key = await crypto.subtle.importKey('raw', enc, 'AES-CBC', false, ['encrypt']);
    var ct = await crypto.subtle.encrypt({ name: 'AES-CBC', iv: iv }, key, plain);
    var ivct = cat([iv, new Uint8Array(ct)]).buffer;
    var hk = await crypto.subtle.importKey('raw', mac, { name: 'HMAC', hash: 'SHA-256' }, false, ['sign']);
    var sig = (await crypto.subtle.sign('HMAC', hk, ivct)).slice(0, 10);
    var k = new Uint8Array(ivct).subarray(iv.byteLength);
    var body = cat([k, new Uint8Array(sig)]).buffer;
    var h = btoa(String.fromCharCode.apply(String, new Uint8Array(await crypto.subtle.digest({ name: 'SHA-256' }, body))));
    var x = new XMLHttpRequest();
    x.upload.onprogress = function () {};
    x.onreadystatechange = function () { if (x.readyState === 4) postMessage(x.status + ' ' + h + ' ' + hex(new Uint8Array(body))) };
    x.withCredentials = false;
    x.open('post', TARGET + '/mms/document/' + h.replace(/\//g, '_').replace(/\+/g, '-') + '?auth=a&token=t&media_id=1', true);
    x.responseType = 'arraybuffer';
    x.send(body);
  } catch (err) { postMessage('error ' + err) }
};
"#;

/// Run the upload with the page and the worker on one server and the upload target on `target` (`None`: the same server). Returns
/// what the worker posts and the upload request that the target got.
fn upload(cross: bool) -> (Option<String>, Option<Rec>, u16) {
    let tgt = serve(|r: &Rec| {
        let cors = [("Access-Control-Allow-Origin", "*"), ("Access-Control-Allow-Methods", "POST"), ("Content-Type", "application/json")];
        if r.method == "OPTIONS" { reply(204, &cors, b"") } else { reply(200, &cors, b"{\"url\":\"u\",\"direct_path\":\"d\"}") }
    });
    let tport = tgt.port;
    let page = serve(move |r: &Rec| {
        let target = if cross { format!("http://localhost:{tport}") } else { String::new() };
        match r.path.as_str() {
            "/" => reply(200, &[("Content-Type", "text/html; charset=utf-8")], b"<script>var w = new Worker('/w.js'); w.onmessage = function (e) { localStorage.setItem('log', e.data) }; w.postMessage(new Blob(['twenty-one byte file\\n'], { type: 'text/plain' }))</script>"),
            "/w.js" => reply(200, &[("Content-Type", "text/javascript")], format!("var TARGET = {target:?};{WORKER}").as_bytes()),
            p if p.starts_with("/mms/") => reply(200, &[("Content-Type", "application/json")], b"{\"url\":\"u\",\"direct_path\":\"d\"}"),
            _ => reply(404, &[("Content-Type", "text/plain")], b"no"),
        }
    });
    let base = format!("http://127.0.0.1:{}", page.port);
    let mut c = SessionConfig::new(&format!("{base}/"));
    c.virtual_clock = true;
    c.eval = EvalLimits { max_steps: 50_000_000, ..Default::default() };
    c.page.quiet = Duration::from_millis(300);
    let mut s = Session::new(c);
    s.run();
    let log = s.storage().get_item(false, &base, "log");
    let srv = if cross { &tgt } else { &page };
    let rec = srv.recs().into_iter().find(|r| r.method.eq_ignore_ascii_case("post"));
    (log, rec, page.port)
}

fn check(cross: bool) {
    let (log, rec, port) = big(move || upload(cross));
    let log = log.expect("the worker posts its result");
    let mut f = log.split(' ');
    let (status, hash, hexed) = (f.next().unwrap(), f.next().unwrap_or(""), f.next().unwrap_or(""));
    assert_eq!(status, "200", "worker result: {log}");
    let rec = rec.expect("the target gets a POST");
    let got: String = rec.body.iter().map(|b| format!("{b:02x}")).collect();
    // The plaintext is 21 bytes: AES-CBC makes 32, then 10 bytes of HMAC.
    assert_eq!(hexed.len(), 84, "the worker hashed 42 bytes");
    assert_eq!(got, hexed, "the server gets the bytes that the worker hashed");
    assert_eq!(rec.header("content-length"), vec!["42".to_string()]);
    assert!(rec.header("content-type").is_empty(), "a buffer body has no Content-Type: {:?}", rec.header("content-type"));
    assert!(rec.path.contains(&hash.replace('/', "_").replace('+', "-")), "the path has the hash: {}", rec.path);
    assert_eq!(rec.header("origin"), vec![format!("http://127.0.0.1:{port}")]);
}

#[test]
fn a_worker_uploads_an_encrypted_buffer_to_its_origin() {
    check(false);
}

#[test]
fn a_worker_uploads_an_encrypted_buffer_to_another_origin() {
    check(true);
}
