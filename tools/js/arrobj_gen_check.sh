#!/bin/sh
# Differential test: random Array/Object method programs (tools/js/arrobj_gen.mjs) in Node (vm) and bare-js.
#   tools/js/arrobj_gen_check.sh [COUNT] [SEED]
set -eu
here=$(dirname "$0")
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
cargo build --release -q -p bare-js --example eval_check
node "$here/arrobj_gen.mjs" "${1:-300}" "${2:-1}" > "$tmp/cases"
node "$here/eval_oracle.mjs" "$tmp/cases" > "$tmp/o"
target/release/examples/eval_check "$tmp/cases" > "$tmp/m"
if diff "$tmp/o" "$tmp/m" > "$tmp/d"; then
  echo "generated cases: $(wc -l < "$tmp/m") match ($(grep -c 'limit$' "$tmp/m") limits, $(grep -c '	throw' "$tmp/m") throws, $(grep -c 'unsupported' "$tmp/m") unsupported)"
else
  echo "generated cases: DIFFERENCES (< node, > bare-js)"; head -30 "$tmp/d"; exit 1
fi
