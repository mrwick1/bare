#!/bin/sh
# Compare the bare-js async/await subset with Node (vm, empty context) on tools/fixtures/js/async-cases.txt.
# Only our own tiny programs run under Node. Never run a fetched bundle with this tool.
#   tools/js/async_check.sh
set -eu
here=$(dirname "$0")
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
cargo build --release -q -p bare-js --example async_check
cases=$here/../fixtures/js/async-cases.txt
node "$here/promise_oracle.mjs" "$cases" > "$tmp/o"
target/release/examples/async_check "$cases" > "$tmp/m"
if diff "$tmp/o" "$tmp/m" > "$tmp/d"; then
  echo "async cases: $(wc -l < "$tmp/m") match"
else
  echo "async cases: DIFFERENCES (< node, > bare-js)"; cat "$tmp/d"; exit 1
fi
