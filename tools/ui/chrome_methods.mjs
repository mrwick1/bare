// Record the request bytes that headless Chrome sends for `fetch` with each method, against a raw TCP server. The fixture is
// tools/fixtures/ui/methods-expected.txt: `case<TAB>request-line;content-length;content-type;body` for each request of the case, joined by ` || `.
// Only the fields that the Fetch standard decides are kept; Chrome adds many more headers (`sec-*`, `user-agent`, `accept-language`).
// The Rust test `browser_fetch_tests::methods_match_the_chrome_fixture` runs the same cases (tools/fixtures/ui/methods-cases.txt).
// Usage: node tools/ui/chrome_methods.mjs > tools/fixtures/ui/methods-expected.txt
import { createServer } from 'node:net';
import { readFileSync, writeFileSync, mkdtempSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';
import { open, sleep } from './chrome_cdp.mjs';

const here = dirname(fileURLToPath(import.meta.url));
const cases = readFileSync(join(here, '..', 'fixtures', 'ui', 'methods-cases.txt'), 'utf8').split('\n').filter((l) => l && !l.startsWith('#')).map((l) => l.split('\t'));

let seen = [];
const server = createServer((s) => {
  let buf = Buffer.alloc(0);
  s.on('data', (d) => {
    buf = Buffer.concat([buf, d]);
    for (;;) {
      const end = buf.indexOf('\r\n\r\n');
      if (end < 0) return;
      const head = buf.subarray(0, end).toString('latin1');
      const lines = head.split('\r\n');
      const h = (n) => lines.slice(1).map((l) => l.split(/:\s*/, 2)).find(([k]) => k.toLowerCase() === n)?.[1] ?? '';
      const len = Number(h('content-length') || 0);
      if (buf.length < end + 4 + len) return;
      const body = buf.subarray(end + 4, end + 4 + len).toString('latin1');
      buf = buf.subarray(end + 4 + len);
      const [method, path] = lines[0].split(' ');
      if (path !== '/page.html' && path !== '/favicon.ico') seen.push(`${lines[0].replace(/ HTTP.*/, '')};${h('content-length')};${h('content-type')};${body}`);
      const send = (status, extra, text, headOnly) => s.write(`HTTP/1.1 ${status} X\r\nContent-Length: ${Buffer.byteLength(text)}\r\n${extra}\r\n` + (headOnly ? '' : text));
      const code = /^\/r(\d+)/.exec(path)?.[1];
      if (path === '/page.html') send(200, 'Content-Type: text/html\r\n', '<!doctype html><body></body>');
      else if (code) send(Number(code), 'Location: /t\r\n', '');
      else if (path === '/empty') send(204, '', '');
      else send(200, 'Content-Type: text/plain\r\n', 'ok', method === 'HEAD');
    }
  });
});
await new Promise((r) => server.listen(0, '127.0.0.1', r));
const base = `http://127.0.0.1:${server.address().port}`;
const dir = mkdtempSync(join(process.env.BARE_TMP ?? tmpdir(), 'chrome-methods-'));
const blank = join(dir, 'blank.html');
writeFileSync(blank, '<!doctype html><body></body>');
const c = await open(blank, 400, 300);
for (const [name, src] of cases) {
  await c.send('Page.navigate', { url: base + '/page.html' });
  for (let i = 0; i < 100 && (await c.eval('document.readyState + location.pathname')) !== 'complete/page.html'; i++) await sleep(20);
  seen = [];
  const r = await c.eval(`(async () => { try { const r = await (${src}); return 'ok:' + r; } catch (e) { return e.name; } })()`);
  console.log(name + '\t' + r + '\t' + seen.join(' || '));
}
c.close();
server.close();
process.exit(0);
