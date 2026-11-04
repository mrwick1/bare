#!/bin/sh
# Parser load matrix: 5 cases x 3 scales, each in a fresh process with a time and a memory cap.
#   tools/js/parse_load.sh [OUTPUT_FILE]
# The caps: 120 s wall time, 1 GiB of address space. A case that hits a cap fails the run.
set -u
out=${1:-/dev/stdout}
cargo build --release -q -p bare-js --example parse_load || exit 1
bin=target/release/examples/parse_load
status=0
: > "$out"
for scale in 1 10 100; do
  for case in stmts chain bodies malformed deep; do
    if ! ( ulimit -v 1048576; timeout 120 "$bin" "$case" "$scale" ) >> "$out" 2>&1; then
      echo "FAILED: case=$case scale=$scale" | tee -a "$out" >&2
      status=1
    fi
  done
done
exit $status
