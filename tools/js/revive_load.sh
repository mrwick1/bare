#!/bin/sh
# Bounded 1x/10x/100x load matrix for JSON.parse with a reviver: 5 cases x 3 scales = 15 runs, each in a fresh process
# with 45 s wall time and 1 GiB of address space. A wrong result or a missing limit fails the run.
#   tools/js/revive_load.sh [OUTPUT_FILE]
set -u
tmp=
if [ "$#" -gt 0 ]; then
  out=$1
else
  tmp=$(mktemp) || exit 1
  out=$tmp
  trap 'rm -f "$tmp"' EXIT HUP INT TERM
fi
cargo build --release -q -p bare-js --example revive_load || exit 1
bin=target/release/examples/revive_load
status=0
: > "$out"
for scale in 1 10 100; do
  for case in shallow deep holes gc cycle; do
    if ! ( ulimit -v 1048576; timeout 45 "$bin" "$case" "$scale" ) >> "$out" 2>&1; then
      echo "FAILED: case=$case scale=$scale" | tee -a "$out" >&2
      status=1
    fi
  done
done
rows=$(grep -c 'checksum=ok' "$out" || true)
[ "$rows" -eq 15 ] || status=1
[ -z "$tmp" ] || cat "$out"
echo "rows=$rows expected=15 status=$status" >&2
exit $status
