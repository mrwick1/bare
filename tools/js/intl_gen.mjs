// Generate crates/js/src/intl_data.rs: English date and time patterns from Node's ICU (CLDR data, Unicode license).
// Only Intl.DateTimeFormat of this Node runs here; no page code.
//   node tools/js/intl_gen.mjs > crates/js/src/intl_data.rs
//
// A pattern is text with fields `{Xw}`: X is w (weekday), y, M, d, the hour
// (h 1-12, K 0-11, H 0-23, k 1-24), m, s, a (day period), z (zone name, a gate);
// w is 1 (no padding, a full year), 2 (two digits) or n, s, l (narrow, short, long). Literals never contain `{`.
const LOCALES = ["en-US", "en", "en-CA", "en-IN", "en-GB"];
const REF = new Date(Date.UTC(2024, 0, 5, 3, 4, 7)); // Fri 2024-01-05 03:04:07: every number is one digit, so padding shows
const W = [undefined, "narrow", "short", "long"], Y = [undefined, "numeric", "2-digit"], MO = [undefined, "numeric", "2-digit", "narrow", "short", "long"], D = [undefined, "numeric", "2-digit"], N = [undefined, "numeric", "2-digit"];
const HC = ["h11", "h12", "h23", "h24"], STY = [undefined, "full", "long", "medium", "short"];
const fmt = (loc, o) => new Intl.DateTimeFormat(loc, { timeZone: "UTC", ...o });

const pool = new Map(); // pattern -> index (0 is the empty pattern)
const intern = (p) => { if (p.includes("\0")) throw new Error(p); if (!pool.has(p)) pool.set(p, pool.size); return pool.get(p); };
intern("");

function names(loc) {
  const months = { n: [], s: [], l: [] }, days = { n: [], s: [], l: [] };
  for (const [k, w] of [["n", "narrow"], ["s", "short"], ["l", "long"]]) {
    for (let m = 0; m < 12; m++) months[k].push(fmt(loc, { month: w, day: "numeric" }).formatToParts(new Date(Date.UTC(2024, m, 5))).find((p) => p.type === "month").value);
    for (let d = 0; d < 7; d++) days[k].push(fmt(loc, { weekday: w }).formatToParts(new Date(Date.UTC(2024, 0, 7 + d))).find((p) => p.type === "weekday").value);
  }
  const ap = [3, 15].map((h) => fmt(loc, { hour: "numeric", hour12: true }).formatToParts(new Date(Date.UTC(2024, 0, 5, h))).find((p) => p.type === "dayPeriod").value);
  return { months, days, ap };
}

// The hour symbol of a formatter: h (1-12), K (0-11), H (0-23) or k (1-24), from what it prints at 00:00 and 12:00.
function hourSym(f) {
  const at = (h) => f.formatToParts(new Date(Date.UTC(2024, 0, 5, h))).find((p) => p.type === "hour")?.value;
  const z = at(0), n = at(12);
  if (z === undefined) return "h";
  return +z === 12 ? "h" : +z === 24 ? "k" : +n === 0 ? "K" : "H";
}

function encode(f, parts, nm) {
  let out = "";
  const hs = hourSym(f);
  for (const p of parts) {
    const v = p.value;
    switch (p.type) {
      case "literal": if (v.includes("{")) throw new Error("brace in " + v); out += v; break;
      case "weekday": out += "{w" + (v === nm.days.n[5] ? "n" : v === nm.days.s[5] ? "s" : v === nm.days.l[5] ? "l" : bad(p)) + "}"; break;
      case "month": out += "{M" + (v === "1" ? "1" : v === "01" ? "2" : v === nm.months.n[0] ? "n" : v === nm.months.s[0] ? "s" : v === nm.months.l[0] ? "l" : bad(p)) + "}"; break;
      case "year": out += "{y" + (v === "2024" ? "1" : v === "24" ? "2" : bad(p)) + "}"; break;
      case "day": out += "{d" + (v === "5" ? "1" : v === "05" ? "2" : bad(p)) + "}"; break;
      case "hour": case "minute": case "second": {
        const want = { hour: ["3", "03"], minute: ["4", "04"], second: ["7", "07"] }[p.type];
        out += "{" + (p.type === "hour" ? hs : p.type[0]) + (v === want[0] ? "1" : v === want[1] ? "2" : bad(p)) + "}"; break;
      }
      case "dayPeriod": out += "{a1}"; break;
      case "timeZoneName": out += "{z1}"; break; // a gate at run time: no zone display names yet
      default: bad(p);
    }
  }
  return out;
}
function bad(p) { throw new Error("unexpected part " + JSON.stringify(p)); }

const date = [], time = [], join = [], exc = [], style = [], hcs = [], nameSets = [], locNames = [];
for (const loc of LOCALES) {
  const nm = names(loc);
  const key = JSON.stringify(nm);
  let ni = nameSets.findIndex((s) => JSON.stringify(s) === key);
  if (ni < 0) { ni = nameSets.length; nameSets.push(nm); }
  locNames.push(ni);
  const dRow = [], jRow = [], dOut = [];
  for (let i = 0; i < 216; i++) {
    const o = dateOpts(i);
    if (!o) { dRow.push(0); jRow.push(0); dOut.push(null); continue; }
    const f = fmt(loc, o);
    dRow.push(intern(encode(f, f.formatToParts(REF), nm)));
    dOut.push(f.format(REF));
    jRow.push(0);
  }
  const tRow = [], tOut = [];
  for (let i = 0; i < 108; i++) {
    const o = timeOpts(i);
    if (!o) { tRow.push(0); tOut.push(null); continue; }
    const f = fmt(loc, o);
    tRow.push(intern(encode(f, f.formatToParts(REF), nm)));
    tOut.push(f.format(REF));
  }
  // date + time: the date, a joiner (", " or " at "), the time; else a whole pattern in the exceptions
  const li = LOCALES.indexOf(loc);
  for (let di = 1; di < 216; di++) {
    if (!dOut[di]) continue;
    const joins = new Map();
    const whole = [];
    for (let ti = 1; ti < 108; ti++) {
      if (!tOut[ti]) continue;
      const f = fmt(loc, { ...dateOpts(di), ...timeOpts(ti) });
      const all = f.format(REF);
      whole.push([ti, all, f]);
      if (all.startsWith(dOut[di]) && all.endsWith(tOut[ti])) { const j = all.slice(dOut[di].length, all.length - tOut[ti].length); joins.set(j, (joins.get(j) || 0) + 1); }
    }
    let best = [...joins].sort((a, b) => b[1] - a[1])[0]?.[0] ?? ", ";
    if (best !== ", " && best !== " at ") throw new Error("joiner " + JSON.stringify(best));
    jRow[di] = best === " at " ? 1 : 0;
    for (const [ti, all, f] of whole) {
      if (all === dOut[di] + best + tOut[ti]) continue;
      exc.push([(li << 16) | (di << 7) | ti, intern(encode(f, f.formatToParts(REF), nm))]);
    }
  }
  const sRow = [];
  for (let hc = 0; hc < 4; hc++) {
    const r = [];
    for (let ds = 0; ds < 5; ds++) for (let ts = 0; ts < 5; ts++) {
      if (!ds && !ts) { r.push(0); continue; }
      const f = fmt(loc, { dateStyle: STY[ds], timeStyle: STY[ts], hourCycle: HC[hc] });
      r.push(intern(encode(f, f.formatToParts(REF), nm)));
    }
    sRow.push("[" + r.join(", ") + "]");
  }
  // resolved hour cycle for hour12 (undefined, true, false) x hourCycle (undefined, h11, h12, h23, h24)
  const hRow = [];
  for (const h12 of [undefined, true, false]) for (const hc of [undefined, ...HC]) hRow.push(HC.indexOf(fmt(loc, { hour: "numeric", hour12: h12, hourCycle: hc }).resolvedOptions().hourCycle));
  date.push(dRow); time.push(tRow); join.push(jRow); style.push(sRow); hcs.push(hRow);
}

function dateOpts(i) {
  const d = i % 3, m = Math.floor(i / 3) % 6, y = Math.floor(i / 18) % 3, w = Math.floor(i / 54);
  if (!w && !y && !m && !d) return null;
  return { weekday: W[w], year: Y[y], month: MO[m], day: D[d] };
}
function timeOpts(i) {
  const s = i % 3, mi = Math.floor(i / 3) % 3, h = Math.floor(i / 9) % 3, hc = Math.floor(i / 27);
  if (!h && !mi && !s) return null;
  return { hour: N[h], minute: N[mi], second: N[s], hourCycle: HC[hc] };
}

exc.sort((a, b) => a[0] - b[0]);
const text = [...pool.keys()];
let at = [0], all = "";
for (const t of text) { all += t; at.push(Buffer.byteLength(all)); }
const rs = (s) => JSON.stringify(s).replace(/\\u([0-9a-f]{4})/g, (_, h) => "\\u{" + h + "}");
const rows = (a) => a.map((r) => "    [" + r.join(", ") + "],").join("\n");
let o = `// Generated by tools/js/intl_gen.mjs from Node ${process.version} (ICU ${process.versions.icu}, CLDR ${process.versions.cldr}). Do not edit.
// English date and time patterns and names (CLDR data, Unicode license: data/unicode/LICENSE.txt). The format is in the generator.

/// Supported locales, in the order of the rows below. The first is the default.
pub const LOCALES: [&str; ${LOCALES.length}] = [${LOCALES.map(rs).join(", ")}];
/// All patterns, one after the other. Pattern \`i\` is \`POOL[AT[i]..AT[i + 1]]\`; pattern 0 is empty.
pub const POOL: &str = ${rs(all)};
pub const AT: [u16; ${at.length}] = [${at.join(", ")}];
/// Date pattern by locale and date index (weekday * 54 + year * 18 + month * 3 + day; each 0 is "absent").
pub const DATE: [[u16; 216]; ${LOCALES.length}] = [
${rows(date)}
];
/// Joiner of a date and a time by locale and date index: 0 ", ", 1 " at ".
pub const JOIN: [[u8; 216]; ${LOCALES.length}] = [
${rows(join)}
];
/// Time pattern by locale and time index (hour cycle * 27 + hour * 9 + minute * 3 + second; hour cycle 0 h11, 1 h12, 2 h23, 3 h24).
pub const TIME: [[u16; 108]; ${LOCALES.length}] = [
${rows(time)}
];
/// Date and time pairs that are not date + joiner + time: (locale << 16 | date << 7 | time, pattern), sorted.
pub const EXCEPT: [(u32, u16); ${exc.length}] = [${exc.map(([k, p]) => `(${k}, ${p})`).join(", ")}];
/// \`dateStyle\` and \`timeStyle\` patterns by locale, hour cycle, and dateStyle * 5 + timeStyle (0 none, 1 full, 2 long, 3 medium,
/// 4 short).
pub const STYLE: [[[u16; 25]; 4]; ${LOCALES.length}] = [
${rows(style)}
];
/// Resolved hour cycle (0 h11, 1 h12, 2 h23, 3 h24) by locale and hour12 (none, true, false) * 5 + hourCycle (none, h11, h12, h23, h24).
pub const HOUR_CYCLE: [[u8; 15]; ${LOCALES.length}] = [
${rows(hcs)}
];
/// Names: 12 months narrow, short, long; 7 weekdays (Sunday first) narrow, short, long; AM, PM.
pub const NAMES: [[&str; 59]; ${nameSets.length}] = [
${nameSets.map((n) => "    [" + [...n.months.n, ...n.months.s, ...n.months.l, ...n.days.n, ...n.days.s, ...n.days.l, ...n.ap].map(rs).join(", ") + "],").join("\n")}
];
/// The names row of each locale.
pub const LOCALE_NAMES: [u8; ${LOCALES.length}] = [${locNames.join(", ")}];
`;
process.stdout.write(o);
console.error(`patterns ${pool.size}, pool bytes ${Buffer.byteLength(all)}, exceptions ${exc.length}, name sets ${nameSets.length}`);
