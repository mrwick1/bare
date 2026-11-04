// Oracle cases for crates/js/src/regexp.rs (S2). Node runs each case; Rust compares every span.
//   node tools/js/regexp_oracle.mjs [seed] [fuzz_count] > cases.tsv
//   node tools/js/regexp_oracle.mjs --target tools/js/regexp_inventory.jsonl > target.tsv
//     (every literal found in the 15 public bundles, each on ~50 controlled inputs, as g/d runs)
// Line: label TAB flags TAB pattern-hex TAB input-hex TAB lastIndex TAB expected
//   hex = UTF-16 code units, 4 hex digits each (so lone surrogates survive).
//   expected: E (SyntaxError) | N (no match) | s,e;s,e;-;... (group 0 first, - = unmatched)
//             | U (Node accepts; the Rust engine must answer "unsupported")
//             | L (Node accepts; the Rust engine must hit a compile bound: CompileError::Limit)
// Node always runs the pattern with the extra flags g and d, so lastIndex is honoured and spans
// are exact; the Rust engine searches from lastIndex the same way.
// Only Node's regexp engine decides the expected value. Nothing here is run from a target bundle.
import vm from 'node:vm';
const ctx = vm.createContext({});
const hex = (s) => [...Array(s.length)].map((_, i) => s.charCodeAt(i).toString(16).padStart(4, '0')).join('');
const show = (s) => JSON.stringify(s).replace(/\t/g, '\\t');
const out = [];
function add(label, flags, pat, input, li = 0, unsup = false) {
  // unsup: true = engine must say unsupported; 'L' = Node accepts, engine must hit a compile bound
  let exp;
  try {
    const re = new RegExp(pat, flags + (flags.includes('g') ? '' : 'g') + 'd');
    if (unsup) exp = unsup === 'L' ? 'L' : 'U';
    else {
      re.lastIndex = li;
      ctx.re = re; ctx.input = input;
      // A case that Node itself cannot finish in 300 ms is dropped, never guessed.
      let m;
      try { m = vm.runInContext('re.exec(input)', ctx, { timeout: 300 }); } catch (e) { if (e.code === 'ERR_SCRIPT_EXECUTION_TIMEOUT') return; throw e; }
      exp = m ? m.indices.map((x) => (x ? x[0] + ',' + x[1] : '-')).join(';') : 'N';
    }
  } catch (e) {
    exp = e instanceof SyntaxError ? 'E' : 'X' + e;
  }
  out.push([label + ' ' + show(pat), flags, hex(pat), hex(input), li, exp].join('\t'));
}
const H = [
  // [flags, pattern, input, lastIndex, unsupported]
  ['', 'abc', 'xxabcxx'], ['', '', ''], ['', '', 'abc', 2], ['', 'a*', 'baaa'], ['', 'a*?', 'aaa'], ['', 'a+?b', 'aaab'],
  ['', '(a*)*', 'b'], ['', '(a*)+', 'b'], ['', '(a*)?', 'b'], ['', '(z)((a+)?(b+)?(c))*', 'zaacbbbcac'],
  ['', '(?:a|ab)(?:c|bcd)(d*)', 'abcd'], ['', 'a|ab', 'abc'], ['', '(a|ab)(c|bcd)', 'abcd'],
  ['', '(a)|b', 'b'], ['', '(a)|(b)', 'b'], ['', '(?:(a)|b)*', 'ab'], ['', '(?:(a)|(b))+', 'ab'], ['', '((a)|(b))*', 'abba'],
  ['', '(?=(a+))a*b\\1', 'baaabac'], ['', '(?=(a+))', 'baaabac'], ['', '(.*?)a(?!(a+)b\\2c)\\2(.*)', 'baaabaac'],
  ['', '(?!a)b', 'ab'], ['', '^\\/journeys\\/(?!new\\/?$)[^/]+\\/?$', '/journeys/abc/'], ['', '^\\/journeys\\/(?!new\\/?$)[^/]+\\/?$', '/journeys/new/'],
  ['', '^\\/journeys\\/(?!new\\/?$)[^/]+\\/?$', '/journeys/new2'], ['', '(?=a)*', 'a', 0, true], ['', '(?!a){2}', 'b', 0, true],
  ['', '(?<=a)b', 'ab', 0, true], ['', '(?<!a)b', 'ab', 0, true], ['', '(?<n>a)\\k<n>', 'aa', 0, true],
  ['u', 'a', 'a', 0, true], ['v', 'a', 'a', 0, true], ['gg', 'a', 'a'], ['x', 'a', 'a'],
  ['', '(', ''], ['', ')', ''], ['', '[', ''], ['', ']', ']'], ['', '}', '}'], ['', '{', '{'], ['', 'a{', 'a{'], ['', 'a{1', 'a{1'], ['', 'a{,5}', 'a{,5}'],
  ['', '{1}', ''], ['', 'a{2,1}', 'aa'], ['', '*', ''], ['', 'a**', ''], ['', '+a', ''], ['', '?', ''], ['', '^*', ''], ['', '\\b+', ''], ['', '$?', ''],
  ['', '\\', ''], ['', '(?', ''], ['', '(?x)', ''], ['', '[b-a]', 'a'], ['', '[\\d-x]', '-'], ['', '[\\d-x]', 'x'], ['', '[a-\\d]', '-'], ['', '[--a]', '-'],
  ['', 'a{2}', 'aaa'], ['', 'a{2,}', 'aaaa'], ['', 'a{1,2}?', 'aaa'], ['', 'a{0}', 'a'], ['', '(a){0}', 'a'], ['', '(?:a|b){2,3}', 'abab'],
  ['', '(a{1,2}){2}', 'aaaa'], ['', '(?:){5}', 'x'], ['', '(?:a*){2}', 'aa'], ['', 'a{4294967295}', 'a', 0, 'L'], ['', 'a{0,4294967296}', 'a', 0, 'L'],
  ['', '\\u{3}', 'uuu'], ['', '\\u0041', 'A'], ['', '\\u004', 'u004'], ['', '\\x41', 'A'], ['', '\\x4', 'x4'], ['', '\\cA', '\x01'], ['', '\\c', '\\c'], ['', '\\c1', '\\c1'], ['', '[\\c1]', '\x11'],
  ['', '[\\c]', 'c'], ['', '[\\c]', '\\'], ['', '\\0', '\0'], ['', '\\00', '\0'], ['', '\\08', '\x008'], ['', '\\101', 'A'], ['', '\\377', '\xff'], ['', '\\400', ' 0'], ['', '\\8', '8'],
  ['', '(a)\\1', 'aa'], ['', '\\1(a)', 'a'], ['', '(a\\1)', 'a'], ['', '(a)\\2', 'a\x02'], ['', '(a)\\10', 'a\x08'], ['', '\\1', '\x01'], ['', '[\\1]', '\x01'], ['', '[\\8]', '8'],
  ['', '\\p{L}', 'p{L}'], ['', '\\k', 'k'], ['', '\\-', '-'], ['', '\\a', 'a'], ['', '[\\b]', '\b'], ['', '\\B', 'a'], ['', '[\\B]', 'B'],
  ['', '.', '\n'], ['s', '.', '\n'], ['', '.', ' '], ['', '.', '😀'], ['', '^.$', '😀'], ['', '^..$', '😀'], ['', '😀', 'x😀'],
  ['', '😀+', '😀\ude00'], ['', '[😀]', '\ude00'], ['', '^[^x]$', '\ud83d'], ['', '\\ud83d', '😀'], ['', '\\ud83d\\ude00', '😀'], ['', '.', '\ud800'],
  ['', '^a', 'b\na'], ['m', '^a', 'b\na'], ['m', '^a', 'b\ra'], ['m', '^a', 'b a'], ['m', 'a$', 'a\nb'], ['', 'a$', 'a\n'], ['m', 'a$', 'a\r\nb'], ['m', '^$', '\n\n'],
  ['', '\\bfoo\\b', 'a foo b'], ['', '\\Boo', 'foo'], ['', '\\b', ''], ['', '\\B', ''], ['', '\\bé', 'é'], ['', '\\w+', 'é_x1'], ['', '\\W', 'é'],
  ['', '\\s', ' '], ['', '\\s', '﻿'], ['', '\\s', '᠎'], ['', '\\s', ' '], ['', '\\s', '​'], ['', '\\S+', ' ab\t'], ['', '\\d+', 'a123'], ['', '\\D+', '12ab3'], ['', '[\\s\\S]+', 'a\nb'],
  ['', '[^]', '\n'], ['', '[]', 'a'], ['', '[^]*', 'a\nb'], ['', '[\\w-]+', 'ab-c d'], ['', '[a-z-9]+', 'a-9z'], ['', '[a\\-z]+', 'a-z'], ['', '[.]', 'a.'], ['', '[$^]', '^'],
  ['i', 'abc', 'ABC'], ['i', 'ß', 'SS'], ['i', 'ß', 'ẞ'], ['i', 'ǆ', 'ǅ'], ['i', 'ǆ', 'Ǆ'], ['i', 'k', 'K'], ['i', 'K', 'k'], ['i', 'ſ', 's'], ['i', 's', 'ſ'], ['i', 'ſ', 'S'],
  ['i', 'é', 'É'], ['i', 'σ', 'ς'], ['i', 'ς', 'Σ'], ['i', 'µ', 'Μ'], ['i', 'ÿ', 'Ÿ'], ['i', '[a-z]+', 'ABC'], ['i', '[^a-z]', 'A'], ['i', '[^a-z]', '1'], ['i', '[\\u212a]', 'k'], ['i', '[k]', 'K'],
  ['i', '[ſ]', 'S'], ['i', '[a-z]', 'ſ'], ['i', '[a-z]', 'K'], ['i', '\\w', 'ſ'], ['i', '\\w', 'K'], ['i', '\\W', 'ſ'], ['i', '[\\W]', 'S'], ['i', '[^\\W]', 'ſ'], ['i', '(a)\\1', 'aA'], ['i', '(ß)\\1', 'ßSS'],
  ['i', '\\u0131', 'I'], ['i', 'i', 'İ'], ['i', 'ǰ', 'J'], ['i', 'Ω', 'Ω'], ['i', 'Ω', 'ω'], ['i', 'ᾳ', 'ᾼ'], ['i', '𐐀', '𐐨'], ['i', '\\u00e5', 'Å'],
  ['i', 'Googlebot|Bingbot', 'xgooglebotx'], ['i', '%3A', '%3a'],
  ['y', 'a', 'ba'], ['y', 'a', 'ba', 1], ['y', 'a', 'ab', 1], ['', 'a', 'ba', 1], ['', 'a', 'a', 2], ['', '', 'a', 1], ['', '', 'a', 2], ['y', '', '', 0], ['', '$', 'ab', 1],
  ['', 'x*', 'ab', 1], ['', '(?:)', 'ab', 2], ['', '()', 'ab', 1], ['', '(a)|b', 'xb', 0], ['', '(?:(a)|b)\\1', 'b'], ['', '(?:(a)|b)\\1', 'ab'],
  ['', '(a*)*', 'aaa'], ['', '(a*)+', 'aaa'], ['', '(a|b*)*', 'abba'], ['', '(?:a?)*?b', 'aab'], ['', '(?:a?)+?b', 'aab'], ['', '(a?)*?', 'aa'], ['', '(a?){2,3}', 'aa'], ['', '(a?){3}', 'a'],
  ['', '(?:(a)|(b)|(c))+', 'abc'], ['', '(?:(a)(b)?)+', 'aba'], ['', '(?:(a)|(b))*?c', 'abc'], ['', '((a)|b)+', 'ab'], ['', '(?=(a))\\1', 'a'], ['', '(?!(a))b', 'b'], ['', '(?=(a)|b)', 'b'], ['', '(?:(?=(a))a)+', 'aa'],
  ['', '(?=a(?=b))a', 'ab'], ['', '(?!a(?!b))a', 'ab'], ['', '(?!a(?!b))a', 'ac'], ['', '(?=(a*))\\1b', 'aab'], ['', '(?:(?=(a+))a)*b\\1', 'aab'],
  ['', '^(?:\\/(agents|campaigns|journeys|clients)(\\/|$))', '/agents/x'], ['', '^\\/clients\\/[^/]+\\/([^/]+)', '/clients/1/2'], ['', '^(\\d+(\\.\\d+)?)?(xs|sm|md|lg|xl)$', '1.5xl'], ['', '^(\\d+(\\.\\d+)?)?(xs|sm|md|lg|xl)$', 'xl'],
  ['', '^\\[(?:([a-z-]+):)?(.+)\\]$', '[a-b:c]'], ['', '\\[((?:\\[.*\\])|.+)\\]', 'x[[y]]z'], ['', '\\/\\[[^/]+?\\](?=\\/|$)', '/a/[b]/c'], ['', '\\.rsc($|\\?)', 'a.rsc?x'], ['', '\\((.*)\\)', 'f(a)(b)'],
  ['', '([a-z0-9])([A-Z])', 'fooBar'], ['', '\\r\\n?', 'a\r\nb'], ['', '\\u0000|\\uFFFD', 'a�'], ['', '[\\u0000-\\u0020\\u007f]', 'ab c'], ['', '[|\\\\{}()[\\]^$+*?.-]', 'a.b'], ['', '[\\n"\\\\]', 'a"b'],
  ['', '^[a-zA-Z][a-zA-Z\\d+\\-.]*?:', 'http://x'], ['', '\\n( *(at )?)', 'x\n  at y'], ['', '^(rgba?|hsla?|hwb|(ok)?(lab|lch)|color-mix)\\(.+\\)$', 'oklch(1 2 3)'],
  ['', '\\d+(%|px|r?em|[sdl]?v([hwib]|min|max)|pt|pc|in|cm|mm|cap|ch|ex|r?lh|cq(w|h|i|b|min|max))|\\b(calc|min|max|clamp)\\(.+\\)|^0$', 'calc(1px)'],
  ['', '^(2272|2461|3299|6393|7592)$', '3299'], ['', '^(2272|2461|3299|6393|7592)$', '3298'],
];
if (process.argv[2] === '--target') {
  const { readFileSync } = await import('node:fs');
  const probes = ['', 'a', 'A', ' ', '/', '//', '///x', 'a//b///c', '/journeys', '/journeys/', '/journeys/abc', '/journeys/abc/', '/journeys/new', '/journeys/new/', '/journeys/new2', '/calls', '/calls/12', '/calls/12/', '/results/', '/report-settings', '/agents/', '/clients/1/x', '/clients/1/x/y',
    'https://x.com/a?b#c', 'http://', 'ftp://h', 'a:b', 'x.rsc', 'x.rsc?y', 'Googlebot/2.1', 'GOOGLEBOT', 'mozilla slurp', 'fooBar', 'fooBarBaz', 'onClick', 'on1', 'Onclick', '1.5xl', 'xl', '12px', 'calc(1px)', 'min(1,2)', 'rgb(1,2,3)', 'oklch(1 2 3)', 'url(x)', 'linear-gradient(a)', 'repeating-conic-gradient(x)', 'inset_1px_2px', '-.5em_0',
    'a\r\nb'.replace('\\r\\n', '\r\n'), 'a\rb', 'a\nb', '\u0000x\uFFFD', ' a  b\t', '[a]', '[[x]]', '[a-b:c]', '[x]/[y]', '/a/[b]/c', '3299', '3298', '2272', '1/2', 'KeyA', 'DigitZ', 'a.b', '%3a', '%3A', 'a\\b', 'f(a)(b)', '()', 'x'.repeat(40), '1'.repeat(40), 'ab c', '\u007f', 'a-b_c d', 'é', '\ud800', '\u{1F600}'];
  for (const line of readFileSync(process.argv[3], 'utf8').trim().split('\n')) {
    const { file, start, pattern, flags } = JSON.parse(line);
    for (const s of probes) for (const li of [0, 1]) add(file.slice(0, 5) + '@' + start, flags, pattern, s, li);
  }
  console.log(out.join('\n'));
  process.exit(0);
}
for (const [f, p, s, li, u] of H) add('hand', f, p, s, li ?? 0, u);
// Seeded fuzz: small safe patterns over a tiny alphabet, short inputs. Node judges validity too.
let seed = Number(process.argv[2] ?? 1) >>> 0 || 1;
const rnd = (n) => ((seed = (Math.imul(seed, 1664525) + 1013904223) >>> 0), (seed >>> 8) % n);
const pick = (a) => a[rnd(a.length)];
const atoms = ['a', 'b', 'c', 'A', 'B', 'ß', 'ſ', 'k', 'x', ' ', '\\n', '\\r', '.', '[abc]', '[^a]', '[a-c]', '[\\d_]', '[^\\s]', '\\d', '\\w', '\\W', '\\s', '\\S', '\\b', '\\B', '^', '$', '\\1', '\\2', '\\u212a', '\\u017f', '[\\u212a-\\u2130]'];
const quants = ['', '', '', '*', '+', '?', '*?', '+?', '??', '{2}', '{1,2}', '{0,2}?', '{2,}', '{0}'];
function gen(d) {
  const n = 1 + rnd(3), alts = [];
  for (let k = 0; k < 1 + (rnd(4) === 0 ? 1 + rnd(2) : 0); k++) {
    let s = '';
    for (let i = 0; i < n; i++) {
      const r = rnd(10);
      let a;
      if (r < 6 || d > 2) a = pick(atoms);
      else if (r < 8) a = '(' + gen(d + 1) + ')';
      else if (r < 9) a = '(?:' + gen(d + 1) + ')';
      else a = (rnd(2) ? '(?=' : '(?!') + gen(d + 1) + ')';
      const q = /^(\^|\$|\\b|\\B)$/.test(a) || a.startsWith('(?=') || a.startsWith('(?!') ? '' : pick(quants);
      s += a + q;
    }
    alts.push(s);
  }
  return alts.join('|');
}
const alpha = ['a', 'b', 'c', 'A', 'B', 'x', ' ', '\n', '\r', '1', '_', 'ß', 'ſ', 'K', 'K', 'k', '\u{1F600}', '\ud800', '\ude00', ' ', 'S', 's'];
const nf = Number(process.argv[3] ?? 600);
for (let i = 0; i < nf; i++) {
  const p = gen(0);
  const flags = ['', 'i', 'm', 's', 'y', 'im', 'is', 'iy'][rnd(8)];
  const len = rnd(9);
  let s = '';
  for (let k = 0; k < len; k++) s += pick(alpha);
  add('fuzz', flags, p, s, rnd(3) === 0 ? rnd(len + 2) : 0);
}
console.log(out.join('\n'));
