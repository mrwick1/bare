#!/bin/sh
# Compare String.prototype.matchAll and RegExp.prototype[Symbol.matchAll] with Node (vm, empty context) on
# tools/fixtures/js/matchall-cases.txt. It reuses the promise tools: their cases end with a reader function. Only our own
# tiny programs run under Node. Never run a fetched bundle with this tool.
#   tools/js/matchall_check.sh [--update]
set -eu
here=$(dirname "$0")
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
cargo build --release -q -p bare-js --example promise_check
cases=$here/../fixtures/js/matchall-cases.txt
node "$here/promise_oracle.mjs" "$cases" > "$tmp/o"
if [ "${1:-}" = "--update" ]; then cp "$tmp/o" "$here/../fixtures/js/matchall-cases.expected"; fi
target/release/examples/promise_check "$cases" > "$tmp/m"
if diff "$tmp/o" "$tmp/m" > "$tmp/d"; then
  echo "matchall cases: $(wc -l < "$tmp/m") match"
else
  echo "matchall cases: DIFFERENCES (< node, > bare-js)"; cat "$tmp/d"; exit 1
fi
