#!/bin/sh
# MutationObserver and requestAnimationFrame load matrix: 2 cases x 3 scales (1x, 10x, 100x), each in a fresh process with caps.
#   tools/js/observer_load.sh [OUTPUT_FILE]
# The caps: 45 s wall time and 1 GiB of address space per run (6 processes). A wrong checksum or a cap hit exits with status 1.
set -u
out=${1:-/dev/stdout}
cargo build --release -q -p bare --example observer_load || exit 1
bin=${CARGO_TARGET_DIR:-target}/release/examples/observer_load
status=0
: > "$out"
for scale in 1 10 100; do
  for case in mo raf; do
    if ! ( ulimit -v 1048576; timeout 45 "$bin" "$case" "$scale" ) >> "$out" 2>&1; then
      echo "FAILED: case=$case scale=$scale" | tee -a "$out" >&2
      status=1
    fi
  done
done
exit $status
