#!/bin/sh
# Compare the bare-js typed arrays, DataView and ArrayBuffer additions with Node (vm) on tools/fixtures/js/typed-cases.txt.
# Only our own tiny programs run under Node. Never run a fetched bundle with this tool.
#   tools/js/typed_check.sh
set -eu
here=$(dirname "$0")
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
cargo build --release -q -p bare-js --example eval_check
cases=$here/../fixtures/js/typed-cases.txt
node "$here/binary_oracle.mjs" "$cases" > "$tmp/o"
EVAL_CHECK_STEPS=2000000000 target/release/examples/eval_check "$cases" > "$tmp/m"
if diff -a "$tmp/o" "$tmp/m" > "$tmp/d"; then
  echo "typed cases: $(wc -l < "$tmp/m") match ($(grep -c 'limit$' "$tmp/m" || true) limits, $(grep -c '	throw' "$tmp/m" || true) throws)"
else
  echo "typed cases: DIFFERENCES (< node, > bare-js)"; cut -c1-1500 "$tmp/d"; exit 1
fi
