#!/bin/sh
# Compare the bare-js EventTarget/Event part of the DOM bridge with Node (vm, empty context) on tools/fixtures/js/browser-event-cases.txt.
# Only our own tiny programs run under Node. Never run a fetched bundle with this tool.
#   tools/js/browser_check.sh
set -eu
here=$(dirname "$0")
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
cargo build --release -q -p bare --example browser_check
cases=$here/../fixtures/js/browser-event-cases.txt
node "$here/browser_oracle.mjs" "$cases" > "$tmp/o"
target/release/examples/browser_check "$cases" > "$tmp/m"
if diff "$tmp/o" "$tmp/m" > "$tmp/d"; then
  echo "browser event cases: $(wc -l < "$tmp/m") match"
else
  echo "browser event cases: DIFFERENCES (< node, > bare)"; cat "$tmp/d"; exit 1
fi
