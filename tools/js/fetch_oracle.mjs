// Oracle for the pure parts of the bare fetch layer (Headers, Request, Response and the byte reader of a response body).
// Runs each tiny case of tools/fixtures/js/fetch-cases.txt in Node's `vm` (a fresh context that only gets Node's own
// Headers, Request, Response and ReadableStream; 2 s timeout), waits 50 ms for promises, then calls the reader function that is the last
// expression: `name<TAB>result`. Only our own tiny programs run here: never pass a fetched bundle. No network is used.
// Node does not enforce forbidden request header names (it is not a browser), so those are only tested in Rust.
//   node tools/js/fetch_oracle.mjs tools/fixtures/js/fetch-cases.txt
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

export async function runCase(src) {
  try {
    const ctx = vm.createContext({ Headers, Request, Response, ReadableStream });
    const reader = vm.runInContext(src, ctx, { timeout: 2000 });
    await new Promise((r) => setTimeout(r, 50));
    return esc(reader());
  } catch (e) {
    if (e && e.code === "ERR_SCRIPT_EXECUTION_TIMEOUT") return "limit";
    if (e !== null && typeof e === "object") return "throw:" + esc(e.name);
    return "throw-value:" + esc(e);
  }
}

if (process.argv[2]) {
  for (const c of parseCases(fs.readFileSync(process.argv[2], "utf8"))) console.log(c.name + "\t" + (await runCase(c.src)));
}
