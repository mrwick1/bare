// Screenshot tools/fixtures/ui/sticky-pixels.html in headless Chrome (200x150) at several page scroll offsets and write
// tools/fixtures/ui/chrome-sticky-pixels.txt. The file holds, for each offset, the picture as runs: `@<scrollY>`, then one line `<y0>-<y1> <x0>-<x1>=<rrggbb> ...`
// for each band of rows that are the same. The bare test (`browser_sticky_tests.rs`) draws the same page at the same offsets and compares every pixel.
// Usage: BARE_TMP=/data/bare-tmp/tmp node tools/ui/chrome_sticky_pixels.mjs
import { writeFileSync } from 'node:fs';
import { inflateSync } from 'node:zlib';
import { open, sleep } from './chrome_cdp.mjs';

const W = 200, H = 150;
// Decode an 8-bit RGB or RGBA PNG (the screenshot) into rows of `rrggbb` strings.
function decode(png) {
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
  for (let x = 0; x < W; ) { let e = x; while (e + 1 < W && row[e + 1] === row[x]) e++; out.push(`${x}-${e}=${row[x]}`); x = e + 1; }
  return out.join(' ');
};
const c = await open('tools/fixtures/ui/sticky-pixels.html', W, H);
const out = [];
for (const y of [0, 40, 380, 500]) {
  await c.eval(`scrollTo(0, ${y}); 0`);
  await sleep(300);
  const shot = await c.send('Page.captureScreenshot', { format: 'png' });
  const rows = decode(Buffer.from(shot.result.data, 'base64')).map(runs);
  out.push(`@${y}`);
  for (let y0 = 0; y0 < H; ) { let e = y0; while (e + 1 < H && rows[e + 1] === rows[y0]) e++; out.push(`${y0}-${e} ${rows[y0]}`); y0 = e + 1; }
}
writeFileSync('tools/fixtures/ui/chrome-sticky-pixels.txt', out.join('\n') + '\n');
c.close();
process.exit(0);
