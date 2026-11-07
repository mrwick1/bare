// A small CDP helper for the scroll comparisons: start headless Chrome on a file, then run steps.
// Usage from a script: `import { open } from './chrome_cdp.mjs'; const c = await open(file, 400, 300, { bars: true });`
// `c.eval(js)` runs JS in the page and returns the value. `c.shot(path)` writes a PNG. `c.wheel(x, y, dx, dy)`, `c.key(name)`,
// `c.mouse(type, x, y, extra)` use the same input pipeline as a device. `c.close()` stops Chrome.
import { spawn } from 'node:child_process';
import { mkdtempSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';

export const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

export async function open(file, width, height, { bars = false, args = [] } = {}) {
  const dir = mkdtempSync(join(process.env.BARE_TMP ?? tmpdir(), 'chrome-cdp-'));
  const port = 9400 + Math.floor(Math.random() * 400);
  const flags = ['--headless=new', '--no-sandbox', '--disable-gpu', '--force-device-scale-factor=1', `--window-size=${width},${height}`, `--user-data-dir=${dir}`, `--remote-debugging-port=${port}`, ...args];
  if (!bars) flags.push('--hide-scrollbars');
  const chrome = spawn('google-chrome-stable', [...flags, 'about:blank'], { stdio: 'ignore' });
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
  await send('Page.enable');
  await send('Emulation.setDeviceMetricsOverride', { width, height, deviceScaleFactor: 1, mobile: false });
  await send('Page.navigate', { url: 'file://' + resolve(file) });
  await sleep(800);
  await send('Emulation.setFocusEmulationEnabled', { enabled: true });
  const c = {
    send,
    async eval(js) { const r = await send('Runtime.evaluate', { expression: js, returnByValue: true, awaitPromise: true }); return r.result.exceptionDetails ? 'EXC ' + r.result.exceptionDetails.text : r.result.result.value; },
    async shot(path) { const r = await send('Page.captureScreenshot', { format: 'png' }); writeFileSync(path, Buffer.from(r.result.data, 'base64')); },
    mouse: (type, x, y, extra = {}) => send('Input.dispatchMouseEvent', { type, x, y, button: 'none', buttons: 0, clickCount: 0, ...extra }),
    wheel: (x, y, deltaX, deltaY) => send('Input.dispatchMouseEvent', { type: 'mouseWheel', x, y, deltaX, deltaY }),
    async key(name, modifiers = 0) {
      const K = { ArrowDown: 40, ArrowUp: 38, ArrowLeft: 37, ArrowRight: 39, PageDown: 34, PageUp: 33, Home: 36, End: 35, Tab: 9, Enter: 13, ' ': 32 };
      const vk = K[name] ?? name.toUpperCase().charCodeAt(0);
      const code = name === ' ' ? 'Space' : /^[a-z]$/i.test(name) ? 'Key' + name.toUpperCase() : name;
      const base = { key: name, code, windowsVirtualKeyCode: vk, nativeVirtualKeyCode: vk, modifiers };
      const text = name === ' ' ? ' ' : name === 'Enter' ? '\r' : name.length === 1 ? name : undefined;
      await send('Input.dispatchKeyEvent', { type: text ? 'keyDown' : 'rawKeyDown', ...base, ...(text ? { text } : {}) });
      await send('Input.dispatchKeyEvent', { type: 'keyUp', ...base });
    },
    sleep,
    close() { ws.close(); chrome.kill(); },
  };
  return c;
}
