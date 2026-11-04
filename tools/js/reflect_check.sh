#!/bin/sh
# Compare Object.getOwnPropertyNames, Reflect.ownKeys and Reflect.construct with Node (vm, empty context) on
# tools/fixtures/js/reflect-cases.txt. It reuses the promise tools: their cases end with a reader function.
# Only our own tiny programs run under Node. Never run a fetched bundle with this tool.
#   tools/js/reflect_check.sh [--update]
set -eu
here=$(dirname "$0")
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
cargo build --release -q -p bare-js --example promise_check
cases=$here/../fixtures/js/reflect-cases.txt
node "$here/promise_oracle.mjs" "$cases" > "$tmp/o"
if [ "${1:-}" = "--update" ]; then cp "$tmp/o" "$here/../fixtures/js/reflect-cases.expected"; fi
target/release/examples/promise_check "$cases" > "$tmp/m"
if diff "$tmp/o" "$tmp/m" > "$tmp/d"; then
  echo "reflect cases: $(wc -l < "$tmp/m") match"
else
  echo "reflect cases: DIFFERENCES (< node, > bare-js)"; cat "$tmp/d"; exit 1
fi
