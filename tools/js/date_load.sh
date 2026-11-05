#!/bin/sh
# Bounded 1x/10x/100x load matrix for Date: 7 cases x 3 scales = 21 runs, each in a fresh process with 45 s wall time and
# 1 GiB of address space. The expected values come from a second calendar algorithm in the example. A wrong result or a
# missing limit fails the run.
#   tools/js/date_load.sh [OUTPUT_FILE]
set -u
tmp=
if [ "$#" -gt 0 ]; then
  out=$1
else
  tmp=$(mktemp) || exit 1
  out=$tmp
  trap 'rm -f "$tmp"' EXIT HUP INT TERM
fi
cargo build --release -q -p bare-js --example date_load || exit 1
bin=target/release/examples/date_load
status=0
: > "$out"
for scale in 1 10 100; do
  for case in utc iso parse gc clock cap longstr; do
    if ! ( ulimit -v 1048576; timeout 45 "$bin" "$case" "$scale" ) >> "$out" 2>&1; then
      echo "FAILED: case=$case scale=$scale" | tee -a "$out" >&2
      status=1
    fi
  done
done
rows=$(grep -c 'checksum=ok' "$out" || true)
[ "$rows" -eq 21 ] || status=1
[ -z "$tmp" ] || cat "$out"
echo "rows=$rows expected=21 status=$status" >&2
exit $status
