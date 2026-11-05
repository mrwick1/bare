#!/bin/sh
# DOM/JS bridge load matrix: 5 cases x 3 scales (1x, 10x, 100x), each in a fresh process with caps.
#   tools/js/browser_load.sh [OUTPUT_FILE]
# The caps: 45 s wall time and 1 GiB of address space per run. A case that hits a cap, or whose checksum is wrong,
# fails the run. 15 processes in total, and 300 s for all of them.
set -u
tmp=
if [ "$#" -gt 0 ]; then
  out=$1
else
  tmp=$(mktemp) || exit 1
  out=$tmp
  trap 'rm -f "$tmp"' EXIT HUP INT TERM
fi
cargo build --release -q -p bare --example browser_load || exit 1
bin=target/release/examples/browser_load
status=0
deadline=$(( $(date +%s) + 300 ))
: > "$out"
for scale in 1 10 100; do
  for case in mutation event lookup svg svgparse; do
    left=$((deadline - $(date +%s)))
    [ "$left" -gt 45 ] && left=45
    if [ "$left" -le 0 ] || ! ( ulimit -v 1048576; timeout "$left" "$bin" "$case" "$scale" ) >> "$out" 2>&1; then
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
