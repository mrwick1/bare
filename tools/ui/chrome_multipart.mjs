// Record the request bodies and Content-Type headers that Chrome sends for `fetch` with FormData, Blob and other body inits.
// Usage: BARE_TMP=/data/bare-tmp node tools/ui/chrome_multipart.mjs tools/fixtures/net/chrome-bodies.txt
// A local echo server keeps the raw bytes of each request. Chrome runs each case on a page served by that server.
// Output: one line per case, tab separated: name, body expression, caller headers (JSON), Content-Type Chrome sent,
// body. In the body, `{B}` stands for Chrome's multipart boundary; `\\`, `\r`, `\n` and `\t` are escapes.
import { createServer } from 'node:http';
import { writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { open } from './chrome_cdp.mjs';

const cases = [
  ['strings', `(() => { const f = new FormData(); f.append('a', '1'); f.append('b', 'two words ü'); return f; })()`],
  ['file', `(() => { const f = new FormData(); f.append('t', 'x'); f.append('file', new File(['hello\\n'], 'a.txt', { type: 'text/plain' })); return f; })()`],
  ['file-no-type', `(() => { const f = new FormData(); f.append('file', new File(['abc'], 'e.bin')); return f; })()`],
  ['blob-no-type', `(() => { const f = new FormData(); f.append('b', new Blob(['xyz'])); return f; })()`],
  ['blob-type-name', `(() => { const f = new FormData(); f.append('b', new Blob(['{}'], { type: 'application/json' }), 'j.json'); return f; })()`],
  ['quote-newline-names', `(() => { const f = new FormData(); f.append('a"b\\nc\\rd\\r\\ne', 'v'); f.append('f', new File(['x'], 'q"\\n\\r.txt')); return f; })()`],
  ['lone-cr-lf', `(() => { const f = new FormData(); f.append('v', 'a\\nb\\rc\\r\\nd\\n\\n'); return f; })()`],
  ['unicode-filename', `(() => { const f = new FormData(); f.append('f', new File(['x'], 'ü€.txt', { type: 'text/x-ü' })); return f; })()`],
  ['empty-form', `new FormData()`],
  ['set-order', `(() => { const f = new FormData(); f.append('a', '1'); f.append('b', '2'); f.append('a', '3'); f.set('a', '9'); return f; })()`],
  ['blob-body-type', `new Blob(['{"a":1}'], { type: 'Application/JSON' })`],
  ['blob-body-no-type', `new Blob(['raw'])`],
  ['file-body', `new File(['f'], 'n.txt', { type: 'text/plain' })`],
  ['blob-body-caller-type', `new Blob(['raw'], { type: 'a/b' })`, { 'Content-Type': 'x/y' }],
  ['form-caller-type', `(() => { const f = new FormData(); f.append('a', '1'); return f; })()`, { 'Content-Type': 'multipart/form-data; boundary=zzz' }],
  ['params', `new URLSearchParams({ a: '1 2', b: 'ü&=' })`],
  ['params-caller-type', `new URLSearchParams({ a: '1' })`, { 'content-type': 'x/y' }],
  ['uint8', `new Uint8Array([104, 105])`],
  ['arraybuffer', `new Uint8Array([104, 105, 106]).buffer`],
  ['string', `'plain ü'`],
];

const seen = [];
const srv = createServer((req, res) => {
  const parts = [];
  req.on('data', (c) => parts.push(c));
  req.on('end', () => {
    if (req.method === 'GET') { res.setHeader('content-type', 'text/html'); res.end('<!doctype html><title>x</title>'); return; }
    seen.push({ ct: req.headers['content-type'] ?? '', body: Buffer.concat(parts) });
    res.end('ok');
  });
});
await new Promise((r) => srv.listen(0, '127.0.0.1', r));
const port = srv.address().port;

const dir = process.env.BARE_TMP ?? '/tmp';
const file = join(dir, 'multipart-redirect.html');
writeFileSync(file, `<script>location = 'http://127.0.0.1:${port}/'</script>`);
const c = await open(file, 400, 300);
await c.sleep(1500);
const esc = (s) => s.replace(/\\/g, '\\\\').replace(/\r/g, '\\r').replace(/\n/g, '\\n').replace(/\t/g, '\\t');
const out = [];
for (const [name, expr, headers = {}] of cases) {
  seen.length = 0;
  const r = await c.eval(`fetch('/${name}', { method: 'POST', body: ${expr}, headers: ${JSON.stringify(headers)} }).then(r => r.text())`);
  if (r !== 'ok' || seen.length !== 1) throw new Error(`${name}: ${r} ${seen.length}`);
  let { ct, body } = seen[0];
  const text = body.toString('utf8');
  if (!Buffer.from(text, 'utf8').equals(body)) throw new Error(`${name}: body is not UTF-8`);
  // The boundary Chrome used is the first line of the body, after the two dashes of the delimiter.
  const m = /^--(----WebKitFormBoundary\w+)\r\n|^--(----WebKitFormBoundary\w+)--\r\n/.exec(text);
  const bound = m && (m[1] ?? m[2]);
  let b = text;
  if (bound) { b = b.split(bound).join('{B}'); ct = ct.replace(bound, '{B}'); }
  out.push([name, expr, JSON.stringify(headers), ct, esc(b)].join('\t'));
}
c.close();
srv.close();
writeFileSync(process.argv[2], out.join('\n') + '\n');
console.log(`wrote ${out.length} cases to ${process.argv[2]}`);
