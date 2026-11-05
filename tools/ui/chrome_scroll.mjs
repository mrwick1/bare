// Replay tools/fixtures/ui/scroll-steps.txt in headless Chrome (400x300, classic scrollbars) on tools/fixtures/ui/scroll.html and write
// tools/fixtures/ui/chrome-scroll.txt: `<step> => <result>` per line. The bare test replays the same steps and compares.
import { readFileSync, writeFileSync } from 'node:fs';
import { open, sleep } from './chrome_cdp.mjs';

const steps = readFileSync('tools/fixtures/ui/scroll-steps.txt', 'utf8').trim().split('\n');
const c = await open('tools/fixtures/ui/scroll.html', 400, 300, { bars: true });
await c.eval('document.documentElement.focus(); 0');
const out = [];
for (const s of steps) {
  let r = '';
  if (s.startsWith('JS ')) r = String(await c.eval(s.slice(3)));
  else if (s === 'WAIT') await sleep(300);
  else if (s.startsWith('KEY ')) {
    const [name, mod] = s.slice(4).split('+');
    await c.key(name === 'Space' ? ' ' : name, mod === 'Shift' ? 8 : 0);
    await sleep(700);
    r = String(await c.eval('scrollY'));
  }
  out.push(`${s} => ${r}`);
}
writeFileSync('tools/fixtures/ui/chrome-scroll.txt', out.join('\n') + '\n');
c.close();
