#!/bin/sh
# Compare Proxy behavior with Node (vm, empty context) on tools/fixtures/js/proxy-cases.txt. The `order_*` cases are the
# trap-order matrix: they log the order of the traps for each operation. It reuses the promise tools: their cases end with
# a reader function. Only our own tiny programs run under Node. Never run a fetched bundle with this tool.
#   tools/js/proxy_check.sh [--update]
set -eu
here=$(dirname "$0")
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
cargo build --release -q -p bare-js --example promise_check
cases=$here/../fixtures/js/proxy-cases.txt
node "$here/promise_oracle.mjs" "$cases" > "$tmp/o"
if [ "${1:-}" = "--update" ]; then cp "$tmp/o" "$here/../fixtures/js/proxy-cases.expected"; fi
target/release/examples/promise_check "$cases" > "$tmp/m"
if diff "$tmp/o" "$tmp/m" > "$tmp/d"; then
  echo "proxy cases: $(wc -l < "$tmp/m") match"
else
  echo "proxy cases: DIFFERENCES (< node, > bare-js)"; cat "$tmp/d"; exit 1
fi
