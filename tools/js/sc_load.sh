#!/bin/sh
# Symbol, iterator, Map and Set load matrix: 12 cases x 3 scales (1x, 10x, 100x), each in a fresh process with caps.
#   tools/js/sc_load.sh [OUTPUT_FILE]
# The caps: 45 s wall time and 1 GiB of address space per run. A case that hits a cap, or whose checksum is wrong,
# fails the run. The checksum is the same computation done in Rust. Without OUTPUT_FILE the rows go to a temporary
# file and are printed at the end, so the output is the same when stdout is a pipe, a file or a closed descriptor.
# The last line counts the rows that printed a result; the exit status is 0 only when all 36 are ok.
set -u
out=${1:-}
tmp=
if [ -z "$out" ]; then tmp=$(mktemp) && out=$tmp || exit 1; fi
cargo build --release -q -p bare-js --example sc_load || exit 1
bin=target/release/examples/sc_load
status=0
: > "$out"
for scale in 1 10 100; do
  for case in map_num map_str map_obj set_dedupe churn forof next_loop custom_iter symbol_for symbol_keys toprim foreach_grow; do
    if ! ( ulimit -v 1048576; timeout 45 "$bin" "$case" "$scale" ) >> "$out" 2>&1; then
      echo "FAILED: case=$case scale=$scale" | tee -a "$out" >&2
      status=1
    fi
  done
done
ok=$(grep -c ' run= ' "$out")
[ "$ok" -eq 36 ] || status=1
[ -z "$tmp" ] || { cat "$tmp"; rm -f "$tmp"; }
echo "sc_load: $ok of 36 rows ok (a wrong checksum exits with status 1), status=$status"
exit $status
