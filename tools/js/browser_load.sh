#!/bin/sh
# DOM/JS bridge load matrix: 5 cases x 3 scales (1x, 10x, 100x), each in a fresh process with caps.
#   tools/js/browser_load.sh [OUTPUT_FILE]
# The caps: 45 s wall time and 1 GiB of address space per run. A case that hits a cap, or whose checksum is wrong,
# fails the run. 15 processes in total, and 300 s for all of them.
set -u
out=${1:-/dev/stdout}
cargo build --release -q -p bare --example browser_load || exit 1
bin=target/release/examples/browser_load
status=0
: > "$out"
for scale in 1 10 100; do
  for case in mutation event lookup svg svgparse; do
    left=$((300 - SECONDS))
    [ "$left" -gt 45 ] && left=45
    if [ "$left" -le 0 ] || ! ( ulimit -v 1048576; timeout "$left" "$bin" "$case" "$scale" ) >> "$out" 2>&1; then
      echo "FAILED: case=$case scale=$scale" | tee -a "$out" >&2
      status=1
    fi
  done
done
exit $status
