// Generates random Array-method programs (seeded, deterministic) in the cases format of eval-cases.txt.
// Node (vm) is the oracle: `tools/js/arrobj_gen_check.sh [COUNT] [SEED]` runs the same cases in both engines.
// Only programs made here run under Node. Never pass a fetched bundle to this tool.
const count = +process.argv[2] || 300;
let seed = +process.argv[3] || 1;
const rnd = (n) => { seed = (Math.imul(seed, 1103515245) + 12345) >>> 0; return (seed >>> 8) % n; };
const pick = (a) => a[rnd(a.length)];
const elem = () => pick(["1", "2", "3", "0", "-1", "'a'", "'b'", "'10'", "undefined", "null", "NaN", "-0", "true", "({k:1})"]);
const arr = () => "[" + Array.from({ length: rnd(7) }, () => (rnd(5) === 0 ? "" : elem())).join(",") + (rnd(6) === 0 ? ",," : "") + "]";
const idx = () => pick(["0", "1", "2", "-1", "-2", "5", "undefined", "NaN", "1.5", "-Infinity", "Infinity", "'1'"]);
// Only consistent total preorders: with an inconsistent comparator the order is implementation-defined.
const cmp = () => pick(["function(a,b){var x=String(a),y=String(b);return x<y?-1:x>y?1:0}", "function(a,b){var x=String(b),y=String(a);return x<y?-1:x>y?1:0}", "function(a,b){return String(a).length-String(b).length}", "undefined", "function(a,b){return 0}"]);
const cb = () => pick(["function(x){return x}", "function(x,i){return i%2==0}", "function(x){return typeof x==='number'}", "function(x,i,a){return a.length>i+1&&x}"]);
const op = () => pick([
  () => `a.splice(${idx()},${idx()}${rnd(2) ? "," + elem() + "," + elem() : ""})`,
  () => `a.splice(${idx()})`,
  () => `a.slice(${idx()},${idx()})`,
  () => `a.concat(${arr()},${elem()})`,
  () => `a.shift()`,
  () => `a.unshift(${elem()},${elem()})`,
  () => `a.sort(${cmp()})`,
  () => `a.indexOf(${elem()},${idx()})`,
  () => `a.includes(${elem()},${idx()})`,
  () => `a.map(${cb()})`,
  () => `a.filter(${cb()})`,
  () => `a.find(${cb()})`,
  () => `a.findIndex(${cb()})`,
  () => `a.some(${cb()})`,
  () => `a.every(${cb()})`,
  () => `a.reduce(function(p,c){return p+"|"+String(c)},"s")`,
  () => `Array.from(a,${cb()})`,
  () => `Object.entries(a)`,
  () => `Object.values(a)`,
  () => `Object.assign([],a)`,
  () => `Object.fromEntries(Object.entries(a))`,
  () => `a.push(${elem()})`,
  () => `a.pop()`,
])();
const show = `function S(v){ if (Array.isArray(v)) return "["+Object.keys(v).join(".")+"/"+v.length+"/"+v.map(String).join(",")+"]"; if (v && typeof v==="object") return "{"+Object.keys(v).join(",")+"}"; return String(v); }`;
for (let i = 0; i < count; i++) {
  const steps = Array.from({ length: 1 + rnd(4) }, () => op());
  console.log(`=== gen${i}`);
  console.log(`${show} var a=${arr()}, out=[]; ` + steps.map((s) => `try{ out.push(S(${s})); }catch(e){ out.push(e.name) } out.push(S(a));`).join(" ") + ` out.join(";")`);
}
