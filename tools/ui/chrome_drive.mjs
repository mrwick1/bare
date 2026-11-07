// Record what headless Chrome logs for the driver scenarios of tools/fixtures/ui/drive-steps.txt on tools/fixtures/ui/drive.html.
// Usage: node tools/ui/chrome_drive.mjs tools/fixtures/ui/chrome-drive.txt
// A step is what `Input::Click`, `Input::Key` and `Input::Type` do in bare, sent through the input pipeline of a device
// (`Input.dispatchMouseEvent`, `Input.dispatchKeyEvent`). The bare test is `crates/bare/src/browser_drive_tests.rs`.
import { readFileSync, writeFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { open, sleep } from './chrome_cdp.mjs';

const page = resolve(import.meta.dirname, '../fixtures/ui/drive.html');
const scenarios = [];
for (const line of readFileSync(resolve(import.meta.dirname, '../fixtures/ui/drive-steps.txt'), 'utf8').split('\n')) {
  if (line.startsWith('#') || line === '') continue;
  if (line.startsWith('== ')) scenarios.push({ name: line.slice(3), steps: [] });
  else scenarios.at(-1).steps.push(line);
}
const c = await open(page, 400, 800);

const NAMED = {
  Backspace: [8, 'Backspace'], Delete: [46, 'Delete'], Tab: [9, 'Tab'], Enter: [13, 'Enter', '\r'], Escape: [27, 'Escape'],
  ArrowLeft: [37, 'ArrowLeft'], ArrowRight: [39, 'ArrowRight'], ArrowUp: [38, 'ArrowUp'], ArrowDown: [40, 'ArrowDown'], Home: [36, 'Home'], End: [35, 'End'],
  Space: [32, 'Space', ' '],
};
const MODS = { Shift: [8, 16, 'ShiftLeft'], Control: [2, 17, 'ControlLeft'], Alt: [1, 18, 'AltLeft'], Meta: [4, 91, 'MetaLeft'] };
const keyEvents = (name, modifiers) => {
  const k = name === ' ' ? 'Space' : name;
  const [vk, code, text] = NAMED[k] ?? [name.toUpperCase().charCodeAt(0), /^[a-z]$/i.test(name) ? 'Key' + name.toUpperCase() : /^[0-9]$/.test(name) ? 'Digit' + name : '', name];
  const key = k === 'Space' ? ' ' : NAMED[k] ? k : name;
  const base = { key, code, windowsVirtualKeyCode: vk, nativeVirtualKeyCode: vk, modifiers };
  const printable = text && !(modifiers & 3 && modifiers) ;
  return [{ type: text ? 'keyDown' : 'rawKeyDown', ...base, ...(text ? { text } : {}) }, { type: 'keyUp', ...base }];
};
const press = async (spec) => {
  const parts = spec === '+' ? ['+'] : spec.split('+');
  const mods = parts.slice(0, -1);
  let bits = 0;
  for (const m of mods) {
    bits |= MODS[m][0];
    await c.send('Input.dispatchKeyEvent', { type: 'rawKeyDown', key: m, code: MODS[m][2], windowsVirtualKeyCode: MODS[m][1], nativeVirtualKeyCode: MODS[m][1], modifiers: bits, location: 1 });
  }
  const [down, up] = keyEvents(parts.at(-1), bits);
  await c.send('Input.dispatchKeyEvent', down);
  await c.send('Input.dispatchKeyEvent', up);
  for (const m of mods.reverse()) {
    bits &= ~MODS[m][0];
    await c.send('Input.dispatchKeyEvent', { type: 'keyUp', key: m, code: MODS[m][2], windowsVirtualKeyCode: MODS[m][1], nativeVirtualKeyCode: MODS[m][1], modifiers: bits, location: 1 });
  }
  await sleep(60);
};
const center = async (sel) => c.eval(`(function () { var r = document.querySelector(${JSON.stringify(sel)}).getBoundingClientRect(); return [r.x + r.width / 2, r.y + r.height / 2] })()`);
const click = async (sel) => {
  const [x, y] = await center(sel);
  await c.mouse('mouseMoved', x, y);
  await sleep(120);
  await c.mouse('mousePressed', x, y, { button: 'left', buttons: 1, clickCount: 1, force: 0.5 });
  await sleep(60);
  await c.mouse('mouseReleased', x, y, { button: 'left', buttons: 0, clickCount: 1 });
  await sleep(100);
};
const run = async (step) => {
  const [verb, sel, ...rest] = step.split(' ');
  const text = rest.join(' ');
  if (verb === 'reset') { await sleep(100); await c.eval('window.__log.length = 0'); }
  else if (verb === 'click') await click(sel);
  else if (verb === 'key') {
    if (sel !== '-') await c.eval(`document.querySelector(${JSON.stringify(sel)}).focus()`);
    await sleep(50);
    await press(text);
  } else if (verb === 'type') {
    await c.eval(`(function () { var e = document.querySelector(${JSON.stringify(sel)}); e.focus(); if (e.value) e.select() })()`);
    const t = text.replace(/\\n/g, '\n');
    if (t === '' && await c.eval(`document.querySelector(${JSON.stringify(sel)}).value !== ''`)) await press('Backspace');
    for (const ch of t) await press(ch === '\n' ? 'Enter' : ch);
  } else throw new Error('bad step ' + step);
};

let out = '';
for (const s of scenarios) {
  await c.send('Page.navigate', { url: 'file://' + page });
  await sleep(500);
  await c.send('Emulation.setFocusEmulationEnabled', { enabled: true });
  for (const step of s.steps) await run(step);
  await sleep(150);
  const log = await c.eval('window.__log.slice()');
  out += `== ${s.name}\n${log.join('\n')}${log.length ? '\n' : ''}`;
}
writeFileSync(process.argv[2] ?? '/dev/stdout', out);
c.close();
process.exit(0);
