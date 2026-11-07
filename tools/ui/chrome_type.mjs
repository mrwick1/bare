// Per-key time of typing into a field of a page in headless Chrome (the same page as `type_load`, see tools/js/typing/build.sh).
// Usage: node tools/ui/chrome_type.mjs DIR SELECTOR CHARS. It sends real key events and waits for the next frame.
import { open, sleep } from './chrome_cdp.mjs';
const [dir, sel, n] = [process.argv[2], process.argv[3], +process.argv[4]];
const c = await open(dir + '/index.html', 1280, 800);
await c.eval(`document.querySelector(${JSON.stringify(sel)}).focus()`);
const t = performance.now();
for (let i = 0; i < n; i++) await c.key(String.fromCharCode(97 + (i % 26)));
await c.eval('new Promise(r => requestAnimationFrame(() => setTimeout(r, 0)))');
const ms = performance.now() - t;
const len = await c.eval(`document.querySelector(${JSON.stringify(sel)}).value.length`);
console.log(`chrome ${sel} x${n}: ${ms.toFixed(0)} ms (${(ms / n).toFixed(1)} ms/key), value ${len} chars, title ${await c.eval('document.title')}`);
c.close();
