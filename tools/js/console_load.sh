#!/bin/sh
# Console sink load matrix: 4 cases x 3 scales (1x, 10x, 100x), each in a fresh process with caps.
#   tools/js/console_load.sh [OUTPUT_FILE]
# The caps: 45 s wall time and 1 GiB of address space per run. A case that hits a cap, or whose checksum is wrong,
# fails the run. 12 processes in total.
set -u
out=${1:-}
tmp=
if [ -z "$out" ]; then
  tmp=$(mktemp) || exit 1
  out=$tmp
  trap 'rm -f "$tmp"' EXIT
fi
CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS:-2} cargo build --release -q -p bare --example console_load || exit 1
bin=target/release/examples/console_load
status=0
: > "$out"
for scale in 1 10 100; do
  for case in flood retain format patch; do
    if ! ( ulimit -v 1048576; timeout 45 "$bin" "$case" "$scale" ) >> "$out" 2>&1; then
      echo "FAILED: case=$case scale=$scale" | tee -a "$out" >&2
      status=1
    fi
  done
done
rows=$(grep -c '^case=.* ok ' "$out")
[ "$rows" -eq 12 ] || status=1
[ -z "$tmp" ] || cat "$out"
echo "console_load: $rows of 12 rows ok, status=$status"
exit $status
