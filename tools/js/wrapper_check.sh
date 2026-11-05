#!/bin/sh
# Compare the bare-js primitive wrapper objects (new String/Number/Boolean, Object(prim), the String exotic object, the `this`
# checks, JSON, Array receivers, sloppy `this`) with Node (vm, empty context) on tools/fixtures/js/wrapper-cases.txt. Only our own
# tiny programs run under Node.
#   tools/js/wrapper_check.sh
set -eu
here=$(dirname "$0")
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
export TZ=UTC
cargo build --release -q -p bare-js --example eval_check
cases=$here/../fixtures/js/wrapper-cases.txt
node "$here/eval_oracle.mjs" "$cases" > "$tmp/o"
EVAL_CHECK_STEPS=200000000 target/release/examples/eval_check "$cases" > "$tmp/m"
if diff -a "$tmp/o" "$tmp/m" > "$tmp/d"; then
  echo "wrapper cases: $(wc -l < "$tmp/m") match ($(grep -c '	throw' "$tmp/m" || true) throws)"
else
  echo "wrapper cases: DIFFERENCES (< node, > bare-js)"; cat "$tmp/d"; exit 1
fi
