// Check journeys in headless Chrome, so each assertion is true in a real browser before bare runs it.
//   node chrome_check.mjs NAME...      (for example: node chrome_check.mjs go-dev-home sqlite-org-home)
// It reads NAME.steps and supports open, goto, click css, expect title/url/css/js. It skips other lines.
// Like bare-drive, a css target must match exactly one element.
import { readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { open, sleep } from '../../ui/chrome_cdp.mjs';

const here = dirname(fileURLToPath(import.meta.url));
const q = (s) => JSON.stringify(s);
const parse = (v) => (/^"/.test(v) ? JSON.parse(v) : v);
let bad = 0;
for (const name of process.argv.slice(2)) {
  const text = readFileSync(join(here, name + '.steps'), 'utf8');
  const url = text.match(/--var URL=(\S+)/)[1];
  const c = await open('', 1280, 800);
  const go = async (u) => { await c.send('Page.navigate', { url: u }); await sleep(1500); for (let i = 0; i < 40 && (await c.eval('document.readyState')) !== 'complete'; i++) await sleep(500); await sleep(1500); };
  const fails = [];
  for (const raw of text.split('\n')) {
    const l = raw.trim();
    if (!l || l.startsWith('#') || l.startsWith('timeout')) continue;
    const [verb, ...rest] = l.split(' ');
    const arg = rest.join(' ').replaceAll('${URL}', url);
    let ok = true;
    if (verb === 'open' || verb === 'goto') await go(arg);
    else if (verb === 'click') {
      const sel = parse(arg.match(/^css=(".*")$/)[1]);
      if ((await c.eval(`document.querySelectorAll(${q(sel)}).length`)) !== 1) fails.push('not one match: ' + l);
      await c.eval(`document.querySelector(${q(sel)}).click()`);
      await sleep(1500); for (let i = 0; i < 40 && (await c.eval('document.readyState')) !== 'complete'; i++) await sleep(500); await sleep(1500);
    } else if (verb === 'expect') {
      let m;
      if ((m = arg.match(/^title([=~])(".*")$/))) { const t = await c.eval('document.title'); ok = m[1] === '=' ? t === parse(m[2]) : t.includes(parse(m[2])); }
      else if ((m = arg.match(/^url~(".*")$/))) ok = (await c.eval('location.href')).includes(parse(m[1]));
      else if ((m = arg.match(/^js (".*")$/))) ok = (await c.eval(parse(m[1]))) === true;
      else if ((m = arg.match(/^css=(".*") text~(".*")$/))) ok = await c.eval(`(l => l.length === 1 && (l[0].innerText || l[0].textContent).includes(${q(parse(m[2]))}))(document.querySelectorAll(${q(parse(m[1]))}))`);
      else if ((m = arg.match(/^css=(".*") visible$/))) ok = await c.eval(`(l => l.length === 1 && l[0].getClientRects().length > 0)(document.querySelectorAll(${q(parse(m[1]))}))`);
      else { console.log('  skipped:', l); continue; }
    }
    if (ok !== true) fails.push(l);
  }
  c.close();
  console.log(fails.length ? `FAIL ${name}\n  ${fails.join('\n  ')}` : `ok   ${name}`);
  bad += fails.length;
}
process.exit(bad ? 1 : 0);
