#!/bin/sh
# Byte buffer and text codec load matrix: 5 cases x 3 scales (1x, 10x, 100x), each in a fresh process with caps.
#   tools/js/binary_load.sh [OUTPUT_FILE]
# The caps: 45 s wall time and 1 GiB of address space per run. A case that hits a cap, or whose result is wrong,
# fails the run. The expected result is the same computation done in Rust.
set -u
out=${1:-/dev/stdout}
cargo build --release -q -p bare-js --example binary_load || exit 1
bin=target/release/examples/binary_load
status=0
: > "$out"
for scale in 1 10 100; do
  for case in copy scan scanarr codec stream; do
    if ! ( ulimit -v 1048576; timeout 45 "$bin" "$case" "$scale" ) >> "$out" 2>&1; then
      echo "FAILED: case=$case scale=$scale" | tee -a "$out" >&2
      status=1
    fi
  done
done
exit $status
