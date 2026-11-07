// Screenshot tools/fixtures/ui/refresh-pixels.html in headless Chrome after each step of user input, script and resize, and write
// tools/fixtures/ui/chrome-refresh-pixels.txt. Each step is `@<name> <width>`, then one line `<y0>-<y1> <x0>-<x1>=<rrggbb> ...` for each band of
// rows that are the same. The bare test (`page_tests.rs`) runs the same steps through the window boundary and compares every pixel.
// Usage: BARE_TMP=/data/bare-tmp/tmp node tools/ui/chrome_refresh_pixels.mjs
import { writeFileSync } from 'node:fs';
import { inflateSync } from 'node:zlib';
import { open, sleep } from './chrome_cdp.mjs';

const H = 150;
// Decode an 8-bit RGB or RGBA PNG (the screenshot) into rows of `rrggbb` strings.
function decode(png, W) {
  let p = 8, idat = [], bpp = 0;
  while (p < png.length) {
    const len = png.readUInt32BE(p), type = png.toString('latin1', p + 4, p + 8), data = png.subarray(p + 8, p + 8 + len);
    if (type === 'IHDR') bpp = data[9] === 6 ? 4 : 3;
    if (type === 'IDAT') idat.push(data);
    p += 12 + len;
  }
  const raw = inflateSync(Buffer.concat(idat)), stride = W * bpp, rows = [];
  let prev = Buffer.alloc(stride);
  for (let y = 0; y < H; y++) {
    const f = raw[y * (stride + 1)], line = Buffer.from(raw.subarray(y * (stride + 1) + 1, (y + 1) * (stride + 1)));
    for (let i = 0; i < stride; i++) {
      const a = i >= bpp ? line[i - bpp] : 0, b = prev[i], c = i >= bpp ? prev[i - bpp] : 0;
      const pa = Math.abs(b - c), pb = Math.abs(a - c), pc = Math.abs(a + b - 2 * c);
      const add = [0, a, b, (a + b) >> 1, pa <= pb && pa <= pc ? a : pb <= pc ? b : c][f];
      line[i] = (line[i] + add) & 255;
    }
    rows.push(Array.from({ length: W }, (_, x) => [0, 1, 2].map((k) => line[x * bpp + k].toString(16).padStart(2, '0')).join('')));
    prev = line;
  }
  return rows;
}
const runs = (row) => {
  const out = [];
  for (let x = 0; x < row.length; ) { let e = x; while (e + 1 < row.length && row[e + 1] === row[x]) e++; out.push(`${x}-${e}=${row[x]}`); x = e + 1; }
  return out.join(' ');
};
let W = 200;
const c = await open('tools/fixtures/ui/refresh-pixels.html', W, H);
const out = [];
const left = (x, y) => c.mouse('mousePressed', x, y, { button: 'left', buttons: 1, clickCount: 1 });
const up = (x, y) => c.mouse('mouseReleased', x, y, { button: 'left', buttons: 0, clickCount: 1 });
// The steps. `tests` in page_tests.rs has the same list.
const steps = [
  ['start', async () => {}],
  ['hover', () => c.mouse('mouseMoved', 40, 25)],
  ['away', () => c.mouse('mouseMoved', 195, 140)],
  ['click-f', async () => { await c.mouse('mouseMoved', 110, 30); await left(110, 30); await up(110, 30); }],
  ['tab-g', () => c.key('Tab')],
  ['press-a', async () => { await c.mouse('mouseMoved', 40, 65); await left(40, 65); }],
  ['release-a', () => up(40, 65)],
  ['script', () => c.eval(`document.getElementById('d').className = 'b on'; for (let i = 0; i < 3; i++) document.getElementById('list').append(document.createElement('div')); 0`)],
  ['script-2', () => c.eval(`document.getElementById('list').firstChild.remove(); document.getElementById('d').style.top = '110px'; 0`)],
  ['blur', () => c.eval(`document.activeElement.blur(); 0`)],
  ['resize', async () => { W = 140; await c.send('Emulation.setDeviceMetricsOverride', { width: W, height: H, deviceScaleFactor: 1, mobile: false }); }],
  ['resize-back', async () => { W = 200; await c.send('Emulation.setDeviceMetricsOverride', { width: W, height: H, deviceScaleFactor: 1, mobile: false }); }],
];
for (const [name, run] of steps) {
  await run();
  await sleep(300);
  const shot = await c.send('Page.captureScreenshot', { format: 'png' });
  const rows = decode(Buffer.from(shot.result.data, 'base64'), W).map(runs);
  out.push(`@${name} ${W}`);
  for (let y0 = 0; y0 < H; ) { let e = y0; while (e + 1 < H && rows[e + 1] === rows[y0]) e++; out.push(`${y0}-${e} ${rows[y0]}`); y0 = e + 1; }
}
writeFileSync('tools/fixtures/ui/chrome-refresh-pixels.txt', out.join('\n') + '\n');
c.close();
process.exit(0);
