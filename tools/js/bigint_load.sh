#!/bin/sh
# BigInt load matrix: 7 cases x 3 scales (1x, 10x, 100x), each in a fresh process with caps.
#   tools/js/bigint_load.sh [OUTPUT_FILE]
# The caps: 45 s wall time and 1 GiB of address space per run. A case that hits a cap, or whose result is wrong,
# fails the run. The expected result is the same computation done in Rust.
set -u
tmp=
if [ "$#" -gt 0 ]; then
  out=$1
else
  tmp=$(mktemp) || exit 1
  out=$tmp
  trap 'rm -f "$tmp"' EXIT HUP INT TERM
fi
cargo build --release -q -p bare-js --example bigint_load || exit 1
bin=target/release/examples/bigint_load
status=0
: > "$out"
for scale in 1 10 100; do
  for case in small heapmix mul divmod tostring i64sort u64sort; do
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
