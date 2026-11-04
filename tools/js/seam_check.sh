#!/bin/sh
# Compare the bare-js Array/String/RegExp/Promise and Symbol seam cases with Node (vm, empty context) on tools/fixtures/js/seam-cases.txt.
# It reuses the promise tools: their cases end with a reader function, and these cases are synchronous.
# Only our own tiny programs run under Node. Never run a fetched bundle with this tool.
#   tools/js/seam_check.sh
set -eu
here=$(dirname "$0")
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
cargo build --release -q -p bare-js --example promise_check
cases=$here/../fixtures/js/seam-cases.txt
node "$here/promise_oracle.mjs" "$cases" > "$tmp/o"
if [ "${1:-}" = "--update" ]; then cp "$tmp/o" "$here/../fixtures/js/seam-cases.expected"; fi
target/release/examples/promise_check "$cases" > "$tmp/m"
if diff "$tmp/o" "$tmp/m" > "$tmp/d"; then
  echo "seam cases: $(wc -l < "$tmp/m") match"
else
  echo "seam cases: DIFFERENCES (< node, > bare-js)"; cat "$tmp/d"; exit 1
fi
