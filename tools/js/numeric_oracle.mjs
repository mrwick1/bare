// Node oracle for Number.prototype.toString(radix) and Math.clz32/log. Runs only this authored list, never fetched code.
// Usage: node numeric_oracle.mjs > ../fixtures/js/numeric-cases.txt   (lines: `literal<TAB>radix results joined by |`)
const radixes = [2, 3, 7, 8, 16, 32, 36];
const f64 = new Float64Array(1), u64 = new BigUint64Array(f64.buffer);
let s = 0x9e3779b97f4a7c15n;
const next = () => { s ^= s << 13n; s &= 0xffffffffffffffffn; s ^= s >> 7n; s ^= s << 17n; s &= 0xffffffffffffffffn; return s; };
const vals = [0.1, 0.5, 1.5, 255.5, 1 / 3, 2 / 3, 0.000001, 123.456, 1e21, 1e22, 2 ** 53, 2 ** 53 + 2, 2 ** 64, 2 ** 100, 2 ** 1023,
  1.7976931348623157e308, 5e-324, 2.2250738585072014e-308, 2.225073858507201e-308, 1e-7, 1 - 2 ** -53, 1 + 2 ** -52, 4294967295, 4294967296,
  35, 36, 37, 0.9999999999999999, 9007199254740991, 1e300, 123456789012345680000, 0.30000000000000004];
for (let i = 0; i < 60; i++) { for (const e of [0, 1, 2, 3, 5]) { u64[0] = (BigInt(e === 0 ? 0 : 1023 - 60 + i * 2 + e * 7) << 52n) | (next() & 0xfffffffffffffn); vals.push(f64[0]); } }
for (const v of [...vals, ...vals.map(v => -v)]) {
  if (!isFinite(v) || v === 0) continue;
  console.log(String(v) + "\t" + radixes.map(r => v.toString(r)).join("|"));
}
