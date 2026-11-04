// Per-pass checksums for regexp_load.rs, computed by Node on the same workloads.
//   node tools/js/regexp_load_expect.mjs MODE      (MODE = short | caps | near) prints one number
// checksum: h = h*31 + v (u32 wrapping); a match adds start+1, end+1 for every group (0 if unset);
// "no match" adds 7. Global patterns iterate like String.prototype.match (lastIndex = end, +1 if empty).
const W = {
  short: [
    ['^\\/journeys\\/(?!new\\/?$)[^/]+\\/?$', '', ['/journeys/abc/', '/journeys/new', '/journeys/new/', '/calls/12']],
    ['\\/+', 'g', ['a//b///c', '/x']],
    ['([a-z0-9])([A-Z])', 'g', ['fooBarBaz', 'x1Y2Z']],
    ['^(\\d+(\\.\\d+)?)?(xs|sm|md|lg|xl)$', '', ['1.5xl', 'xl', '12px']],
    ['Googlebot|Mediapartners-Google|Bingbot|Slurp|DuckDuckBot|yandex', 'i', ['Mozilla/5.0 (compatible; GOOGLEBOT/2.1)', 'Mozilla/5.0 Firefox']],
    ['\\r\\n?', 'g', ['a\r\nb\rc\nd']],
    ['\\W', 'g', ['a-b_c d']],
    ['^\\/(agents|campaigns|journeys|clients)(\\/|$)', '', ['/journeys/9', '/clients', '/x']],
  ],
  caps: [
    ['^(\\w+)\\s+(\\d+)-(?:(a)|(b)|(c))\\s*(.*?)\\s*$', '', ['user 123-b  hello world  ', 'x 9-c', 'x 9-z']],
    ['(?:(\\d+)|([a-z]+)|(-))+', '', ['12ab-34cd-56ef-78gh-90ij']],
    ['(a)|(b)|(c)|(d)|(e)|(f)', 'g', ['abcdefabcdef']],
    ['^(?:(?:([a-z]+):)?\\/\\/([^/?#]*))?([^?#]*)(?:\\?([^#]*))?(?:#(.*))?$', '', ['https://example.com/a/b?x=1&y=2#frag', '/rel?q']],
  ],
  near: [['(\\d+)px', '', ['1'.repeat(1000)]], ['(?:a|b)*c', '', ['ab'.repeat(300)]]],
};
let h = 0;
const fold = (v) => { h = (Math.imul(h, 31) + v) >>> 0; };
for (const [p, f, ins] of W[process.argv[2]]) {
  const re = new RegExp(p, f.replace('g', '') + 'gd');
  for (const s of ins) {
    re.lastIndex = 0;
    for (;;) {
      const m = re.exec(s);
      if (!m) { fold(7); break; }
      for (const ix of m.indices) { if (ix) { fold(ix[0] + 1); fold(ix[1] + 1); } else fold(0); }
      if (!f.includes('g')) break;
      re.lastIndex = m.index === re.lastIndex ? re.lastIndex + 1 : re.lastIndex;
    }
  }
}
console.log(h);
