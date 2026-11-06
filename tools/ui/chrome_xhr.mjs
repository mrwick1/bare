// Record what headless Chrome does for the cases of tools/fixtures/ui/xhr-cases.txt (XMLHttpRequest), against a small local server.
// Output: `name<TAB>result` lines, the format of tools/fixtures/ui/xhr-expected.txt. The Rust test `browser_xhr_tests` runs the same
// cases against the same endpoints and compares. Only our own tiny programs run here.
// Usage: node tools/ui/chrome_xhr.mjs > tools/fixtures/ui/xhr-expected.txt
import { createServer } from 'node:http';
import { readFileSync, writeFileSync, mkdtempSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';
import { open, sleep } from './chrome_cdp.mjs';

const here = dirname(fileURLToPath(import.meta.url));
const fx = join(here, '..', 'fixtures', 'ui');
const prelude = readFileSync(join(fx, 'xhr-prelude.js'), 'utf8');
const cases = [];
for (const line of readFileSync(join(fx, 'xhr-cases.txt'), 'utf8').split('\n')) {
  if (line.startsWith('=== ')) cases.push({ name: line.slice(4).trim(), lines: [] });
  else if (cases.length) cases[cases.length - 1].lines.push(line);
}

// The endpoints. The Rust server in browser_xhr_tests.rs answers the same way.
const server = createServer((req, res) => {
  const url = req.url.split('?')[0];
  let body = '';
  req.on('data', (c) => (body += c));
  req.on('end', () => {
    res.sendDate = false;
    const send = (status, headers, text) => { res.writeHead(status, { ...headers, 'Content-Length': Buffer.byteLength(text) }); res.end(text); };
    if (url === '/page.html') return send(200, { 'Content-Type': 'text/html' }, '<!doctype html><body></body>');
    if (url === '/text') return send(200, { 'Content-Type': 'text/plain; charset=utf-8', 'X-Custom': 'abc' }, 'hello');
    if (url === '/json') return send(200, { 'Content-Type': 'application/json' }, '{"a":1,"b":[true,null]}');
    if (url === '/badjson') return send(200, { 'Content-Type': 'application/json' }, '{oops');
    if (url === '/echo') return send(200, { 'Content-Type': 'text/plain' }, JSON.stringify({ m: req.method, ct: req.headers['content-type'] ?? '', x: req.headers['x-test'] ?? '', b: body }));
    if (url === '/accept') return send(200, { 'Content-Type': 'text/plain' }, req.headers.accept ?? '');
    if (url === '/missing') return send(404, { 'Content-Type': 'text/plain' }, 'nope');
    if (url === '/reset') return req.socket.destroy();
    if (url === '/slow') return void setTimeout(() => send(200, { 'Content-Type': 'text/plain' }, 'late'), 300);
    if (url === '/bin') return send(200, { 'Content-Type': 'application/octet-stream' }, Buffer.from([0, 1, 2, 253, 254, 255]));
    if (url === '/headers') {
      res.writeHead(200, ['Content-Type', 'text/plain', 'X-Dup', 'a', 'X-Dup', 'b', 'x-mixed-Case', 'v', 'Set-Cookie', 's=1', 'Content-Length', '1']);
      return res.end('h');
    }
    if (url === '/redir') return send(302, { Location: '/text' }, '');
    if (url === '/empty') return send(204, {}, '');
    if (url === '/len') return send(200, { 'Content-Type': 'text/plain' }, '0123456789');
    if (url === '/chunked') { res.writeHead(200, { 'Content-Type': 'text/plain' }); res.write('abc'); return res.end('def'); }
    if (url === '/utf8') return send(200, { 'Content-Type': 'text/plain; charset=utf-8' }, 'héllo 世界');
    send(404, { 'Content-Type': 'text/plain' }, 'unknown');
  });
});
await new Promise((r) => server.listen(0, '127.0.0.1', r));
const base = `http://127.0.0.1:${server.address().port}`;

const dir = mkdtempSync(join(process.env.BARE_TMP ?? tmpdir(), 'chrome-xhr-'));
const blank = join(dir, 'blank.html');
writeFileSync(blank, '<!doctype html><body></body>');
const c = await open(blank, 400, 300);
const esc = (s) => String(s).replace(/\\/g, '\\\\').replace(/\n/g, '\\n').replace(/\r/g, '\\r').replace(/\t/g, '\\t');
for (const k of cases) {
  await c.send('Page.navigate', { url: base + '/page.html' });
  for (let i = 0; i < 100 && (await c.eval('document.readyState + location.pathname')) !== 'complete/page.html'; i++) await sleep(20);
  const src = k.lines.join('\n').replace(/\n+$/, '');
  await c.eval(`(0, eval)(${JSON.stringify(prelude + '\n' + src)})`);
  let r;
  for (let i = 0; i < 200; i++) {
    r = await c.eval('finished ? out.join("|") : null');
    if (r !== null) break;
    await sleep(25);
  }
  if (r === null) r = 'TIMEOUT:' + (await c.eval('out.join("|")'));
  console.log(k.name + '\t' + esc(r));
}
c.close();
server.close();
process.exit(0);
