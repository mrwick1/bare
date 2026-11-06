// Record what Chrome returns for `getComputedStyle` of elements that are not rendered.
// Usage: BARE_TMP=/data/bare-tmp node tools/ui/chrome_hidden_style.mjs tools/fixtures/ui/chrome-hidden-styles.txt
// One page holds the cases. Each case names one element and the properties to read. The output has one line per read.
import { writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { open } from './chrome_cdp.mjs';

const dir = process.env.BARE_TMP ?? '/tmp';
const file = join(dir, 'hidden-style.html');
writeFileSync(file, `<!doctype html><html><head><style>
  :root { --r: 7px; font-size: 20px; }
  #h1 { display: none; color: rgb(1, 2, 3); font-size: 10px; --x: hello; position: relative; width: 50%; --y: 3px; }
  #h1 > #c1 { position: sticky; width: 40%; height: 2em; margin-top: 3px; top: 5px; padding-left: 10%; line-height: 1.5; font-size: 2em; padding-top: var(--y); border-top: 1px solid red; }
  #h1 #g1 { color: inherit; --z: var(--x) there; width: var(--r); margin-left: 1rem; }
  #h2 { display: none; }
  #h2 #n1 { display: none; color: rgb(9, 9, 9); }
  #h2 #n1 #n2 { position: fixed; }
  #dc { display: contents; color: rgb(4, 5, 6); }
  #dc > #dcc { width: 30px; }
  #hid { color: rgb(7, 8, 9); }
  #hid > #hidc { position: absolute; }
  #v { width: 120px; height: 20px; margin-top: 4px; position: relative; top: 2px; display: block; }
  #fixed-ancestor { display: none; }
  .m { display: block; } #h1 > #c2 { display: none; position: absolute; }
  #tp { display: none; } #tp-child { visibility: hidden; }
</style></head><body>
<div id="v">v</div>
<div id="h1"><span id="c1">c1<b id="g1">g1</b></span><span id="c2">c2</span></div>
<div id="h2"><div id="n1"><div id="n2">n</div></div></div>
<div id="dc"><p id="dcc">x</p></div>
<div id="hid" hidden><p id="hidc">x</p></div>
<div id="dp" style="display:none; color: rgb(5, 6, 7)">${'<div>'.repeat(40)}<div id="dpe" style="position: fixed; width: 10%">e</div>${'</div>'.repeat(40)}</div>
<template id="tpl"><div id="inT" style="position:fixed">t</div></template>
<details id="det"><summary>s</summary><p id="detp" style="position:sticky">p</p></details>
<div style="content-visibility:hidden"><p id="cvh" style="position:sticky">p</p></div>
<iframe id="fr" style="display:none" srcdoc="<p id=ip style='position:fixed'>x</p>"></iframe>
<svg style="display:none"><g id="svgg" style="position:absolute"><rect id="svgr"/></g></svg>
<select id="sel" style="display:none"><option id="opt" style="position:relative">o</option></select>
<script>
  window.pre = getComputedStyle(document.getElementById('h1')).position;
</script>
</body></html>`);

const reads = [
  ['v', ['position', 'display', 'width', 'height', 'margin-top', 'top']],
  ['h1', ['position', 'display', 'color', 'width', 'font-size', '--x', '--y']],
  ['c1', ['position', 'display', 'color', 'width', 'height', 'margin-top', 'top', 'padding-left', 'padding-top', 'line-height', 'font-size', 'border-top-width', 'border-top']],
  ['g1', ['position', 'display', 'color', '--x', '--z', 'width', 'margin-left', 'font-size']],
  ['c2', ['position', 'display', 'width']],
  ['h2', ['display', 'position']],
  ['n1', ['display', 'color', 'position']],
  ['n2', ['display', 'color', 'position', 'width']],
  ['dp', ['display', 'color']],
  ['dpe', ['position', 'color', 'display', 'width']],
  ['dc', ['display', 'color', 'width']],
  ['dcc', ['display', 'color', 'width']],
  ['hid', ['display', 'color']],
  ['hidc', ['display', 'color', 'position']],
  ['inT', ['position', 'display', 'color', 'width']],
  ['detp', ['position', 'display']],
  ['cvh', ['position', 'display']],
  ['svgg', ['position', 'display']],
  ['svgr', ['position', 'display']],
  ['opt', ['position', 'display']],
];
const c = await open(file, 800, 600);
const out = [];
out.push('Chrome ' + (await c.eval('navigator.userAgent')));
out.push('script read of #h1 position before load: ' + (await c.eval('window.pre')));
for (const [id, props] of reads) {
  for (const p of props) {
    const v = await c.eval(`(() => { const e = document.getElementById(${JSON.stringify(id)}) ?? document.getElementById('tpl').content.getElementById(${JSON.stringify(id)}); return e ? JSON.stringify(getComputedStyle(e).getPropertyValue(${JSON.stringify(p)})) : 'no element'; })()`);
    out.push(`#${id} ${p} = ${v}`);
  }
}
// Properties of the declaration object (length, item), for a hidden descendant, and the style of a template child, and detached ones.
const extra = {
  'c1 length > 0': 'getComputedStyle(document.getElementById("c1")).length > 0',
  'c1 camel position': 'getComputedStyle(document.getElementById("c1")).position',
  'c1 cssText': 'JSON.stringify(getComputedStyle(document.getElementById("c1")).cssText.slice(0, 40))',
  'template child, same doc? (ownerDocument !== document)': 'document.getElementById("tpl").content.getElementById("inT").ownerDocument !== document',
  'template child isConnected': 'document.getElementById("tpl").content.getElementById("inT").isConnected',
  'template child length': 'getComputedStyle(document.getElementById("tpl").content.getElementById("inT")).length',
  'template child position': 'JSON.stringify(getComputedStyle(document.getElementById("tpl").content.getElementById("inT")).position)',
  'template child via document.getComputedStyle? (window of content doc)': 'JSON.stringify(document.getElementById("tpl").content.ownerDocument.defaultView)',
  'detached div length': 'getComputedStyle(document.createElement("div")).length',
  'detached div position': 'JSON.stringify(getComputedStyle(document.createElement("div")).position)',
  'detached child of detached, position': '(() => { const a = document.createElement("div"); const b = a.appendChild(document.createElement("div")); b.style.position = "fixed"; return JSON.stringify(getComputedStyle(b).position); })()',
  'iframe doc inside display:none iframe: position': 'JSON.stringify(getComputedStyle(document.getElementById("fr").contentDocument.getElementById("ip")).position)',
  'c1 after display:none removed from #h1, position': '(() => { const h = document.getElementById("h1"); h.style.display = "block"; const r = getComputedStyle(document.getElementById("c1")).position; h.style.display = ""; return r; })()',
  'c1 width when #h1 shown (used value)': '(() => { const h = document.getElementById("h1"); h.style.display = "block"; const r = getComputedStyle(document.getElementById("c1")).width; h.style.display = ""; return r; })()',
  'c1 inherited color via style change': '(() => { const h = document.getElementById("h1"); h.style.color = "rgb(9, 8, 7)"; const r = getComputedStyle(document.getElementById("g1")).color; h.style.color = ""; return r; })()',
  'c1 object stays live: held before, class added after': '(() => { const cs = getComputedStyle(document.getElementById("c1")); document.getElementById("c1").style.position = "fixed"; const r = cs.position; document.getElementById("c1").style.position = ""; return r; })()',
};
for (const [label, js] of Object.entries(extra)) out.push(`${label}: ${JSON.stringify(await c.eval(js))}`);
c.close();
writeFileSync(process.argv[2] ?? 1, out.join('\n') + '\n');
console.log(out.join('\n'));
