#!/bin/sh
# innerHTML load matrix: 3 cases x 3 scales (1x, 10x, 100x), each in a fresh process with caps.
#   tools/js/fragment_load.sh [release|debug] [OUTPUT_FILE]
# The caps: 45 s wall time and 1 GiB of address space per run. A case that hits a cap, or whose checksum is wrong,
# fails the run. 9 processes in total. Release runs on an 8 MiB VM stack, debug on 64 MiB.
set -u
mode=${1:-release}
out=${2:-$(mktemp)}
if [ "$#" -lt 2 ]; then
  trap 'rm -f "$out"' EXIT HUP INT TERM
fi
if [ "$mode" = release ]; then
  cargo build --release -q -p bare --example fragment_load || exit 1
  bin=target/release/examples/fragment_load
else
  cargo build -q -p bare --example fragment_load || exit 1
  bin=target/debug/examples/fragment_load
fi
status=0
: > "$out"
for scale in 1 10 100; do
  for case in parse replace deep; do
    if ! ( ulimit -v 1048576; timeout 45 "$bin" "$case" "$scale" ) >> "$out" 2>&1; then
      echo "FAILED: case=$case scale=$scale" | tee -a "$out" >&2
      status=1
    fi
  done
done
rows=$(grep -c 'checksum=ok' "$out" || true)
if [ "$#" -lt 2 ]; then cat "$out"; fi
echo "rows=$rows expected=9"
[ "$rows" -eq 9 ] || status=1
exit $status
