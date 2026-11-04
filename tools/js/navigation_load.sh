#!/bin/sh
# Navigation load matrix: 5 cases x 3 scales (1x, 10x, 100x), each in a fresh process with caps.
#   tools/js/navigation_load.sh [OUTPUT_FILE]
# The caps: 45 s wall time and 1 GiB of address space per run. A case that hits a cap, or whose checksum is wrong,
# fails the run. 15 processes in total.
set -u
out=${1:-/dev/stdout}
cargo build --release -q -p bare --example navigation_load || exit 1
bin=target/release/examples/navigation_load
status=0
: > "$out"
for scale in 1 10 100; do
  for case in url iterate history storage route; do
    if ! ( ulimit -v 1048576; timeout 45 "$bin" "$case" "$scale" ) >> "$out" 2>&1; then
      echo "FAILED: case=$case scale=$scale" | tee -a "$out" >&2
      status=1
    fi
  done
done
exit $status
