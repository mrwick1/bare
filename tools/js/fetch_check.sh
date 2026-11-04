#!/bin/sh
# Compare the pure parts of the bare-js fetch layer (Headers, Request, Response, body reader) with Node (vm) on
# tools/fixtures/js/fetch-cases.txt. Only our own tiny programs run under Node. Never run a fetched bundle with this tool.
#   tools/js/fetch_check.sh
set -eu
here=$(dirname "$0")
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
cargo build --release -q -p bare --example fetch_check
cases=$here/../fixtures/js/fetch-cases.txt
node "$here/fetch_oracle.mjs" "$cases" > "$tmp/o"
target/release/examples/fetch_check "$cases" > "$tmp/m"
if diff "$tmp/o" "$tmp/m" > "$tmp/d"; then
  echo "fetch cases: $(wc -l < "$tmp/m") match"
else
  echo "fetch cases: DIFFERENCES (< node, > bare)"; cat "$tmp/d"; exit 1
fi
