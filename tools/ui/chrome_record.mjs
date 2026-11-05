// Record the order and properties of the UI events that Chrome fires for synthetic input on tools/fixtures/ui/events.html.
// Usage: node tools/ui/chrome_record.mjs tools/fixtures/ui/chrome-events.txt
// Chrome runs headless with the DevTools protocol. `Input.dispatchMouseEvent` and `Input.dispatchKeyEvent` go through the
// same input pipeline as a real device (hit test, focus, default actions).
import { spawn } from 'node:child_process';
import { mkdtempSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';

const page = 'file://' + resolve(import.meta.dirname, '../fixtures/ui/events.html');
const dir = mkdtempSync(join(tmpdir(), 'chrome-rec-'));
const port = 9400 + Math.floor(Math.random() * 400);
const chrome = spawn('google-chrome-stable', ['--headless=new', '--no-sandbox', '--disable-gpu', '--hide-scrollbars',
  '--force-device-scale-factor=1', '--window-size=400,400', `--user-data-dir=${dir}`, `--remote-debugging-port=${port}`, 'about:blank'], { stdio: 'ignore' });
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
await send('Emulation.setDeviceMetricsOverride', { width: 400, height: 400, deviceScaleFactor: 1, mobile: false });
await send('Page.navigate', { url: page });
await sleep(800);
// Chrome headless may report the page as unfocused: make it focused, so that focus and blur events fire.
await send('Emulation.setFocusEmulationEnabled', { enabled: true });

const mouse = (type, x, y, extra = {}) => send('Input.dispatchMouseEvent', { type, x, y, button: 'none', buttons: 0, clickCount: 0, ...extra });
const move = async (x, y, buttons = 0) => { await mouse('mouseMoved', x, y, { buttons }); await sleep(120); };
const click = async (x, y) => {
  await move(x, y);
  await mouse('mousePressed', x, y, { button: 'left', buttons: 1, clickCount: 1 });
  await sleep(60);
  await mouse('mouseReleased', x, y, { button: 'left', buttons: 0, clickCount: 1 });
  await sleep(60);
};
const KEYS = {
  Backspace: { key: 'Backspace', code: 'Backspace', vk: 8 }, Delete: { key: 'Delete', code: 'Delete', vk: 46 },
  Tab: { key: 'Tab', code: 'Tab', vk: 9 }, Enter: { key: 'Enter', code: 'Enter', vk: 13, text: '\r' },
  ArrowLeft: { key: 'ArrowLeft', code: 'ArrowLeft', vk: 37 }, ArrowRight: { key: 'ArrowRight', code: 'ArrowRight', vk: 39 }, Home: { key: 'Home', code: 'Home', vk: 36 }, End: { key: 'End', code: 'End', vk: 35 },
};
const press = async (name, modifiers = 0) => {
  const k = KEYS[name] ?? (name === ' ' ? { key: ' ', code: 'Space', vk: 32, text: ' ' } : { key: name, code: (/[a-z]/i.test(name) ? 'Key' + name.toUpperCase() : 'Digit' + name), vk: name.toUpperCase().charCodeAt(0), text: name });
  const base = { key: k.key, code: k.code, windowsVirtualKeyCode: k.vk, nativeVirtualKeyCode: k.vk, modifiers };
  await send('Input.dispatchKeyEvent', { type: k.text ? 'keyDown' : 'rawKeyDown', ...base, ...(k.text ? { text: k.text } : {}) });
  await send('Input.dispatchKeyEvent', { type: 'keyUp', ...base });
};
const reset = async () => { await evalJs('window.__log.length = 0'); await sleep(50); };
const take = async () => { await sleep(100); return evalJs('window.__log.slice()'); };

const out = {};
const frozen = () => evalJs('JSON.stringify([document.getElementById("user").value, document.getElementById("pass").value, document.getElementById("chk").checked, document.activeElement.id])');
// Coordinates: the fixture puts each control at left 10, width 200.
const at = { link: [50, 25], user: [50, 75], pass: [50, 125], chk: [20, 170], go: [50, 215], plain: [50, 265], none: [350, 350] };

await click(...at.none); await reset();
await move(...at.plain); out.move_into_button = await take(); await reset();
await mouse('mousePressed', ...at.plain, { button: 'left', buttons: 1, clickCount: 1 }); await sleep(60);
await mouse('mouseReleased', ...at.plain, { button: 'left', buttons: 0, clickCount: 1 });
out.click_plain_button = await take(); await reset();
await move(...at.link); out.move_button_to_link = await take(); await reset();
await mouse('mousePressed', ...at.link, { button: 'left', buttons: 1, clickCount: 1 }); await sleep(60);
await mouse('mouseReleased', ...at.link, { button: 'left', buttons: 0, clickCount: 1 });
out.click_link = await take(); await reset();
await move(...at.user); await reset();
await mouse('mousePressed', ...at.user, { button: 'left', buttons: 1, clickCount: 1 }); await sleep(60);
await mouse('mouseReleased', ...at.user, { button: 'left', buttons: 0, clickCount: 1 });
out.click_text_input_focuses = await take(); await reset();
await press('a'); out.type_a = await take(); await reset();
await press('b'); await reset();
await press('Backspace'); out.backspace = await take(); await reset();
await press('ArrowLeft'); out.arrow_left = await take(); await reset();
await press('Delete'); out.delete = await take(); await reset();
await press('Home'); out.home = await take(); await reset();
await press('Tab'); out.tab_to_password = await take(); await reset();
await press('Tab', 8); out.shift_tab_back = await take(); await reset();
await press('Tab'); await reset();
await press('x'); await reset();
await press('Enter'); out.enter_in_password_submits = await take(); await reset();
await move(...at.chk); await reset();
await click(...at.chk); out.click_checkbox = await take(); await reset();
await move(...at.go); await reset();
await click(...at.go); out.click_submit_button = await take(); await reset();
// More scenarios: leaving a field, change on blur, selection keys, keyboard activation, shift for capitals.
await move(...at.link); await move(...at.none); out.move_link_to_blank = await take(); await reset();
await click(...at.user); await reset();
await press('q'); await reset();
await click(...at.plain); out.edit_then_click_away = await take(); await reset();
await click(...at.user); await reset();
await press('A', 8); out.type_capital_with_shift = await take(); await reset();
await press('ArrowLeft', 8); out.shift_arrow_left = await take(); await reset();
await press('a', 2); out.ctrl_a_select_all = await take(); await reset();
await press('ArrowRight'); out.arrow_right_collapses_selection = await take(); await reset();
await press('Tab'); await press('Tab'); await reset();
await press(' '); out.space_on_checkbox = await take(); await reset();
await press('Tab'); await reset();
await press('Enter'); out.enter_on_submit_button = await take(); await reset();
out.values = await frozen();
// One text file: `== name` starts a scenario, each next line is one event.
const text = Object.entries(out).filter(([k]) => k !== 'values').map(([k, v]) => `== ${k}\n${v.join('\n')}${v.length ? '\n' : ''}`).join('');
writeFileSync(process.argv[2] ?? '/dev/stdout', text);
ws.close();
chrome.kill();
process.exit(0);
