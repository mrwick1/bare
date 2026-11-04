// Token oracle for crates/js (S2). Reads one JS file, parses it with the installed Acorn
// (ecma2022, script), and prints one line per token at the granularity of bare-js:
//   <class> <goal> <start> <end> <newline_before>      offsets are UTF-8 BYTE offsets
// class: i name/keyword, p #private, n number, s string, r regexp, t template piece, x punctuator
// goal:  D slash is division, R regexp goal, T template continuation (a `}` that ends a `${`)
// Acorn splits a template into "`", chunk, "${", "}" ...; this script joins them to one piece like bare-js does.
// The file is only parsed, never run. Needs Acorn: `ACORN=/path/to/acorn.js node tools/js_lex_oracle.mjs FILE`
//   (default: `import('acorn')`).
import { readFileSync } from 'node:fs';

const acorn = await (process.env.ACORN ? import(process.env.ACORN) : import('acorn'));
const file = process.argv[2];
const src = readFileSync(file, 'utf8');
// UTF-16 offset -> byte offset.
const byteAt = new Uint32Array(src.length + 1);
for (let i = 0, b = 0; i < src.length; ) {
  const cp = src.codePointAt(i), n = cp > 0xffff ? 2 : 1, len = cp < 0x80 ? 1 : cp < 0x800 ? 2 : cp < 0x10000 ? 3 : 4;
  for (let k = 0; k < n; k++) byteAt[i + k] = b;
  i += n; b += len;
  byteAt[i] = b;
}
const LT = new RegExp('[\\n\\r' + String.fromCharCode(0x2028, 0x2029) + ']'); // JS line terminators
const raw = [];
acorn.parse(src, { ecmaVersion: 2022, sourceType: 'script', allowHashBang: true, onToken: raw });

const out = [];
let prevEnd = 0;
const emit = (cls, goal, start, end) => {
  const nl = LT.test(src.slice(prevEnd, start)) ? 1 : 0;
  out.push(`${cls} ${goal} ${byteAt[start]} ${byteAt[end]} ${nl}`);
  prevEnd = end;
};
const stack = []; // `{` depth in each open `${`
for (let i = 0; i < raw.length; i++) {
  const t = raw[i], label = t.type.label;
  if (label === 'eof') break;
  if (label === '`' || (label === '}' && stack.length && stack[stack.length - 1] === 0)) {
    const head = label === '`', start = t.start;
    let j = i + 1;
    if (['template', 'invalidTemplate'].includes(raw[j].type.label)) j++;
    const close = raw[j].type.label;
    if (close === '${') { if (head) stack.push(0); }
    else if (close === '`') { if (!head) stack.pop(); }
    else throw new Error('template shape at ' + start);
    emit('t', head ? 'D' : 'T', start, raw[j].end);
    i = j;
    continue;
  }
  if (label === '{' && stack.length) stack[stack.length - 1]++;
  if (label === '}' && stack.length) stack[stack.length - 1]--;
  const cls = label === 'name' ? 'i' : t.type.keyword ? 'i' : label === 'privateId' ? 'p' : label === 'num' ? 'n'
    : label === 'string' ? 's' : label === 'regexp' ? 'r' : 'x';
  emit(cls, cls === 'r' ? 'R' : 'D', t.start, t.end);
}
console.log(`# file=${file} bytes=${Buffer.byteLength(src)} tokens=${out.length}`);
console.log(out.join('\n'));
