#!/bin/sh
# Compare the bare-js Array/Object/Math methods with Node (vm, empty context) on tools/fixtures/js/arrobj-cases.txt.
# Only our own tiny programs run under Node. Never run a fetched bundle with this tool.
#   tools/js/arrobj_check.sh
set -eu
here=$(dirname "$0")
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
cargo build --release -q -p bare-js --example eval_check
cases=$here/../fixtures/js/arrobj-cases.txt
node "$here/eval_oracle.mjs" "$cases" > "$tmp/o"
target/release/examples/eval_check "$cases" > "$tmp/m"
if diff "$tmp/o" "$tmp/m" > "$tmp/d"; then
  echo "arrobj cases: $(wc -l < "$tmp/m") match ($(grep -c 'limit$' "$tmp/m") limits, $(grep -c '	throw' "$tmp/m") throws)"
else
  echo "arrobj cases: DIFFERENCES (< node, > bare-js)"; cat "$tmp/d"; exit 1
fi
