#!/bin/sh
# Full check of crates/js/src/regexp.rs without cargo (parent has not wired the module yet).
#   tools/js/regexp_check.sh [OUT_DIR]
# Needs rustc and node. (To rebuild regexp_inventory.jsonl, run regexp_inventory.mjs with ACORN set.) Reads only public patterns; no target bundle is run.
set -eu
here=$(cd "$(dirname "$0")" && pwd)
src=$here/../../crates/js/src/regexp.rs
out=${1:-$(mktemp -d)}
rustc --edition 2024 --test "$src" -o "$out/unit_debug" && "$out/unit_debug" -q
rustc --edition 2024 -O --test "$src" -o "$out/unit_release" && "$out/unit_release" -q
rustc --edition 2024 --test "$here/regexp_load.rs" -o "$out/oracle_debug" && "$out/oracle_debug" -q
rustc --edition 2024 -O "$here/regexp_load.rs" -o "$out/regexp_load"
"$out/regexp_load" canon-dump | node "$here/regexp_canon_check.mjs"
node "$here/regexp_oracle.mjs" --target "$here/regexp_inventory.jsonl" > "$out/target.tsv"
"$out/regexp_load" check "$out/target.tsv"
for seed in 11 12 13; do
  node "$here/regexp_oracle.mjs" $seed 20000 > "$out/fuzz$seed.tsv"
  "$out/regexp_load" check "$out/fuzz$seed.tsv"
done
# Pathological pattern: Node must hit its own timeout; our matcher must hit its step budget.
if timeout 5 node -e 'new RegExp("(a*)*b").test("a".repeat(40))' 2>/dev/null; then echo "node finished: not pathological?"; else echo "node: pathological (a*)*b stopped by 5 s timeout (expected)"; fi
"$out/regexp_load" budget 1
