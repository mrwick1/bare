#!/bin/sh
# Compare the bare-js Number, Math, URI and global functions with Node (vm, empty context) on tools/fixtures/js/number-cases.txt.
# Only our own tiny programs run under Node. Never run a fetched bundle with this tool.
#   tools/js/number_check.sh
set -eu
here=$(dirname "$0")
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
cargo build --release -q -p bare-js --example eval_check
cases=$here/../fixtures/js/number-cases.txt
node "$here/eval_oracle.mjs" "$cases" > "$tmp/o"
EVAL_CHECK_STEPS=2000000000 target/release/examples/eval_check "$cases" > "$tmp/m"
if diff -a "$tmp/o" "$tmp/m" > "$tmp/d"; then
  echo "number cases: $(wc -l < "$tmp/m") match ($(grep -c '	throw' "$tmp/m" || true) throws)"
else
  echo "number cases: DIFFERENCES (< node, > bare-js)"; cat "$tmp/d"; exit 1
fi
