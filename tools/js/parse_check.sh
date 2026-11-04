#!/bin/sh
# Compare the bare-js parser with Acorn (ecma2022, script). Nothing is run; sources are only parsed.
#   ACORN=file:///path/to/acorn.mjs tools/js/parse_check.sh [BUNDLE_DIR]
# 1. every case in tools/fixtures/js/parse-cases.txt: accept/reject, and the whole tree with spans for accepted cases;
# 2. with BUNDLE_DIR: every .js file there: the whole tree with spans.
# A rejected case matches when both sides reject (the messages differ, so only the word `err` is compared).
set -eu
here=$(dirname "$0")
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
cargo build --release -q -p bare-js --example parse_check
bin=target/release/examples/parse_check
status=0
cases=$here/../fixtures/js/parse-cases.txt
node "$here/parse_oracle.mjs" cases "$cases" | sed 's/^\([0-9]*\) err.*/\1 err/' > "$tmp/o"
$bin cases "$cases" | sed 's/^\([0-9]*\) err.*/\1 err/' > "$tmp/m"
if diff "$tmp/o" "$tmp/m" > "$tmp/d"; then
  echo "cases: $(wc -l < "$tmp/m") match ($(grep -c ' err$' "$tmp/m") rejected by both, $(grep -c ' unsupported$' "$tmp/m") unsupported)"
else
  echo "cases: DIFFERENCES (first 20 lines; line number = case number, from 0)"; head -20 "$tmp/d"; status=1
fi
if [ $# -ge 1 ]; then
  for f in "$1"/*.js; do
    node "$here/parse_oracle.mjs" dump "$f" > "$tmp/o"
    $bin dump "$f" > "$tmp/m"
    if cmp -s "$tmp/o" "$tmp/m"; then echo "bundle ok: $(basename "$f") ($(wc -c < "$tmp/m") bytes of tree)"; else echo "bundle DIFFERS: $f"; status=1; fi
  done
fi
exit $status
