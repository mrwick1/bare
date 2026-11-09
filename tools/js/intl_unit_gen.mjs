// Generate English unit suffixes and currency names from Node's Intl data.
import fs from 'node:fs';
const source = fs.readFileSync('crates/js/src/eval_intl_num.rs', 'utf8');
const units = [...source.match(/const SANCTIONED_UNITS:[\s\S]*?= \[([\s\S]*?)\];/)[1].matchAll(/"([^"]+)"/g)].map(x => x[1]);
const q = JSON.stringify;
let out = '// English suffixes from Node Intl. Regenerate with tools/js/intl_unit_gen.mjs.\n';
out += 'pub const UNIT_NAMES: [&str; 45] = [' + units.map(q).join(',') + '];\n';
out += 'const UNIT_SUFFIXES: [[(&str, &str); 3]; 45] = [\n';
for (const unit of units) {
  out += '[' + ['short','narrow','long'].map(unitDisplay => {
    const f = new Intl.NumberFormat('en-US', {style:'unit',unit,unitDisplay});
    return '(' + [1,2].map(n => q(f.formatToParts(n).filter(p => ['literal','unit'].includes(p.type)).map(p => p.value).join(''))).join(',') + ')';
  }).join(',') + '],\n';
}
out += '];\nconst UNIT_DENOMINATORS: [[&str; 3]; 45] = [\n';
for (const unit of units) {
 out += '[' + ['short','narrow','long'].map(unitDisplay => {
 const f = new Intl.NumberFormat('en-US', {style:'unit',unit:'meter-per-'+unit,unitDisplay});
 const s = f.formatToParts(2).find(p => p.type === 'unit').value;
 return q(unitDisplay === 'long' ? s.split(' per ')[1] : s.split('/')[1]);
 }).join(',') + '],\n';
}
out += '];\nconst CURRENCY_NAMES: &[(&str, &str, &str)] = &[\n';
for (const currency of Intl.supportedValuesOf('currency')) {
 const f = new Intl.NumberFormat('en-US', {style:'currency',currency,currencyDisplay:'name',maximumFractionDigits:0});
 out += '(' + [currency,...[1,2].map(n=>f.formatToParts(n).find(p=>p.type==='currency').value)].map(q).join(',') + '),\n';
}
out += '];\n';
fs.writeFileSync('crates/js/src/intl_unit_data.rs', out);
