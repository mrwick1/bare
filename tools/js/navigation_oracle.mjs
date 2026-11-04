// Oracle for the URL and URLSearchParams part of the bare navigation module. Runs each tiny case of
// tools/fixtures/js/navigation-url-cases.txt in Node's `vm` (a fresh context that only gets Node's own URL and
// URLSearchParams, 2 s timeout). The last expression of a case is a reader function: `name<TAB>result`.
// Only our own tiny programs run here: never pass a fetched bundle.
//   node tools/js/navigation_oracle.mjs tools/fixtures/js/navigation-url-cases.txt
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

const esc = (s) => String(s).replace(/\\/g, "\\\\").replace(/\n/g, "\\n").replace(/\r/g, "\\r").replace(/\t/g, "\\t");

export function runCase(src) {
  try {
    const ctx = vm.createContext({ URL, URLSearchParams });
    return esc(vm.runInContext(src, ctx, { timeout: 2000 })());
  } catch (e) {
    if (e && e.code === "ERR_SCRIPT_EXECUTION_TIMEOUT") return "limit";
    if (e !== null && typeof e === "object") return "throw:" + esc(e.name);
    return "throw-value:" + esc(e);
  }
}

if (process.argv[2]) {
  for (const c of parseCases(fs.readFileSync(process.argv[2], "utf8"))) console.log(c.name + "\t" + runCase(c.src));
}
