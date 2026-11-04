#!/bin/sh
# Compare the bare-js class and async interaction cases with Node (vm, empty context) on tools/fixtures/js/interact-cases.txt.
# Only our own tiny programs run under Node. Never run a fetched bundle with this tool.
#   tools/js/interact_check.sh
set -eu
here=$(dirname "$0")
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
cargo build --release -q -p bare-js --example async_check
cases=$here/../fixtures/js/interact-cases.txt
node "$here/promise_oracle.mjs" "$cases" > "$tmp/o"
target/release/examples/async_check "$cases" > "$tmp/m"
if diff "$tmp/o" "$tmp/m" > "$tmp/d"; then
  echo "interact cases: $(wc -l < "$tmp/m") match"
else
  echo "interact cases: DIFFERENCES (< node, > bare-js)"; cat "$tmp/d"; exit 1
fi
