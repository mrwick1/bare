#!/bin/sh
# Compare the default ReadableStream of the bare browser layer with Node (vm) on tools/fixtures/js/streams-cases.txt.
# Only our own tiny programs run under Node. Never run a fetched bundle with this tool. The Rust side reuses the
# `fetch_check` example, which runs any cases file in a Browser.
#   tools/js/streams_check.sh
set -eu
here=$(dirname "$0")
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
cargo build --release -q -p bare --example fetch_check
cases=$here/../fixtures/js/streams-cases.txt
node "$here/streams_oracle.mjs" "$cases" > "$tmp/o"
target/release/examples/fetch_check "$cases" > "$tmp/m"
if diff "$tmp/o" "$tmp/m" > "$tmp/d"; then
  echo "streams cases: $(wc -l < "$tmp/m") match"
else
  echo "streams cases: DIFFERENCES (< node, > bare)"; cat "$tmp/d"; exit 1
fi
