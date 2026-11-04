#!/bin/sh
# Compare bare-js tokens with Acorn on every .js file in DIR (read only, nothing is run).
#   ACORN=file:///path/to/acorn.mjs tools/js_lex_check.sh DIR
set -eu
dir=${1:?usage: js_lex_check.sh DIR}
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
cargo build --release -q -p bare-js --example lex_check
for f in "$dir"/*.js; do
  node "$(dirname "$0")/js_lex_oracle.mjs" "$f" > "$tmp/t"
  target/release/examples/lex_check "$f" "$tmp/t"
done
