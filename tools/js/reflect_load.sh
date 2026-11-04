#!/bin/sh
# Object.getOwnPropertyNames / Reflect.ownKeys / Reflect.construct load matrix: 7 cases x 3 scales (1x, 10x, 100x), each in a
# fresh process with caps.
#   tools/js/reflect_load.sh [OUTPUT_FILE]
# The caps: 45 s wall time and 1 GiB of address space per run (21 processes). A wrong checksum exits with status 1.
set -u
out=${1:-}
tmp=
if [ -z "$out" ]; then tmp=$(mktemp) && out=$tmp || exit 1; fi
cargo build --release -q -p bare-js --example reflect_load || exit 1
bin=target/release/examples/reflect_load
status=0
: > "$out"
for scale in 1 10 100; do
  for case in construct_fn construct_class construct_args keys views nested cross_script; do
    if ! ( ulimit -v 1048576; timeout 45 "$bin" "$case" "$scale" ) >> "$out" 2>&1; then
      echo "FAILED: case=$case scale=$scale" | tee -a "$out" >&2
      status=1
    fi
  done
done
ok=$(grep -c ' run= ' "$out")
[ "$ok" -eq 21 ] || status=1
[ -z "$tmp" ] || { cat "$tmp"; rm -f "$tmp"; }
echo "reflect_load: $ok of 21 rows ok, status=$status"
exit $status
