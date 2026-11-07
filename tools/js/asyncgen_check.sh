#!/bin/sh
# Compare bare-js async generators and for-await with Node (vm, empty context) on tools/fixtures/js/asyncgen-cases.txt.
# The order of microtasks must match. Only our own tiny programs run under Node. Never run a fetched bundle with this tool.
#   tools/js/asyncgen_check.sh
set -eu
here=$(dirname "$0")
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
cargo build --release -q -p bare-js --example async_check
cases=$here/../fixtures/js/asyncgen-cases.txt
node "$here/promise_oracle.mjs" "$cases" > "$tmp/o"
"${CARGO_TARGET_DIR:-target}"/release/examples/async_check "$cases" > "$tmp/m"
if diff "$tmp/o" "$tmp/m" > "$tmp/d"; then
  echo "asyncgen cases: $(wc -l < "$tmp/m") match"
else
  echo "asyncgen cases: DIFFERENCES (< node, > bare-js)"; cat "$tmp/d"; exit 1
fi
