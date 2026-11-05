#!/bin/sh
# Compare bare-js Date with Node (vm, empty context) on tools/fixtures/js/date-cases.txt. The cases use UTC only, so the result
# does not depend on TZ (checked with UTC, America/New_York and Asia/Kolkata). Cases where V8 differs from the standard are not
# in this file: they are Rust tests in crates/js/src/eval_date_tests.rs. Only our own tiny programs run under Node. Never run a
# fetched bundle with this tool.
#   tools/js/date_check.sh [--update]
set -eu
here=$(dirname "$0")
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
cargo build --release -q -p bare-js --example eval_check
cases=$here/../fixtures/js/date-cases.txt
TZ=America/New_York node "$here/eval_oracle.mjs" "$cases" > "$tmp/o"
if [ "${1:-}" = "--update" ]; then cp "$tmp/o" "$here/../fixtures/js/date-cases.expected"; fi
target/release/examples/eval_check "$cases" > "$tmp/m"
if diff "$tmp/o" "$tmp/m" > "$tmp/d"; then
  echo "date cases: $(wc -l < "$tmp/m") match"
else
  echo "date cases: DIFFERENCES (< node, > bare-js)"; cat "$tmp/d"; exit 1
fi
