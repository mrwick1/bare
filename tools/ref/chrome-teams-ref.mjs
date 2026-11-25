// Reference capture of the Teams journey in Chrome: timings, HAR, console, page errors, screenshots, DOM, data-tid boxes, storage state.
// usage: PW=... node tools/ref/chrome-teams-ref.mjs OUTDIR USER_AGENT   (OUTDIR holds live credentials: keep it mode 0700, never commit it)
import { createRequire } from 'module';
import { writeFileSync, appendFileSync, chmodSync } from 'fs';
// PLAYWRIGHT_MODULES: a node_modules directory that holds playwright-core.
const require = createRequire((process.env.PLAYWRIGHT_MODULES ?? process.env.HOME + '/.local/lib/browse/node_modules') + '/');
const { chromium } = require('playwright-core');
const out = process.argv[2];
const ua = process.argv[3];
const b = await chromium.launch({ executablePath: '/usr/bin/google-chrome-stable', headless: true });
const ctx = await b.newContext({ viewport: { width: 1280, height: 800 }, userAgent: ua, recordHar: { path: out + '/teams.har.zip', content: 'attach' } });
const p = await ctx.newPage();
const t0 = Date.now(); let last = t0; const laps = [];
const lap = (n) => { const t = Date.now(); laps.push({ step: n, ms: t - last, total: t - t0 }); console.log(`${n}\t${t - last} ms\t(total ${t - t0} ms)`); last = t; };
const log = (o) => appendFileSync(out + '/console.jsonl', JSON.stringify({ t: Date.now() - t0, ...o }) + '\n');
ctx.on('console', m => log({ kind: 'console', level: m.type(), text: m.text(), url: m.location().url, line: m.location().lineNumber }));
ctx.on('weberror', e => log({ kind: 'pageerror', text: String(e.error()), stack: e.error()?.stack }));
p.on('pageerror', e => log({ kind: 'pageerror-main', text: String(e), stack: e.stack }));
const vis = (s, timeout) => p.locator(s).first().waitFor({ state: 'visible', timeout });
const snap = async (name) => {
  await p.screenshot({ path: `${out}/${name}.png` });
  writeFileSync(`${out}/${name}.html`, await p.content());
  const boxes = await p.evaluate(() => [...document.querySelectorAll('[data-tid]')].map(e => { const r = e.getBoundingClientRect(); return { tid: e.getAttribute('data-tid'), tag: e.tagName, x: Math.round(r.x), y: Math.round(r.y), w: Math.round(r.width), h: Math.round(r.height), text: (e.innerText || '').slice(0, 60) }; }));
  writeFileSync(`${out}/${name}.boxes.json`, JSON.stringify(boxes, null, 1));
};
await p.goto('https://teams.microsoft.com/v2/', { waitUntil: 'commit' }); lap('open');
await vis('input[name=loginfmt]', 45000); lap('login form visible');
await p.fill('input[name=loginfmt]', 'arjun@qilinlab.com'); await p.click('#idSIButton9');
await vis('input[name=passwd]', 20000); lap('password form visible');
await p.fill('input[name=passwd]', process.env.PW); await p.click('#idSIButton9');
await vis('#idSIButton9', 30000); lap('stay signed in visible');
await p.click('#idSIButton9'); last = Date.now();
await vis("[data-tid='app-layout-area--main']", 90000); lap('main area visible');
await snap('at-main-area');
await p.waitForFunction(() => { const l = document.getElementById('loading-screen'); return !l || l.getAttribute('aria-hidden') === 'true'; }, null, { timeout: 90000 }); lap('loading screen hidden');
await p.waitForTimeout(20000); lap('settle 20 s');
await snap('settled');
await ctx.storageState({ path: out + '/state.json' }); chmodSync(out + '/state.json', 0o600);
writeFileSync(out + '/timings.json', JSON.stringify(laps, null, 1));
await ctx.close(); await b.close();
