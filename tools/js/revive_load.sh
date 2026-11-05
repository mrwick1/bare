#!/bin/sh
# Bounded 1x/10x/100x load matrix for JSON.parse with a reviver: 5 cases x 3 scales = 15 runs, each in a fresh process
# with 45 s wall time and 1 GiB of address space. A wrong result or a missing limit fails the run.
#   tools/js/revive_load.sh [OUTPUT_FILE]
set -u
out=${1:-/dev/stdout}
cargo build --release -q -p bare-js --example revive_load || exit 1
bin=target/release/examples/revive_load
status=0
: > "$out"
for scale in 1 10 100; do
  for case in shallow deep holes gc cycle; do
    if ! ( ulimit -v 1048576; timeout 45 "$bin" "$case" "$scale" ) >> "$out" 2>&1; then
      echo "FAILED: case=$case scale=$scale" | tee -a "$out" >&2
      status=1
    fi
  done
done
exit $status
