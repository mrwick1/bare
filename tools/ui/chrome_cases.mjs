// Record what headless Chrome answers for the cases of a DOM-side case file (same format as tools/fixtures/js/dom-cases.txt):
// each case is a script whose last expression is a reader function. A case runs in a fresh iframe (its own realm). After the
// script, the page waits 400 ms for timers, then the reader runs. Output: `name<TAB>result`, the format of the `dom_check` example.
// Only our own tiny programs run here. Usage: node tools/ui/chrome_cases.mjs tools/fixtures/js/dom-cases.txt > dom-cases.expected
import { mkdtempSync, readFileSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { open, sleep } from './chrome_cdp.mjs';

const cases = [];
for (const line of readFileSync(process.argv[2], 'utf8').split('\n')) {
  if (line.startsWith('=== ')) cases.push({ name: line.slice(4).trim(), lines: [] });
  else if (cases.length) cases[cases.length - 1].lines.push(line);
}
const esc = (s) => String(s).replace(/\\/g, '\\\\').replace(/\n/g, '\\n').replace(/\r/g, '\\r').replace(/\t/g, '\\t');
const dir = mkdtempSync(join(process.env.BARE_TMP ?? tmpdir(), 'chrome-cases-'));
const page = join(dir, 'blank.html');
writeFileSync(page, '<!doctype html><body></body>');
const c = await open(page, 400, 300);
await c.eval(`window.__run = async (src, wait) => {
  const f = document.createElement('iframe'); document.body.appendChild(f);
  const w = f.contentWindow; let out;
  try {
    const reader = w.eval(src);
    await new Promise((r) => setTimeout(r, wait));
    const v = reader();
    out = String(v);
  } catch (e) {
    out = e !== null && typeof e === 'object' ? 'throw:' + e.name + ':' + e.message : 'throw-value:' + e;
  }
  f.remove();
  return out;
}`);
for (const k of cases) {
  const src = k.lines.join('\n').replace(/\n+$/, '');
  const r = await c.eval(`__run(${JSON.stringify(src)}, 400)`);
  console.log(k.name + '\t' + esc(r));
}
c.close();
process.exit(0);
