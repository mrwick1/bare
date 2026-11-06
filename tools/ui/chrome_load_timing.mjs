// Load one URL in cold headless Chrome and print the same numbers as `BARE_NET_TRACE=1 bare --run URL`.
// Usage: node tools/ui/chrome_load_timing.mjs URL [RUNS=3]
// Each run starts a new Chrome with a new profile (cold cache, no connection kept). Per request it prints the start
// (from the navigation start), dns, connect, tls, ttfb (request sent to first byte), body (first byte to last byte), size,
// protocol and whether the connection was reused. Per page: the `load` event time, the time the network was idle for 500 ms,
// and the CPU time of all Chrome processes (CDP `SystemInfo.getProcessInfo`). One page load per run: not a load test.
import { spawn } from 'node:child_process';
import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const [url, runs = '3'] = process.argv.slice(2);
if (!url) { console.error('usage: chrome_load_timing.mjs URL [RUNS]'); process.exit(2); }

async function cdp(wsUrl) {
  const ws = new WebSocket(wsUrl);
  await new Promise((r) => (ws.onopen = r));
  let n = 0;
  const waiting = new Map();
  const handlers = [];
  ws.onmessage = (m) => {
    const d = JSON.parse(m.data);
    if (d.id && waiting.has(d.id)) { waiting.get(d.id)(d); waiting.delete(d.id); } else handlers.forEach((h) => h(d));
  };
  return {
    send: (method, params = {}, sessionId) => new Promise((r) => { const id = ++n; waiting.set(id, r); ws.send(JSON.stringify({ id, method, params, sessionId })); }),
    on: (h) => handlers.push(h),
    close: () => ws.close(),
  };
}

async function once() {
  const dir = mkdtempSync(join(process.env.BARE_TMP ?? tmpdir(), 'chrome-timing-'));
  const port = 9800 + Math.floor(Math.random() * 100);
  const chrome = spawn('google-chrome-stable', ['--headless=new', '--no-sandbox', '--disable-gpu', '--window-size=1280,800', `--user-data-dir=${dir}`, `--remote-debugging-port=${port}`, 'about:blank'], { stdio: 'ignore' });
  try {
    let version;
    for (let i = 0; i < 100; i++) { try { version = await (await fetch(`http://127.0.0.1:${port}/json/version`)).json(); break; } catch { await sleep(100); } }
    const targets = await (await fetch(`http://127.0.0.1:${port}/json`)).json();
    const page = await cdp(targets.find((t) => t.type === 'page').webSocketDebuggerUrl);
    const browser = await cdp(version.webSocketDebuggerUrl);
    const cpu = async () => (await browser.send('SystemInfo.getProcessInfo')).result.processInfo.reduce((s, p) => s + p.cpuTime, 0);
    let lastNet = 0, loadAt = null, open = 0;
    page.on((d) => {
      if (d.method === 'Network.requestWillBeSent') { open++; lastNet = 0; }
      if (d.method === 'Network.loadingFinished' || d.method === 'Network.loadingFailed') { open--; lastNet = Date.now(); }
      if (d.method === 'Page.loadEventFired') loadAt = Date.now();
    });
    await page.send('Page.enable');
    await page.send('Network.enable');
    await page.send('Network.setCacheDisabled', { cacheDisabled: true });
    const cpu0 = await cpu();
    const t0 = Date.now();
    await page.send('Page.navigate', { url });
    // Idle: load fired, nothing open and nothing finished for 500 ms.
    while (true) {
      await sleep(50);
      if (loadAt && open === 0 && Date.now() - lastNet > 500) break;
      if (Date.now() - t0 > 60000) break;
    }
    const idleAt = lastNet - t0;
    const cpuUs = (await cpu()) - cpu0;
    const r = await page.send('Runtime.evaluate', {
      returnByValue: true,
      expression: `JSON.stringify({ nav: performance.getEntriesByType('navigation')[0], res: performance.getEntriesByType('resource') })`,
    });
    const { nav, res } = JSON.parse(r.result.result.value);
    const rows = [nav, ...res].map((e) => ({
      at: e.startTime, dns: e.domainLookupEnd - e.domainLookupStart, tcp: e.secureConnectionStart ? e.secureConnectionStart - e.connectStart : e.connectEnd - e.connectStart,
      tls: e.secureConnectionStart ? e.connectEnd - e.secureConnectionStart : 0, ttfb: e.responseStart - e.requestStart, body: e.responseEnd - e.responseStart,
      queue: e.requestStart - Math.max(e.connectEnd, e.fetchStart), bytes: e.encodedBodySize, proto: e.nextHopProtocol,
      reused: e.connectEnd === e.connectStart && e.domainLookupEnd === e.domainLookupStart, url: e.name,
    }));
    page.close(); browser.close();
    return { rows, loadMs: nav.loadEventEnd, idleMs: idleAt, cpuUs: cpuUs * 1e6 };
  } finally {
    chrome.kill();
    await sleep(300);
    rmSync(dir, { recursive: true, force: true });
  }
}

const f = (ms) => Math.round(ms * 1000);
for (let i = 1; i <= Number(runs); i++) {
  const o = await once();
  console.log(`run ${i}: load_us ${f(o.loadMs)} idle_us ${f(o.idleMs)} cpu_us ${Math.round(o.cpuUs)} requests ${o.rows.length}`);
  console.log('chrome\tat_us\twait_us\tdns_us\ttcp_us\ttls_us\tttfb_us\tbody_us\tbytes\treused\tproto\turl');
  for (const r of o.rows) console.log(['chrome', f(r.at), f(r.queue), f(r.dns), f(r.tcp), f(r.tls), f(r.ttfb), f(r.body), r.bytes, r.reused ? 1 : 0, r.proto, r.url].join('\t'));
  await sleep(20000); // a few page loads per minute at most
}
