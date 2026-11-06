// Oracle for the bare-js evaluator. Runs each tiny case of tools/fixtures/js/eval-cases.txt in Node's `vm`
// (a fresh empty context, 2 s timeout) and prints `name<TAB>result`. Only our own tiny programs run here:
// never pass a fetched bundle to this tool.
//   node tools/js/eval_oracle.mjs tools/fixtures/js/eval-cases.txt
import fs from "node:fs";
import vm from "node:vm";

export function parseCases(text) {
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
    case "bigint": return "bigint:" + String(v);
    default: return v === null ? "null" : "object";
  }
}

export function runCase(src) {
  try {
    return show(vm.runInContext(src, vm.createContext({}), { timeout: 2000 }));
  } catch (e) {
    if (e && e.code === "ERR_SCRIPT_EXECUTION_TIMEOUT") return "limit";
    // Our call-depth limit is not catchable. Node's equivalent is a RangeError, so a case that only recurses reads as `limit`.
    if (e && e.name === "RangeError" && e.message === "Maximum call stack size exceeded") return "limit";
    if (e !== null && typeof e === "object") return "throw:" + esc(String(e.name)) + ":" + esc(String(e.message));
    return "throw-value:" + show(e);
  }
}

if (process.argv[2]) {
  for (const c of parseCases(fs.readFileSync(process.argv[2], "utf8"))) console.log(c.name + "\t" + runCase(c.src));
}
