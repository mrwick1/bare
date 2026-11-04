// Check Canonicalize (ECMA-262 22.2.2.7.3, non-Unicode i) of crates/js/src/regexp.rs against Node
// for all 65536 code units. Input on stdin: `regexp_load canon-dump` (65536 hex lines).
//   regexp_load canon-dump | node tools/js/regexp_canon_check.mjs
// 1) Spec rule from String.prototype.toUpperCase. 2) Node's own /^c$/i on every unit that shares a
// canonical form with another, and on one unit from each other class (no false equivalence).
import { readFileSync } from 'node:fs';
const got = readFileSync(0, 'utf8').trim().split('\n').map((x) => parseInt(x, 16));
if (got.length !== 65536) throw new Error('need 65536 lines');
// Rust's standard library has a newer Unicode version than this Node's ICU. These units get an
// uppercase mapping in Rust (Latin Extended-D letters added after Unicode 16) that Node 22 lacks.
// Known and reported, not hidden: anything else is a failure.
const DRIFT = new Set([0xa7ce, 0xa7cf, 0xa7d2, 0xa7d3, 0xa7d4, 0xa7d5]);
let bad = 0, drift = 0;
const spec = (c) => {
  const s = String.fromCharCode(c), u = s.toUpperCase();
  if (u.length !== 1) return c;
  const cu = u.charCodeAt(0);
  return c >= 128 && cu < 128 ? c : cu;
};
for (let c = 0; c < 65536; c++) if (got[c] !== spec(c)) { if (DRIFT.has(c)) { drift++; continue; } if (bad++ < 10) console.log('canon differs', c.toString(16), got[c].toString(16), spec(c).toString(16)); }
const groups = new Map();
for (let c = 0; c < 65536; c++) (groups.get(got[c]) ?? groups.set(got[c], []).get(got[c])).push(c);
let checked = 0, multi = 0;
const reps = [...groups.keys()];
for (const [k, members] of groups) {
  if (members.length < 2) continue;
  multi++;
  for (const a of members) {
    if (DRIFT.has(a) || members.some((m) => DRIFT.has(m))) continue;
    const re = new RegExp('^' + (a < 0x20 || (a >= 0xd800 && a < 0xe000) ? '\\u' + a.toString(16).padStart(4, '0') : String.fromCharCode(a).replace(/[\\^$.*+?()[\]{}|\/]/, '\\$&')) + '$', 'i');
    for (const b of members) { checked++; if (!re.test(String.fromCharCode(b))) { if (bad++ < 10) console.log('node says no match', a.toString(16), b.toString(16)); } }
    for (let t = 0; t < 3; t++) {
      const other = reps[(a * 7 + t * 12345) % reps.length];
      if (other !== k) { checked++; if (re.test(String.fromCharCode(other))) { if (bad++ < 10) console.log('node says match', a.toString(16), other.toString(16)); } }
    }
  }
}
console.log(`canon: bad=${bad} unicode-version drift units=${drift} multi-member classes=${multi} regexp checks=${checked}`);
process.exit(bad ? 1 : 0);
