// Run the cases of tools/fixtures/ui/<topic>-cases.txt in headless Chrome and write tools/fixtures/ui/chrome-<topic>.txt (`name<TAB>result`).
// Usage: node tools/ui/chrome_topic.mjs raf mutation intersection walker range element
// Lines before the first case are a prelude that every script starts with. A case is `=== name`, then optional lines `@html MARKUP` (the body) and `@css RULES`, then a script. The last expression of the script is
// a reader function. The case runs in a fresh 400x300 iframe (own realm, standards mode, no scrollbars). After the script the page waits
// 400 ms for frames and timers, then the reader runs and its result is turned into a string. A throw gives `throw:Name:message`.
// The bare test (`crates/bare/src/browser_topic_tests.rs`) runs the same cases on the same markup and compares.
import { mkdtempSync, readFileSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { open } from './chrome_cdp.mjs';

const esc = (s) => String(s).replace(/\\/g, '\\\\').replace(/\n/g, '\\n').replace(/\r/g, '\\r').replace(/\t/g, '\\t');
const dir = mkdtempSync(join(process.env.BARE_TMP ?? tmpdir(), 'chrome-topic-'));
const blank = join(dir, 'blank.html');
writeFileSync(blank, '<!doctype html><body></body>');
const c = await open(blank, 400, 300);
await c.eval(`window.__run = async (html, css, src) => {
  const f = document.createElement('iframe');
  f.style.cssText = 'position:absolute;left:0;top:0;width:400px;height:300px;border:0';
  f.srcdoc = '<!doctype html><html><head><style>' + css + '</style></head><body>' + html + '</body></html>';
  document.body.appendChild(f);
  await new Promise((r) => (f.onload = r));
  const w = f.contentWindow; let out;
  try {
    const reader = w.eval(src);
    await new Promise((r) => setTimeout(r, 400));
    out = String(reader());
  } catch (e) {
    out = e !== null && typeof e === 'object' ? 'throw:' + e.name + ':' + e.message : 'throw-value:' + e;
  }
  f.remove();
  return out;
}`);
for (const topic of process.argv.slice(2)) {
  const cases = [], pre = [];
  for (const line of readFileSync(`tools/fixtures/ui/${topic}-cases.txt`, 'utf8').split('\n')) {
    if (line.startsWith('=== ')) cases.push({ name: line.slice(4).trim(), html: '', css: '', lines: [...pre] });
    else if (!cases.length) pre.push(line);
    else if (line.startsWith('@html ')) cases.at(-1).html += line.slice(6);
    else if (line.startsWith('@css ')) cases.at(-1).css += line.slice(5);
    else cases.at(-1).lines.push(line);
  }
  const out = [];
  for (const k of cases) {
    const r = await c.eval(`__run(${JSON.stringify(k.html)}, ${JSON.stringify(k.css)}, ${JSON.stringify(k.lines.join('\n').replace(/\n+$/, ''))})`);
    out.push(`${k.name}\t${esc(r)}`);
  }
  writeFileSync(`tools/fixtures/ui/chrome-${topic}.txt`, out.join('\n') + '\n');
  console.log(`${topic}: ${cases.length} cases`);
}
c.close();
process.exit(0);
