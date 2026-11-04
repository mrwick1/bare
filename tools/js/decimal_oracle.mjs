// Oracle for crates/js/src/number.rs (the decimal reduction behind lex, Number(string) and JSON).
// Node computes every expected bit pattern: Number(text) for lexer and string text, JSON.parse
// for JSON text. Only generated decimal text is written; no target bundle is read or run.
//
// Usage: node decimal_oracle.mjs <dir> [seed] [count]
// Writes <dir>/manifest.tsv (lines: expected-bits-hex, kind, file) and one file per case.
// Kinds: lex = one unsigned literal with `_` separators (lexer path), str = any sign (string path),
// json = JSON number text (JSON path).
import { mkdirSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';

const [dir, seedArg = '12345', countArg = '400'] = process.argv.slice(2);
if (!dir) {
  console.error('usage: node decimal_oracle.mjs <dir> [seed] [count]');
  process.exit(2);
}
mkdirSync(dir, { recursive: true });

let state = BigInt(seedArg);
const M64 = (1n << 64n) - 1n;
const rnd = () => {
  state = (state * 6364136223846793005n + 1442695040888963407n) & M64;
  return Number(state >> 11n) / 2 ** 53;
};
const int = (n) => Math.floor(rnd() * n);
const bitsOf = (x) => {
  const v = new DataView(new ArrayBuffer(8));
  v.setFloat64(0, x);
  return v.getBigUint64(0).toString(16).padStart(16, '0');
};
const zeros = (n) => '0'.repeat(n);

// Exact decimal of n * 2^e for BigInt n > 0: multiply by 5^k and move the point k places.
function exact(n, e) {
  if (e >= 0) return (n << BigInt(e)).toString();
  const k = -e;
  const s = (n * 5n ** BigInt(k)).toString().padStart(k + 1, '0');
  return `${s.slice(0, s.length - k)}.${s.slice(s.length - k)}`;
}

// The halfway point above the positive finite double with bits E (exponent) and F (fraction),
// as an exact decimal, and the same value minus one unit in its last place (still below it).
function halfway(E, F) {
  const m = E === 0 ? F : F + (1n << 52n);
  const e = E === 0 ? -1074 : E - 1075;
  const h = exact(2n * m + 1n, e - 1);
  const frac = h.includes('.') ? h.split('.')[1].length : 0;
  const d = BigInt(h.replace('.', '')) - 1n;
  let lower = d.toString().padStart(frac + 1, '0');
  if (frac) lower = `${lower.slice(0, lower.length - frac)}.${lower.slice(lower.length - frac)}`;
  return { h, lower };
}

const cases = [];
const add = (kind, text) => cases.push({ kind, text });

// 1. Halfway points: exact ties, and one step above and below, with and without long padding.
for (let t = 0; t < Number(countArg); t++) {
  const pick = int(10);
  let E;
  if (pick < 2) E = 0; // subnormal
  else if (pick < 3) E = 2046; // near MAX
  else E = 1 + int(2045);
  const { h, lower } = halfway(E, BigInt(Math.floor(rnd() * 2 ** 52)));
  const pad = [0, 0, 1100, 3000, 2 ** 13, 1_000_000][int(6)];
  const neg = rnd() < 0.3 ? '-' : '';
  const variant = int(3);
  const body = variant === 0 ? `${h}${zeros(pad)}` // exact tie
    : variant === 1 ? `${h}${zeros(pad)}1` // above the tie
    : `${lower}${pad ? `${lower.includes('.') ? '' : '.'}${'9'.repeat(pad)}` : ''}`; // below the tie
  const text = `${neg}${body}`;
  const kind = neg ? 'str' : ['lex', 'json', 'str'][int(3)];
  if (kind === 'lex') {
    // Separators between integer digits only (never first or last, never next to `.`).
    const [ip, fp] = body.split('.');
    // Linear: groups of three from the left, after a first group of length (len mod 3).
    const head = ip.length % 3;
    const parts = head ? [ip.slice(0, head)] : [];
    for (let i = head; i < ip.length && ip.length > 4; i += 3) parts.push(ip.slice(i, i + 3));
    const grouped = ip.length > 4 ? parts.join('_') : ip;
    add(kind, fp === undefined ? grouped : `${grouped}.${fp}`);
  } else {
    add(kind, text);
  }
}

// 2. Cancelling mantissas and long zero runs (the million-digit family), plus random short text.
for (const L of [1025, 1500, 2100, 4000, 30000, 1_000_000]) {
  add('lex', `${'9'.repeat(L)}.5e-${L}`);
  add('str', `${'9'.repeat(L)}.5e-${L}`);
  add('json', `1${zeros(L)}e-${L}`);
  add('lex', `0.${zeros(L)}1e${L}`);
  add('str', `-${'7'.repeat(L)}e-${L}`);
}
for (let t = 0; t < 200; t++) {
  const L = 1 + int(3000);
  let d = '1';
  for (let i = 1; i < L; i++) d += int(10);
  const dot = 1 + int(L);
  const e = int(800) - 400 - Math.floor(L / 3) * (int(2) === 0 ? 1 : 0);
  const s = `${d.slice(0, dot)}${dot < L ? `.${d.slice(dot)}` : ''}e${e}`;
  add(int(2) ? 'lex' : 'str', s);
}

const lines = [];
cases.forEach((c, i) => {
  const file = `case-${String(i).padStart(5, '0')}.txt`;
  writeFileSync(join(dir, file), c.text);
  const want = c.kind === 'json' ? JSON.parse(c.text) : Number(c.kind === 'lex' ? c.text.replace(/_/g, '') : c.text);
  lines.push(`${bitsOf(want)}\t${c.kind}\t${file}`);
});
writeFileSync(join(dir, 'manifest.tsv'), `${lines.join('\n')}\n`);
const tally = cases.reduce((a, c) => ((a[c.kind] = (a[c.kind] ?? 0) + 1), a), {});
console.log(`decimal oracle: ${cases.length} cases ${JSON.stringify(tally)} longest=${Math.max(...cases.map((c) => c.text.length))} units (seed ${seedArg})`);
