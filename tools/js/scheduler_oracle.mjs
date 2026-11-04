// Oracle for the scheduler of the bare browser host. Runs each tiny case of tools/fixtures/js/scheduler-cases.txt in
// Node's `vm` (a fresh context that only gets Node's own timers, queueMicrotask and MessageChannel). It waits 200 ms of
// real time for the case to finish (every case uses delays of 50 ms or less), then calls the reader function that is the
// last expression of the case: `name<TAB>result`.
// Only our own tiny programs run here: never pass a fetched bundle.
//   node tools/js/scheduler_oracle.mjs tools/fixtures/js/scheduler-cases.txt
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
    const ctx = vm.createContext({ setTimeout, clearTimeout, setInterval, clearInterval, queueMicrotask, MessageChannel });
    const reader = vm.runInContext(src, ctx, { timeout: 2000 });
    await new Promise((r) => setTimeout(r, 200));
    return esc(reader());
  } catch (e) {
    if (e !== null && typeof e === "object") return "throw:" + esc(e.name) + ":" + esc(e.message);
    return "throw-value:" + esc(e);
  }
}

if (process.argv[2]) {
  for (const c of parseCases(fs.readFileSync(process.argv[2], "utf8"))) console.log(c.name + "\t" + (await runCase(c.src)));
  process.exit(0);
}
