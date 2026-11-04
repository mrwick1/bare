#!/bin/sh
# Compare the bare-js URL and URLSearchParams with Node (vm, URL and URLSearchParams only) on
# tools/fixtures/js/navigation-url-cases.txt. A case that bare gates (`unsupported:...`) is listed, not counted as a
# difference: it must be a documented gate. Only our own tiny programs run under Node. Never run a fetched bundle.
#   tools/js/navigation_check.sh
set -eu
here=$(dirname "$0")
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
cargo build --release -q -p bare --example navigation_check
cases=$here/../fixtures/js/navigation-url-cases.txt
node "$here/navigation_oracle.mjs" "$cases" > "$tmp/o"
target/release/examples/navigation_check "$cases" > "$tmp/m"
grep -P '\tunsupported:' "$tmp/m" | cut -f2 | sort | uniq -c | sort -rn > "$tmp/g" || true
paste "$tmp/o" "$tmp/m" | awk -F'\t' '$4 !~ /^unsupported:/ && $2 != $4 { print "< " $1 "\t" $2; print "> " $3 "\t" $4 }' > "$tmp/d"
total=$(wc -l < "$tmp/m"); gated=$(grep -c -P '\tunsupported:' "$tmp/m" || true)
echo "navigation url cases: $total, gated by bare: $gated"
cat "$tmp/g"
if [ -s "$tmp/d" ]; then echo "DIFFERENCES (< node, > bare)"; cat "$tmp/d"; exit 1; fi
echo "all ungated cases match"
