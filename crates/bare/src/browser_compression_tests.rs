//! Tests of `CompressionStream` and `DecompressionStream` (`shims/compression.js`). Real scripts run in a real realm, on a big stack.

use crate::browser::*;
use bare_html::tree;
use bare_js::eval::{Error, EvalLimits};
use bare_js::heap::HeapLimits;
use bare_js::value::Value;

fn big<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(64 << 20).spawn(f).unwrap().join().unwrap()
}

fn mk() -> Browser {
    Browser::new(tree::parse("<body></body>"), BrowserLimits::default(), EvalLimits { max_steps: 20_000_000, ..Default::default() }, HeapLimits::default()).expect("browser")
}

fn js(b: &mut Browser, src: &str) -> String {
    match b.run_script(src) {
        Ok(v) => b.it.display(v),
        Err(Error::Throw(v)) if matches!(v, Value::Object(_)) => format!("throw:{}:{}", b.it.get_str(v, "name"), b.it.get_str(v, "message")),
        Err(e) => format!("other:{e:?}"),
    }
}

/// Run a script, then the microtasks. Gives `out.join('|')`.
fn go(b: &mut Browser, src: &str) -> String {
    let r = js(b, src);
    assert!(!r.starts_with("throw") && !r.starts_with("other"), "{r}");
    b.drain().unwrap();
    js(b, "out.join('|')")
}

/// The helpers of the scripts: `bytes(stream)` reads a stream to one array; `pump(T, chunks)` writes the chunks through a transform.
const HELP: &str = "var out = [];
async function bytes(s) { const r = s.getReader(), parts = []; for (;;) { const { done, value } = await r.read(); if (done) break; parts.push(...value); } return parts; }
function pump(t, chunks) { const w = t.writable.getWriter(); for (const c of chunks) w.write(c).catch(() => {}); w.close().catch(() => {}); return bytes(t.readable); }
";

#[test]
fn each_format_round_trips_and_gzip_starts_with_its_magic() {
    big(|| {
        let mut b = mk();
        let src = format!(
            "{HELP}
(async () => {{
  out.push(typeof CompressionStream, typeof DecompressionStream, String(new CompressionStream('gzip')));
  const text = 'hello hello hello '.repeat(200), enc = new TextEncoder();
  for (const f of ['gzip', 'deflate', 'deflate-raw']) {{
    const z = await pump(new CompressionStream(f), [enc.encode(text.slice(0, 1000)), enc.encode(text.slice(1000))]);
    const back = await pump(new DecompressionStream(f), [new Uint8Array(z.slice(0, 5)), new Uint8Array(z.slice(5))]);
    out.push(f + ':' + (z.length < text.length) + ':' + (new TextDecoder().decode(new Uint8Array(back)) === text));
    if (f === 'gzip') out.push(z[0] + ',' + z[1]);
  }}
}})().catch(e => out.push('error:' + e));"
        );
        assert_eq!(go(&mut b, &src), "function|function|[object CompressionStream]|gzip:true:true|31,139|deflate:true:true|deflate-raw:true:true");
    });
}

#[test]
fn bytes_from_another_encoder_decompress() {
    big(|| {
        let mut b = mk();
        // `gzip.compress(b'hello', mtime=0)`, `zlib.compress(b'hello')` and raw deflate of the same, from Python.
        let src = format!(
            "{HELP}
(async () => {{
  const cases = [['gzip', [31,139,8,0,0,0,0,0,2,255,203,72,205,201,201,7,0,134,166,16,54,5,0,0,0]], ['deflate', [120,156,203,72,205,201,201,7,0,6,44,2,21]], ['deflate-raw', [203,72,205,201,201,7,0]]];
  for (const [f, z] of cases) out.push(f + ':' + new TextDecoder().decode(new Uint8Array(await pump(new DecompressionStream(f), [new Uint8Array(z)]))));
}})().catch(e => out.push('error:' + e));"
        );
        assert_eq!(go(&mut b, &src), "gzip:hello|deflate:hello|deflate-raw:hello");
    });
}

#[test]
fn a_bad_format_bad_data_or_trailing_bytes_are_type_errors() {
    big(|| {
        let mut b = mk();
        let src = format!(
            "{HELP}
try {{ new CompressionStream('br'); }} catch (e) {{ out.push(e.name); }}
try {{ new DecompressionStream(); }} catch (e) {{ out.push(e.name); }}
(async () => {{
  for (const z of [[], [1, 2, 3], [203,72,205,201,201,7,0,9], [203,72,205]]) {{
    try {{ await pump(new DecompressionStream('deflate-raw'), [new Uint8Array(z)]); out.push('no error'); }} catch (e) {{ out.push(e.name); }}
  }}
  try {{ await pump(new CompressionStream('gzip'), ['text']); out.push('no error'); }} catch (e) {{ out.push(e.name); }}
}})();"
        );
        assert_eq!(go(&mut b, &src), "TypeError|TypeError|TypeError|TypeError|TypeError|TypeError|TypeError");
    });
}

#[test]
fn a_blob_stream_piped_through_gzip_makes_a_response_body() {
    big(|| {
        let mut b = mk();
        // The shape of a web mail logger: it gzips a JSON body before the upload.
        let src = format!(
            "{HELP}
(async () => {{
  const body = JSON.stringify({{ events: new Array(500).fill('the same long experiment string') }});
  const buf = await new Response(new Blob([body]).stream().pipeThrough(new CompressionStream('gzip'))).arrayBuffer();
  const back = await new Response(new Blob([buf]).stream().pipeThrough(new DecompressionStream('gzip'))).text();
  out.push(buf.byteLength < body.length / 10, back === body);
}})().catch(e => out.push('error:' + e));"
        );
        assert_eq!(go(&mut b, &src), "true|true");
    });
}
