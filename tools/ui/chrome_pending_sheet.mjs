// Record what Chrome does with `getComputedStyle` while a style sheet is still loading.
// Usage: node tools/ui/chrome_pending_sheet.mjs tools/fixtures/ui/chrome-pending-sheets.txt
// A local HTTP server serves each case and a CSS file after a delay (`/s.css?c=R,G,B&d=MS&k=KEY`: `#t{color:rgb(R,G,B)}`).
// Headless Chrome loads each case over the DevTools protocol. The page logs what it reads. The script prints the log with
// the time of each event and the time at which the server sent each CSS response (ms since the page was requested).
import { spawn } from 'node:child_process';
import { mkdtempSync, writeFileSync } from 'node:fs';
import { createServer } from 'node:http';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

// `R()` reads: the color of #t, and how many sheets the document lists.
const HEAD = `<script>
window.__log = [];
window.__t0 = Date.now();
const L = (s) => __log.push((Date.now() - __t0) + 'ms ' + s);
const R = (label) => L(label + ': ' + getComputedStyle(document.getElementById('t')).color + ' sheets=' + document.styleSheets.length);
const mk = (href, extra = {}) => { const l = document.createElement('link'); l.rel = 'stylesheet'; l.href = href; Object.assign(l, extra); l.addEventListener('load', () => L('load event ' + href.slice(0, 22))); l.addEventListener('error', () => L('error event ' + href.slice(0, 22))); return l; };
</script>`;
const BODY = '<div id="t">x</div>';
const S = (k, c, d = 400) => `/s.css?c=${c}&d=${d}&k=${k}`;
const cases = {
  // A script inserts a link. Read in the same task, in a microtask, in a timer, in `load` of the link, in `load` of the window.
  'A1 script-inserted link, slow': `${HEAD}${BODY}<script>
    const l = mk('${S('a1', '1,2,3')}'); document.head.appendChild(l);
    R('same task'); L('link.sheet is ' + (l.sheet === null ? 'null' : 'set'));
    Promise.resolve().then(() => R('microtask'));
    setTimeout(() => R('timer 0'), 0);
    l.addEventListener('load', () => { R('in link load'); L('link.sheet is ' + (l.sheet === null ? 'null' : 'set')); });
    addEventListener('load', () => R('window load'));
    setTimeout(() => R('at 900ms'), 900);
  </script>`,
  // The link is in the DOM but detached first, then attached: same result expected.
  'A2 script-inserted link, 404': `${HEAD}${BODY}<script>
    const l = mk('/missing.css?k=a2'); document.head.appendChild(l);
    R('same task'); setTimeout(() => R('at 600ms'), 600);
  </script>`,
  // Same URL as an earlier loaded sheet: the memory cache may answer at once.
  'A3 script-inserted link, URL already loaded by a link': `${HEAD}${BODY}<link rel="stylesheet" href="${S('a3', '1,2,3', 50)}"><script>
    R('first script');
    document.getElementById('t').id = 'u';
    const l = mk('${S('a3', '1,2,3', 50)}'); document.head.appendChild(l); document.getElementById('u').id = 't';
    R('same task, same URL again'); Promise.resolve().then(() => R('microtask')); setTimeout(() => R('timer 0'), 0);
  </script>`,
  'A4 script-inserted link, URL already loaded by fetch': `${HEAD}${BODY}<script>
    fetch('${S('a4', '1,2,3', 100)}').then((r) => r.text()).then(() => {
      L('fetch done');
      const l = mk('${S('a4', '1,2,3', 100)}'); document.head.appendChild(l); R('same task after fetch');
      setTimeout(() => R('timer 0'), 0); setTimeout(() => R('at +200ms'), 200);
    });
  </script>`,
  // The link loaded once and a script changes its href.
  'B1 href changed on a loaded link': `${HEAD}${BODY}<link id="k" rel="stylesheet" href="${S('b1a', '1,2,3', 50)}"><script>
    const k = document.getElementById('k'); R('first script');
    k.href = '${S('b1b', '4,5,6', 400)}';
    R('same task after href change'); L('link.sheet is ' + (k.sheet === null ? 'null' : 'set'));
    Promise.resolve().then(() => R('microtask')); setTimeout(() => R('timer 0'), 0);
    k.addEventListener('load', () => R('in link load'));
    setTimeout(() => R('at 900ms'), 900);
  </script>`,
  // A link without a sheet yet (the old href never loaded): href changes.
  'B2 script sets href on a link made earlier without href': `${HEAD}${BODY}<script>
    const l = mk(''); l.removeAttribute('href'); document.head.appendChild(l); R('no href');
    l.href = '${S('b2', '1,2,3', 400)}'; R('same task after href'); setTimeout(() => R('at 900ms'), 900);
  </script>`,
  // A parser-inserted link in head, then an inline script.
  'C1 parser-inserted link in head, then inline script': `<head><script>window.__log=[];window.__t0=Date.now();const L=(s)=>__log.push((Date.now()-__t0)+'ms '+s);const R=(label)=>L(label+': '+getComputedStyle(document.getElementById('t')).color+' sheets='+document.styleSheets.length);</script>
    <link rel="stylesheet" href="${S('c1', '1,2,3', 400)}"></head><body><div id="t">x</div><script>L('script runs'); R('inline script after link');</script></body>`,
  'C2 parser-inserted link in body, then inline script': `<head>${HEAD}</head><body><div id="t">x</div><link rel="stylesheet" href="${S('c2', '1,2,3', 400)}"><script>L('script runs'); R('inline script after link');</script></body>`,
  // The script comes before the link: the link is not yet parsed when the script runs.
  'C3 inline script, then parser-inserted link': `<head>${HEAD}</head><body><div id="t">x</div><script>R('script before link');</script><link rel="stylesheet" href="${S('c3', '1,2,3', 400)}"><script>L('second script'); R('second script after link');</script></body>`,
  // A parser-inserted link with media that does not match, and one with `disabled`.
  'C4 parser-inserted link media=print': `<head>${HEAD}<link rel="stylesheet" media="print" href="${S('c4', '1,2,3', 400)}"></head><body><div id="t">x</div><script>L('script runs'); R('inline script');</script></body>`,
  'C5 parser-inserted link disabled': `<head>${HEAD}<link rel="stylesheet" disabled href="${S('c5', '1,2,3', 400)}"></head><body><div id="t">x</div><script>L('script runs'); R('inline script');</script></body>`,
  'C6 parser-inserted link, async external script after it': `<head>${HEAD}<link rel="stylesheet" href="${S('c6', '1,2,3', 400)}"></head><body><div id="t">x</div><script src="/r.js?k=c6"></script><script>R('after external script');</script></body>`,
  'C7 parser-inserted link, defer script': `<head>${HEAD}<link rel="stylesheet" href="${S('c7', '1,2,3', 400)}"><script defer src="/r.js?k=c7"></script></head><body><div id="t">x</div></body>`,
  // React: a preload link, then a stylesheet link for the same URL.
  'D1 script preload as=style, then rel changed to stylesheet': `${HEAD}${BODY}<script>
    const l = document.createElement('link'); l.rel = 'preload'; l.as = 'style'; l.href = '${S('d1', '1,2,3', 400)}';
    l.addEventListener('load', () => { L('preload load event'); l.rel = 'stylesheet'; R('same task after rel=stylesheet'); L('sheet ' + (l.sheet === null ? 'null' : 'set')); Promise.resolve().then(() => R('microtask')); setTimeout(() => R('timer 0'), 0); });
    document.head.appendChild(l); R('after preload insert');
  </script>`,
  'D2 preload loaded, then a new stylesheet link with the same URL': `${HEAD}${BODY}<script>
    const p = document.createElement('link'); p.rel = 'preload'; p.as = 'style'; p.href = '${S('d2', '1,2,3', 200)}';
    p.addEventListener('load', () => {
      L('preload load event');
      const l = mk('${S('d2', '1,2,3', 200)}'); document.head.appendChild(l);
      R('same task after stylesheet insert'); L('sheet ' + (l.sheet === null ? 'null' : 'set'));
      Promise.resolve().then(() => R('microtask')); setTimeout(() => R('timer 0'), 0); setTimeout(() => R('at +300ms'), 300);
    });
    document.head.appendChild(p);
  </script>`,
  'D3 preload still loading, stylesheet link with the same URL inserted': `${HEAD}${BODY}<script>
    const p = document.createElement('link'); p.rel = 'preload'; p.as = 'style'; p.href = '${S('d3', '1,2,3', 400)}'; document.head.appendChild(p);
    const l = mk('${S('d3', '1,2,3', 400)}'); document.head.appendChild(l); R('same task'); setTimeout(() => R('at 900ms'), 900);
  </script>`,
  // media
  'E1 script-inserted link media=print': `${HEAD}${BODY}<script>
    const l = mk('${S('e1', '1,2,3', 100)}', { media: 'print' }); document.head.appendChild(l); R('same task'); setTimeout(() => R('at 500ms'), 500);
    setTimeout(() => { l.media = 'all'; R('same task after media=all'); setTimeout(() => R('+500ms'), 500); }, 600);
  </script>`,
  // disabled
  'F1 script-inserted link disabled': `${HEAD}${BODY}<script>
    const l = mk('${S('f1', '1,2,3', 100)}', { disabled: true }); document.head.appendChild(l); R('same task'); setTimeout(() => { R('at 500ms'); l.disabled = false; R('same task after disabled=false'); setTimeout(() => R('+500ms'), 500); }, 500);
  </script>`,
  'F2 loaded link, disabled set': `${HEAD}${BODY}<link id="k" rel="stylesheet" href="${S('f2', '1,2,3', 50)}"><script>
    const k = document.getElementById('k'); R('before'); k.disabled = true; R('same task after disabled=true'); k.disabled = false; R('same task after disabled=false');
  </script>`,
  // Remove a loaded link and read in the same task.
  'G1 loaded link removed': `${HEAD}${BODY}<link id="k" rel="stylesheet" href="${S('g1', '1,2,3', 50)}"><script>
    const k = document.getElementById('k'); R('before'); k.remove(); R('same task after remove'); document.head.appendChild(k); R('same task after re-insert'); setTimeout(() => R('timer 0'), 0); setTimeout(() => R('at 500ms'), 500);
  </script>`,
  // innerHTML
  'H1 link inserted by innerHTML': `${HEAD}${BODY}<script>
    document.head.innerHTML += '<link rel="stylesheet" href="${S('h1', '1,2,3', 300)}">'; R('same task'); setTimeout(() => R('at 800ms'), 800);
  </script>`,
  // An inline style with an @import of a slow sheet.
  'I1 script-inserted style with @import': `${HEAD}${BODY}<script>
    const s = document.createElement('style'); s.textContent = '@import url("${S('i1', '1,2,3', 300)}"); #t{background-color:rgb(7,8,9)}'; document.head.appendChild(s);
    L('bg ' + getComputedStyle(document.getElementById('t')).backgroundColor); R('same task'); setTimeout(() => { L('bg ' + getComputedStyle(document.getElementById('t')).backgroundColor); R('at 800ms'); }, 800);
  </script>`,
  // Two links: the first is slow, the second is fast. Does the fast one apply while the slow one waits? (Same property, later wins.)
  'J1 two script-inserted links, slow then fast': `${HEAD}${BODY}<script>
    document.head.appendChild(mk('${S('j1a', '1,2,3', 500)}')); document.head.appendChild(mk('${S('j1b', '4,5,6', 50)}'));
    setTimeout(() => R('at 250ms (fast loaded, slow not)'), 250); setTimeout(() => R('at 900ms'), 900);
  </script>`,
};

const served = [];
const server = createServer((req, res) => {
  const u = new URL(req.url, 'http://x');
  if (u.pathname === '/s.css' || u.pathname === '/r.js') {
    const d = +(u.searchParams.get('d') ?? 0);
    const asked = Date.now();
    setTimeout(() => {
      served.push({ at: asked, url: req.url, sent: Date.now() });
      if (u.pathname === '/r.js') { res.writeHead(200, { 'content-type': 'text/javascript' }); res.end(`R('external script runs');`); return; }
      res.writeHead(200, { 'content-type': 'text/css', 'cache-control': 'max-age=3600' });
      res.end(`#t{color:rgb(${u.searchParams.get('c')})}`);
    }, d);
    return;
  }
  if (u.pathname === '/missing.css') { res.writeHead(404, { 'content-type': 'text/css' }); res.end(''); return; }
  const name = u.searchParams.get('case');
  res.writeHead(200, { 'content-type': 'text/html' });
  res.end(`<!doctype html>${cases[name]}`);
});
await new Promise((r) => server.listen(0, '127.0.0.1', r));
const origin = `http://127.0.0.1:${server.address().port}`;

const dir = mkdtempSync(join(tmpdir(), 'chrome-rec-'));
const port = 9400 + Math.floor(Math.random() * 400);
const chrome = spawn('google-chrome-stable', ['--headless=new', '--no-sandbox', '--disable-gpu', `--user-data-dir=${dir}`, `--remote-debugging-port=${port}`, 'about:blank'], { stdio: 'ignore' });
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
let targets;
for (let i = 0; i < 100; i++) {
  try { targets = await (await fetch(`http://127.0.0.1:${port}/json`)).json(); if (targets.some((t) => t.type === 'page')) break; } catch {}
  await sleep(100);
}
const ws = new WebSocket(targets.find((t) => t.type === 'page').webSocketDebuggerUrl);
await new Promise((r) => (ws.onopen = r));
let n = 0;
const waiting = new Map();
ws.onmessage = (m) => { const d = JSON.parse(m.data); if (d.id && waiting.has(d.id)) { waiting.get(d.id)(d); waiting.delete(d.id); } };
const send = (method, params = {}) => new Promise((r) => { const id = ++n; waiting.set(id, r); ws.send(JSON.stringify({ id, method, params })); });
const evalJs = async (e) => (await send('Runtime.evaluate', { expression: e, returnByValue: true })).result.result.value;
await send('Page.enable');

const out = [`Chrome ${(await (await fetch(`http://127.0.0.1:${port}/json/version`)).json()).Browser}`];
for (const name of Object.keys(cases)) {
  served.length = 0;
  await send('Page.navigate', { url: `${origin}/p.html?case=${encodeURIComponent(name)}` });
  await sleep(1800);
  const log = await evalJs('window.__log ? window.__log.join("\\n") : "(no log)"');
  const t0page = await evalJs('window.__t0');
  out.push(`\n## ${name}`, log, ...served.map((s) => `  server: ${s.url.split('&k=')[1] ?? s.url} requested ${s.at - t0page}ms sent ${s.sent - t0page}ms`));
}
writeFileSync(process.argv[2] ?? '/dev/stdout', out.join('\n') + '\n');
chrome.kill();
server.close();
process.exit(0);
