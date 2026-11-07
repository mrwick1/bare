#!/bin/sh
# Compare the bare-js evaluator with Node (vm, empty context) on `class X extends <builtin>`:
# tools/fixtures/js/subclass-cases.txt (sync) and subclass-promise-cases.txt (promises).
# Only our own tiny programs run under Node. Never run a fetched bundle with this tool.
#   tools/js/subclass_check.sh
set -eu
here=$(dirname "$0")
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
cargo build --release -q -p bare-js --example eval_check --example promise_check
st=0
for k in subclass:eval:eval_check subclass-promise:promise:promise_check; do
  name=${k%%:*}; rest=${k#*:}; oracle=${rest%%:*}_oracle.mjs; bin=${rest#*:}
  cases=$here/../fixtures/js/$name-cases.txt
  node "$here/$oracle" "$cases" > "$tmp/o"
  "${CARGO_TARGET_DIR:-target}/release/examples/$bin" "$cases" > "$tmp/m"
  if diff "$tmp/o" "$tmp/m" > "$tmp/d"; then
    echo "$name cases: $(wc -l < "$tmp/m") match"
  else
    echo "$name cases: DIFFERENCES (< node, > bare-js)"; cat "$tmp/d"; st=1
  fi
done
exit $st
