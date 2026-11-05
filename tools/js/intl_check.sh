#!/bin/sh
# Compare the bare-js Intl.DateTimeFormat with Node (vm, empty context) on tools/fixtures/js/intl-cases.txt.
# Both run with TZ=UTC. The matrix cases need a large step budget. Only our own tiny programs run under Node.
#   tools/js/intl_check.sh
set -eu
here=$(dirname "$0")
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
export TZ=UTC
cargo build --release -q -p bare-js --example eval_check
cases=$here/../fixtures/js/intl-cases.txt
node "$here/eval_oracle.mjs" "$cases" > "$tmp/o"
EVAL_CHECK_STEPS=2000000000 target/release/examples/eval_check "$cases" > "$tmp/m"
if diff -a "$tmp/o" "$tmp/m" > "$tmp/d"; then
  echo "intl cases: $(wc -l < "$tmp/m") match ($(grep -c '	throw' "$tmp/m" || true) throws)"
else
  echo "intl cases: DIFFERENCES (< node, > bare-js)"; cat "$tmp/d"; exit 1
fi
