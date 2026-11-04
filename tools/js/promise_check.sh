#!/bin/sh
# Compare the bare-js Promise subset with Node (vm, empty context) on tools/fixtures/js/promise-cases.txt.
# Only our own tiny programs run under Node. Never run a fetched bundle with this tool.
#   tools/js/promise_check.sh
set -eu
here=$(dirname "$0")
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
cargo build --release -q -p bare-js --example promise_check
cases=$here/../fixtures/js/promise-cases.txt
node "$here/promise_oracle.mjs" "$cases" > "$tmp/o"
target/release/examples/promise_check "$cases" > "$tmp/m"
if diff "$tmp/o" "$tmp/m" > "$tmp/d"; then
  echo "promise cases: $(wc -l < "$tmp/m") match"
else
  echo "promise cases: DIFFERENCES (< node, > bare-js)"; cat "$tmp/d"; exit 1
fi
