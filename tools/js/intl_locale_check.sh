#!/bin/sh
# Compare the bare-js Intl.Locale and Intl.RelativeTimeFormat with Node (vm, empty context) on
# tools/fixtures/js/intl-locale-cases.txt. Only our own tiny programs run under Node.
#   tools/js/intl_locale_check.sh
set -eu
here=$(dirname "$0")
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
cargo build --release -q -p bare-js --example eval_check
cases=$here/../fixtures/js/intl-locale-cases.txt
node "$here/eval_oracle.mjs" "$cases" > "$tmp/o"
target/release/examples/eval_check "$cases" > "$tmp/m"
if diff -a "$tmp/o" "$tmp/m" > "$tmp/d"; then
  echo "intl locale cases: $(wc -l < "$tmp/m") match ($(grep -c '	throw' "$tmp/m" || true) throws)"
else
  echo "intl locale cases: DIFFERENCES (< node, > bare-js)"; cat "$tmp/d"; exit 1
fi
