#!/bin/sh
# Compare the bare scheduler (timers, queueMicrotask, MessageChannel) with Node (vm; only its own timers, queueMicrotask, MessageChannel; 200 ms of real time per case) on tools/fixtures/js/scheduler-cases.txt.
# Only our own tiny programs run under Node. Never run a fetched bundle with this tool.
#   tools/js/scheduler_check.sh
set -eu
here=$(dirname "$0")
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
cargo build --release -q -p bare --example scheduler_check
cases=$here/../fixtures/js/scheduler-cases.txt
node "$here/scheduler_oracle.mjs" "$cases" > "$tmp/o"
target/release/examples/scheduler_check "$cases" > "$tmp/m"
if diff "$tmp/o" "$tmp/m" > "$tmp/d"; then
  echo "scheduler cases: $(wc -l < "$tmp/m") match"
else
  echo "scheduler cases: DIFFERENCES (< node, > bare)"; cat "$tmp/d"; exit 1
fi
