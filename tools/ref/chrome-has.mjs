// Reads a gap census (`BARE_CENSUS`, lines TAG NAME get|has COUNT) from argv[2]. Prints the names that Chrome has on the same kind
// of object: COUNT TAG NAME KIND. Untagged objects (`?`) and modules are skipped.
import { createRequire } from 'module';
import { readFileSync } from 'fs';
const require = createRequire((process.env.PLAYWRIGHT_MODULES ?? process.env.HOME + '/.local/lib/browse/node_modules') + '/');
const { chromium } = require('playwright-core');
const max = new Map();
for (const l of readFileSync(process.argv[2], 'utf8').split('\n')) {
  const [tag, name, kind, n] = l.split('\t');
  if (!n || tag === '?' || tag === 'Module') continue;
  const k = `${tag}\t${name}\t${kind}`;
  max.set(k, Math.max(max.get(k) ?? 0, +n));
}
const rows = [...max].map(([k, n]) => [...k.split('\t'), n]);
const b = await chromium.launch({ executablePath: '/usr/bin/google-chrome-stable', headless: true });
const p = await b.newPage();
await p.goto('data:text/html,<body></body>');
const has = await p.evaluate(async (rows) => {
  const svg = { SVGSVGElement: 'svg', SVGPathElement: 'path', SVGGElement: 'g', SVGCircleElement: 'circle', SVGRectElement: 'rect', SVGEllipseElement: 'ellipse', SVGElement: 'desc' };
  const sample = (t) => {
    switch (t) {
      case 'Window': return window;
      case 'Document': return document;
      case 'Navigator': return navigator;
      case 'Screen': return screen;
      case 'Text': return document.createTextNode('');
      case 'Comment': return document.createComment('');
      case 'HTMLElement': return document.createElement('span');
      case 'HTMLBodyElement': return document.body;
      case 'HTMLHtmlElement': return document.documentElement;
      case 'DOMStringMap': return document.body.dataset;
      case 'Intl': return Intl;
      case 'Intl.ListFormat': return Intl.ListFormat.prototype;
    }
    if (svg[t]) return document.createElementNS('http://www.w3.org/2000/svg', svg[t]);
    const C = globalThis[t];
    return C && C.prototype;
  };
  const worker = rows.filter((r) => r[0] === 'DedicatedWorkerGlobalScope').map((r) => r[1]);
  const src = `onmessage = (e) => postMessage(e.data.map((n) => n in self))`;
  const w = new Worker(URL.createObjectURL(new Blob([src], { type: 'text/javascript' })));
  const inWorker = await new Promise((ok) => { w.onmessage = (e) => ok(e.data); w.postMessage(worker); });
  let i = 0;
  return rows.map(([t, n]) => {
    if (t === 'DedicatedWorkerGlobalScope') return inWorker[i++];
    const o = sample(t);
    return o == null ? null : n in Object(o);
  });
}, rows);
await b.close();
const out = rows.map((r, i) => [r[3], r[0], r[1], r[2], has[i]]).sort((a, b) => b[0] - a[0]);
for (const r of out) if (r[4] !== false) console.log(r.join('\t'));
