// Oracle for the bare-js Promise subset. Runs each tiny case of tools/fixtures/js/promise-cases.txt in Node's `vm`
// (a fresh empty context, 2 s timeout). The last expression of a case is a reader function. The log is read
// before the microtask checkpoint (synchronous effects only) and after it (a macrotask turn runs the whole
// microtask queue): `name<TAB>pre ## post`. Only our own tiny programs run here: never pass a fetched bundle.
//   node tools/js/promise_oracle.mjs tools/fixtures/js/promise-cases.txt
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

process.on("unhandledRejection", () => {});

const esc = (s) => String(s).replace(/\\/g, "\\\\").replace(/\n/g, "\\n").replace(/\r/g, "\\r").replace(/\t/g, "\\t");
const turn = () => new Promise((r) => setImmediate(r));

export async function runCase(src) {
  try {
    const reader = vm.runInContext(src, vm.createContext({}), { timeout: 2000 });
    const pre = reader();
    await turn();
    return esc(pre) + " ## " + esc(reader());
  } catch (e) {
    if (e && e.code === "ERR_SCRIPT_EXECUTION_TIMEOUT") return "limit";
    if (e !== null && typeof e === "object") return "throw:" + esc(e.name) + ":" + esc(e.message);
    return "throw-value:" + esc(e);
  }
}

if (process.argv[2]) {
  for (const c of parseCases(fs.readFileSync(process.argv[2], "utf8"))) console.log(c.name + "\t" + (await runCase(c.src)));
}
