// List the standard JS built-ins of this Node and write a script that checks each one in bare-js.
// Output: a JS program for `eval_check -e`. It returns the missing names, one per line.
// Getters that throw on the prototype (for example Map.prototype.size) show as missing: check them by hand.
//   node tools/js/builtin_inventory.mjs > /tmp/inv.js
const roots = ['Object', 'Function', 'Array', 'String', 'Number', 'Boolean', 'Symbol', 'BigInt', 'Math', 'JSON', 'Reflect',
  'Promise', 'Map', 'Set', 'WeakMap', 'WeakSet', 'WeakRef', 'FinalizationRegistry', 'Date', 'RegExp', 'Error', 'TypeError',
  'RangeError', 'SyntaxError', 'ReferenceError', 'EvalError', 'URIError', 'AggregateError', 'ArrayBuffer', 'SharedArrayBuffer',
  'DataView', 'Uint8Array', 'Int8Array', 'Uint8ClampedArray', 'Int16Array', 'Uint16Array', 'Int32Array', 'Uint32Array',
  'Float32Array', 'Float64Array', 'BigInt64Array', 'BigUint64Array', 'Proxy', 'Intl', 'Atomics', 'Iterator'];
const globals = ['eval', 'parseInt', 'parseFloat', 'isNaN', 'isFinite', 'encodeURI', 'encodeURIComponent', 'decodeURI',
  'decodeURIComponent', 'escape', 'unescape'];
const skip = new Set(['length', 'name', 'prototype', 'caller', 'arguments', 'constructor']);
// Legacy RegExp statics such as RegExp.$& are not valid member names.
const legacy = /^RegExp\.(\$.*|input|lastMatch|lastParen|leftContext|rightContext)$/;
const out = [...globals];
const add = (path, o) => {
  for (const k of Reflect.ownKeys(o)) {
    if (typeof k === 'symbol') out.push(`${path}[Symbol.${k.description.replace('Symbol.', '')}]`);
    else if (!skip.has(k)) out.push(`${path}.${k}`);
  }
};
for (const r of roots) {
  const v = globalThis[r];
  if (!v) continue;
  out.push(r);
  add(r, v);
  if (v.prototype) add(`${r}.prototype`, v.prototype);
}
const TA = Object.getPrototypeOf(Uint8Array);
add('%TypedArray%', TA);
add('%TypedArray%.prototype', TA.prototype);
for (const k of Object.getOwnPropertyNames(Intl)) {
  const v = Intl[k];
  if (typeof v === 'function' && v.prototype) { add(`Intl.${k}`, v); add(`Intl.${k}.prototype`, v.prototype); }
}
const lines = out.filter((p) => !legacy.test(p)).map((p) => {
  const q = JSON.stringify(p);
  return `try { if (${p.replace('%TypedArray%', 'TA')} === undefined) m.push(${q}); } catch (e) { m.push(${q}); }`;
});
console.log(`var m = []; var TA; try { TA = Object.getPrototypeOf(Uint8Array); } catch (e) {}\n${lines.join('\n')}\nm.length + ' of ${lines.length} missing\\n' + m.join('\\n')`);
