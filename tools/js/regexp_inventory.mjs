// Regexp literal inventory (S2). Parses each FILE with Acorn (ecma2022, script; never runs it)
// and prints one JSON line per regexp literal: {"file","start","pattern","flags"}.
// Also counts `new RegExp(` / `RegExp(` call sites, which have dynamic patterns (unresolved).
//   ACORN=file:///path/acorn.mjs node tools/js/regexp_inventory.mjs FILE...
import { readFileSync } from 'node:fs';
import { basename } from 'node:path';
const acorn = await import(process.env.ACORN);
const dyn = [];
for (const f of process.argv.slice(2)) {
  const src = readFileSync(f, 'utf8');
  const toks = [];
  const ast = acorn.parse(src, { ecmaVersion: 2022, sourceType: 'script', allowHashBang: true, onToken: toks });
  for (const t of toks) {
    if (t.type.label === 'regexp')
      console.log(JSON.stringify({ file: basename(f), start: t.start, pattern: t.value.pattern, flags: t.value.flags }));
  }
  const walk = (n) => {
    if (!n || typeof n.type !== 'string') return;
    if ((n.type === 'NewExpression' || n.type === 'CallExpression') && n.callee.type === 'Identifier' && n.callee.name === 'RegExp')
      dyn.push({ file: basename(f), start: n.start, arg0: n.arguments[0]?.type, flagsArg: n.arguments.length > 1 });
    for (const k in n) { const v = n[k]; if (Array.isArray(v)) v.forEach(walk); else if (v && typeof v.type === 'string') walk(v); }
  };
  walk(ast);
}
console.error(JSON.stringify({ dynamic_regexp_calls: dyn }));
