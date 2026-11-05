#!/bin/sh
# Authored Node checksums, then 12 cases at 1x/10x/100x in capped fresh processes.
set -eu
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
cargo build --release -q -p bare-js --example wrapper_load
node - <<'JS' > "$work/cases"
const bodies = {
  empty: () => 0,
  primcall: (i) => i.toFixed(0).length + 'abc'.length,
  newstring: (i) => new String('x' + i).length,
  newnumber: (i) => new Number(i).valueOf(),
  newbool: (i) => (new Boolean(i % 2).valueOf() ? 1 : 0),
  objectprim: (i) => (typeof Object(i) === 'object' ? 1 : 0),
  strindex: (i) => new String('abcdef')[i % 6].length,
  strkeys: () => Object.keys(new String('abcdef')).length,
  wrapcall: (i) => new Number(i).toFixed(1).length,
  looseeq: (i) => (new Number(i) == i ? 1 : 0),
  json: () => JSON.stringify(new String('a')).length,
  forin: () => { let n = 0; for (const k in new String('abc')) n++; return n; },
};
for (const n of [1000, 10000, 100000]) {
  for (const [kind, f] of Object.entries(bodies)) {
    let sum = 0;
    for (let i = 0; i < n; i++) sum += f(i);
    console.log(kind, n, sum);
  }
}
JS
while read -r kind n expected; do
  (ulimit -v 1048576; timeout 45 target/release/examples/wrapper_load "$kind" "$n" "$expected") >> "$work/out"
done < "$work/cases"
test "$(wc -l < "$work/out")" -eq 36
cat "$work/out"
