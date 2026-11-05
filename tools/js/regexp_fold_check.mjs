// Check simple case folding (ECMA-262 Canonicalize with `u` and `i`) of bare-unicode against Node.
// Input on stdin: `regexp_load fold-dump` (one "code-point folding" hex pair per line).
//   regexp_load fold-dump | node tools/js/regexp_fold_check.mjs
// For each code point, Node's own /^c$/iu is tested on its single-code-point lower and upper case
// and on the members of our folding group. Our answer: the two foldings are equal.
import { readFileSync } from 'node:fs';
const ours = new Map(readFileSync(0, 'utf8').trim().split('\n').map((l) => l.split(' ').map((x) => parseInt(x, 16))));
const fold = (c) => ours.get(c) ?? c;
const group = new Map();
for (const [c, f] of ours) (group.get(f) ?? group.set(f, [f]).get(f)).push(c);
// Our data is Unicode 18. Node's ICU may be older: a code point that Node sees as unassigned is drift.
// Drift is reported, not hidden. Any other difference is a failure.
const cn = /^\p{Cn}$/u;
let bad = 0, drift = 0, checked = 0;
for (let c = 0; c <= 0x10ffff; c++) {
  if (c >= 0xd800 && c < 0xe000) continue;
  const s = String.fromCodePoint(c);
  const cands = new Set(group.get(fold(c)) ?? []);
  for (const t of [s.toLowerCase(), s.toUpperCase()]) if ([...t].length === 1) cands.add(t.codePointAt(0));
  cands.delete(c);
  if (!cands.size) continue;
  const re = new RegExp('^\\u{' + c.toString(16) + '}$', 'iu');
  for (const d of cands) {
    checked++;
    const node = re.test(String.fromCodePoint(d)), mine = fold(c) === fold(d);
    if (node === mine) continue;
    if (cn.test(s) || cn.test(String.fromCodePoint(d))) { drift++; continue; }
    if (bad++ < 10) console.log('fold differs', c.toString(16), d.toString(16), 'node', node, 'ours', mine);
  }
}
console.log(`fold: bad=${bad} unicode-version drift pairs=${drift} folding entries=${ours.size} regexp checks=${checked}`);
process.exit(bad ? 1 : 0);
