// Oracle for the bare-js byte buffers and text codecs. Runs each tiny case of tools/fixtures/js/binary-cases.txt in
// Node's `vm` (a fresh context with only Node's TextEncoder and TextDecoder added, 2 s timeout) and prints
// `name<TAB>result`, the format of eval_oracle.mjs. Only our own tiny programs run here: never pass a fetched bundle.
//   node tools/js/binary_oracle.mjs tools/fixtures/js/binary-cases.txt
import fs from "node:fs";
import vm from "node:vm";

function parseCases(text) {
  const cases = [];
  for (const line of text.split("\n")) {
    if (line.startsWith("=== ")) cases.push({ name: line.slice(4).trim(), lines: [] });
    else if (cases.length) cases[cases.length - 1].lines.push(line);
  }
  return cases.map((c) => ({ name: c.name, src: c.lines.join("\n").replace(/\n+$/, "") }));
}

const esc = (s) => s.replace(/\\/g, "\\\\").replace(/\n/g, "\\n").replace(/\r/g, "\\r").replace(/\t/g, "\\t");

function show(v) {
  switch (typeof v) {
    case "string": return "string:" + esc(v);
    case "number": return "number:" + (Object.is(v, -0) ? "-0" : String(v));
    case "boolean": return "boolean:" + v;
    case "undefined": return "undefined";
    case "function": return "function";
    default: return v === null ? "null" : "object";
  }
}

export function runCase(src) {
  try {
    return show(vm.runInContext(src, vm.createContext({ TextEncoder, TextDecoder }), { timeout: 2000 }));
  } catch (e) {
    if (e && e.code === "ERR_SCRIPT_EXECUTION_TIMEOUT") return "limit";
    if (e !== null && typeof e === "object") return "throw:" + esc(String(e.name)) + ":" + esc(String(e.message));
    return "throw-value:" + show(e);
  }
}

if (process.argv[2]) {
  for (const c of parseCases(fs.readFileSync(process.argv[2], "utf8"))) console.log(c.name + "\t" + runCase(c.src));
}
