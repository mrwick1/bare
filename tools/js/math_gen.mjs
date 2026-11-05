// Inputs and Node results for tools/js/math_check.sh. Our own list only. One row per line:
// x, 20 unary results, atan2(x, y), pow(x, y / 1000), hypot(x, y), y.
let s = 12345n; const rnd = () => { s = (s * 6364136223846793005n + 1442695040888963407n) & ((1n << 64n) - 1n); return Number(s >> 11n) / 2 ** 53; };
const xs = [];
for (let i = 0; i < 20000; i++) { const r = rnd(); const k = i % 5; xs.push(k == 0 ? (r - 0.5) * 2 : k == 1 ? (r - 0.5) * 20 : k == 2 ? (r - 0.5) * 2000 : k == 3 ? r * 1e-5 : (r - 0.5) * 1e6); }
const fs = ["sin","cos","tan","asin","acos","atan","sinh","cosh","tanh","asinh","acosh","atanh","exp","expm1","log","log10","log2","log1p","cbrt","sqrt"];
const lines = [];
const fmt = v => Object.is(v, -0) ? "-0" : String(v);
const row = (x, y) => { const r = [x]; for (const f of fs) r.push(Math[f](x)); r.push(Math.atan2(x, y), Math.pow(x, y / 1000), Math.hypot(x, y), y); return r.map(fmt).join(" "); };
// 1. original random rows (unchanged)
for (const x of xs) { const y = xs[(lines.length * 7) % xs.length]; lines.push(row(x, y)); }
// 2. special values
const f64 = new Float64Array(1), u64 = new BigUint64Array(f64.buffer);
const fromBits = b => { u64[0] = b & ((1n << 64n) - 1n); return f64[0]; };
const next = (x, d) => { f64[0] = x; u64[0] += BigInt(d); return f64[0]; };
const sx = [0, -0, Infinity, -Infinity, NaN, 1, -1, 2, -2, 0.5, -0.5, 3, 10, -10, 22, 21.99999, 22.00001, 1e300, -1e300, -1e-300, 1e-300, 5e-324, -5e-324, 2.225073858507201e-308, 2.2250738585072014e-308, 1e-310, -1e-310,
  2 ** -27, 2 ** -28, 2 ** -29, 2 ** -54, 2 ** -55, 2 ** -1022, 2 ** 28, 2 ** 29, 2 ** 53, 2 ** 66, 2 ** 65, 2 ** 1023, 1.7976931348623157e308, 2 ** 31, 2 ** 32, 2 ** 63, 2 ** 64,
  709.78, 709.782712893384, 709.7827128933841, 710, 710.4758600739439, 710.4758600739440, 709.7822265625, -745.13, -745.1332191019411, -745.1332191019412, -708, -709, -745, -746, -56 * Math.LN2, 56 * Math.LN2,
  Math.LN2, Math.LN2 / 2, 1.5 * Math.LN2, Math.E, -Math.E, Math.SQRT2, Math.SQRT1_2, 1 + 2 ** -52, 1 - 2 ** -53, 0.975, 0.9750001, 0.4375, 0.6875, 1.1875, 2.4375, 0.67434, 0.6744,
  Math.PI / 4, Math.PI / 2, Math.PI, 3 * Math.PI / 4, 2 * Math.PI, 1e5, 1e6, 1e7, 1e8, 1e9, 1e15, 1e22, 1e23, 1e100, 1.5707963267948966e22, 3.141592653589793e22];
for (const k of [1, 2, 3, 4, 5, 8, 15, 16, 17, 31, 32, 33, 63753, 100000, 63661977236758134, 636619772367581343075535, 1e12, 1e15, 4e21, 6366197723675813430]) {
  const v = k * Math.PI / 2;
  for (const w of [v, -v, v / 2, v * 1.0000001]) sx.push(w);
  for (const d of [-2, -1, 0, 1, 2]) { sx.push(next(v, d)); sx.push(-next(v, d)); }
}
for (const k of [1e5 / (Math.PI / 2), 1e22 / (Math.PI / 2)]) { const kk = Math.round(k); for (let d = -3; d <= 3; d++) sx.push((kk + d) * Math.PI / 2); }
for (const b of [0x3FE921FB54442D18n, 0x400921FB54442D18n, 0x3FF921FB54442D18n, 0x4002D97C7F3321D2n, 0x413921FB54442D18n, 0x413921FB54442D19n, 0x4170000000000000n, 0x40862E42FEFA39EFn, 0x3FD62E42FEFA39EFn, 0x3FF0A2B2F3690000n])
  for (const d of [-2, -1, 0, 1, 2]) { const v = next(fromBits(b), d); sx.push(v, -v); }
const sy = [0, -0, Infinity, -Infinity, NaN, 1000, -1000, 2000, -2000, 3000, 500, -500, 1500, -1500, 2500, 100, -100, 1, -1, 1e-3, 1e6, -1e6, 1e12, 3e12, 4e15, 1e17, 1e18, 1e19, 1e21, 1e24, 1e25, 1e300, -1e300, 2147483648e3, 2147483647e3, 2147483649e3, 1e-300, 5e-324, 1e14, 3e14, 6e14, 1000 + 1e-13, 999.9999999999999, 1e10, 1e11 + 1, 0.5, 7000, -7000, 12345678, 2 ** 52 * 1000, 2 ** 53 * 1000, 2 ** 64 * 1000, 2 ** 65 * 1000];
for (const x of sx) for (const y of sy) lines.push(row(x, y));
// 3. random-ish: negative bases with integer / half-integer / non-integer exponents
for (let i = 0; i < 4000; i++) { const x = (rnd() - 0.5) * (i % 2 ? 20 : 2000); const e = [Math.round(rnd() * 40 - 20), Math.round(rnd() * 40 - 20) + 0.5, rnd() * 10 - 5, Math.round(rnd() * 2000 - 1000), Math.round(rnd() * 3e5)][i % 5]; lines.push(row(x, e * 1000)); }
// 4. random bit patterns, so every exponent range and branch is hit
for (let i = 0; i < 20000; i++) { const b = (BigInt(Math.floor(rnd() * 2 ** 32)) << 32n) | BigInt(Math.floor(rnd() * 2 ** 32)); const c = (BigInt(Math.floor(rnd() * 2 ** 32)) << 32n) | BigInt(Math.floor(rnd() * 2 ** 32)); lines.push(row(fromBits(b), fromBits(c))); }
// 5. dense ranges for exp/expm1/log/trig near thresholds
for (let i = 0; i < 6000; i++) { const x = [709 + rnd() * 2, -745 - rnd() * 1 + 0.5, (rnd() - 0.5) * 80, 1e5 * (1 + rnd() * 1e-3), 1e22 * (1 + (rnd() - 0.5) * 1e-2), 2 ** (rnd() * 120 - 60)][i % 6]; lines.push(row(x, [rnd() * 6 - 3, 1, 2][i % 3] * 1000)); }
console.log(lines.join("\n"));
