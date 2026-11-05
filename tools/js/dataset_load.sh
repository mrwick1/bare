#!/bin/sh
# Dataset load matrix: 3 cases x 3 sizes (1k, 10k, 100k operations) x 3 attribute counts (1, 10, 50), each in a fresh process
# with caps.
#   tools/js/dataset_load.sh [OUTPUT_FILE]
# The caps: 45 s wall time and 1 GiB of address space per run. A case that hits a cap, or whose checksum is wrong,
# fails the run. 27 processes in total.
set -u
out=${1:-$(mktemp)}
if [ "$#" -eq 0 ]; then
  trap 'rm -f "$out"' EXIT HUP INT TERM
fi
cargo build --release -q -p bare --example dataset_load || exit 1
bin=target/release/examples/dataset_load
status=0
: > "$out"
for ops in 1000 10000 100000; do
  for attrs in 1 10 50; do
    for case in read write keys; do
      if ! ( ulimit -v 1048576; timeout 45 "$bin" "$case" "$ops" "$attrs" ) >> "$out" 2>&1; then
        echo "FAILED: case=$case ops=$ops attrs=$attrs" | tee -a "$out" >&2
        status=1
      fi
    done
  done
done
rows=$(grep -c 'checksum=ok' "$out" || true)
if [ "$#" -eq 0 ]; then cat "$out"; fi
echo "rows=$rows expected=27"
[ "$rows" -eq 27 ] || status=1
exit $status
