#!/bin/sh
# XMLHttpRequest load rows over loopback: 1, 100 and 1000 sequential GETs, each in a fresh process with caps.
#   tools/js/xhr_load.sh [OUTPUT_FILE]
# The caps: 45 s wall time and 1 GiB of address space per run. A wrong reply, or a live object count that does not return
# to the baseline, fails the run.
set -u
out=${1:-/dev/stdout}
cargo build --release -q -p bare --example xhr_load || exit 1
bin=${CARGO_TARGET_DIR:-target}/release/examples/xhr_load
status=0
: > "$out"
for n in 1 100 1000; do
  if ! ( ulimit -v 1048576; timeout 45 "$bin" "$n" ) >> "$out" 2>&1; then
    echo "FAILED: n=$n" | tee -a "$out" >&2
    status=1
  fi
done
exit $status
