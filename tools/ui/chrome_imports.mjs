// Record what headless Chrome answers for the `@import` cases of tools/fixtures/ui/imports-cases.txt: each case has `@file PATH` blocks
// (`/page.html` is the page) and a last `@read id id` line. Output: `name<TAB>id=direction,id=direction`, the format that the
// `imports` tests of the bare driver compare with. Usage: node tools/ui/chrome_imports.mjs tools/fixtures/ui/imports-cases.txt > tools/fixtures/ui/imports-chrome.txt
import { readFileSync, writeFileSync } from 'node:fs';
import { createServer } from 'node:http';
import { join } from 'node:path';
import { tmpdir } from 'node:os';
import { open, sleep } from './chrome_cdp.mjs';

const cases = [];
for (const line of readFileSync(process.argv[2], 'utf8').split('\n')) {
  if (line.startsWith('=== ')) cases.push({ name: line.slice(4).trim(), files: {}, cur: null, read: [] });
  else if (!cases.length) continue;
  else if (line.startsWith('@file ')) { const k = cases.at(-1); k.cur = line.slice(6); k.files[k.cur] = ''; }
  else if (line.startsWith('@read ')) { const k = cases.at(-1); k.read = line.slice(6).trim().split(' '); k.cur = null; }
  else if (cases.at(-1).cur) cases.at(-1).files[cases.at(-1).cur] += line + '\n';
}
let files = {};
const srv = createServer((q, r) => {
  const t = files[q.url];
  if (t === undefined) { r.writeHead(404, { 'content-type': 'text/plain' }); r.end('no'); return; }
  r.writeHead(200, { 'content-type': q.url.endsWith('.css') ? 'text/css' : 'text/html' });
  r.end(t);
});
await new Promise((r) => srv.listen(0, '127.0.0.1', r));
const base = `http://127.0.0.1:${srv.address().port}`;
const blank = join(process.env.BARE_TMP ?? tmpdir(), 'chrome-imports-blank.html');
writeFileSync(blank, '<!doctype html><body></body>');
const c = await open(blank, 400, 300);
for (const k of cases) {
  files = k.files;
  await c.send('Page.navigate', { url: base + '/page.html' });
  await sleep(600);
  const v = await c.eval(`${JSON.stringify(k.read)}.map((i) => i + '=' + getComputedStyle(document.getElementById(i)).direction).join()`);
  console.log(k.name + '\t' + v);
}
c.close();
srv.close();
process.exit(0);
