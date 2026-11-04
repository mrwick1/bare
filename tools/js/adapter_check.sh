#!/bin/sh
# Compare the bare-js byte iterator, tag and species adapters with Node (vm) on tools/fixtures/js/adapter-cases.txt.
# Only our own tiny programs run under Node. Never run a fetched bundle with this tool.
#   tools/js/adapter_check.sh
set -eu
here=$(dirname "$0")
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
cargo build --release -q -p bare-js --example eval_check
cases=$here/../fixtures/js/adapter-cases.txt
node "$here/binary_oracle.mjs" "$cases" > "$tmp/o"
target/release/examples/eval_check "$cases" > "$tmp/m"
if diff "$tmp/o" "$tmp/m" > "$tmp/d"; then
  echo "adapter cases: $(wc -l < "$tmp/m") match ($(grep -c 'limit$' "$tmp/m") limits, $(grep -c '	throw' "$tmp/m") throws)"
else
  echo "adapter cases: DIFFERENCES (< node, > bare-js)"; cat "$tmp/d"; exit 1
fi
# Generic iterators across await (Node vm, empty context; promise_oracle.mjs reads the log before and after the checkpoint).
cargo build --release -q -p bare-js --example async_check
cases=$here/../fixtures/js/adapter-async-cases.txt
node "$here/promise_oracle.mjs" "$cases" > "$tmp/o2"
target/release/examples/async_check "$cases" > "$tmp/m2"
if diff "$tmp/o2" "$tmp/m2" > "$tmp/d2"; then
  echo "adapter async cases: $(wc -l < "$tmp/m2") match"
else
  echo "adapter async cases: DIFFERENCES (< node, > bare-js)"; cat "$tmp/d2"; exit 1
fi
