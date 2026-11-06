#!/bin/sh
# Load matrix for AbortController, structuredClone, btoa/atob, Intl.Segmenter and matchAll: 5 cases x 3 scales (1x, 10x, 100x),
# each in a fresh process with caps.
#   tools/js/web_apis_load.sh [OUTPUT_FILE]
# The caps: 45 s wall time and 1 GiB of address space per run (15 processes). A wrong checksum or a leak exits with status 1.
set -u
out=${1:-/dev/stdout}
cargo build --release -q -p bare --example web_apis_load || exit 1
bin=target/release/examples/web_apis_load
status=0
: > "$out"
for scale in 1 10 100; do
  for case in abort clone base64 segmenter matchall; do
    if ! ( ulimit -v 1048576; timeout 45 "$bin" "$case" "$scale" ) >> "$out" 2>&1; then
      echo "FAILED: case=$case scale=$scale" | tee -a "$out" >&2
      status=1
    fi
  done
done
exit $status
