#!/bin/sh
# Array/String/Symbol seam load matrix: 7 cases x 3 scales (1x, 10x, 100x), each in a fresh process with caps.
#   tools/js/seam_load.sh [OUTPUT_FILE]
# The caps: 45 s wall time and 1 GiB of address space per run. A case that hits a cap, or whose checksum is wrong,
# fails the run. The checksum is the same computation done in Rust. Without OUTPUT_FILE the rows go to a temporary
# file and are printed at the end, so the output is the same when stdout is a pipe, a file or a closed descriptor.
# The last line counts the rows with `checksum=ok`; the exit status is 0 only when all 21 are ok.
set -u
out=${1:-}
tmp=
if [ -z "$out" ]; then tmp=$(mktemp) && out=$tmp || exit 1; fi
cargo build --release -q -p bare-js --example seam_load || exit 1
bin=target/release/examples/seam_load
status=0
: > "$out"
for scale in 1 10 100; do
  for case in from_set from_map_entries concat_flag iter_methods forof_break string_hooks species_map; do
    if ! ( ulimit -v 1048576; timeout 45 "$bin" "$case" "$scale" ) >> "$out" 2>&1; then
      echo "FAILED: case=$case scale=$scale" | tee -a "$out" >&2
      status=1
    fi
  done
done
ok=$(grep -c 'checksum=ok' "$out")
[ "$ok" -eq 21 ] || status=1
[ -z "$tmp" ] || { cat "$tmp"; rm -f "$tmp"; }
echo "seam_load: $ok of 21 rows checksum=ok, status=$status"
exit $status
