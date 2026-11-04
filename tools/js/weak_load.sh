#!/bin/sh
# WeakMap and WeakSet load matrix: 7 cases x 3 scales (1x, 10x, 100x), each in a fresh process with caps.
#   tools/js/weak_load.sh [OUTPUT_FILE]
# The caps: 45 s wall time and 1 GiB of address space per run. A wrong checksum or a wrong entry count exits with status 1.
set -u
out=${1:-}
tmp=
if [ -z "$out" ]; then tmp=$(mktemp) && out=$tmp || exit 1; fi
cargo build --release -q -p bare-js --example weak_load || exit 1
bin=target/release/examples/weak_load
status=0
: > "$out"
for scale in 1 10 100; do
  for case in map_obj set_obj churn dead_cycles chain_fwd chain_rev closures; do
    if ! ( ulimit -v 1048576; timeout 45 "$bin" "$case" "$scale" ) >> "$out" 2>&1; then
      echo "FAILED: case=$case scale=$scale" | tee -a "$out" >&2
      status=1
    fi
  done
done
ok=$(grep -c ' run= ' "$out")
[ "$ok" -eq 21 ] || status=1
[ -z "$tmp" ] || { cat "$tmp"; rm -f "$tmp"; }
echo "weak_load: $ok of 21 rows ok, status=$status"
exit $status
