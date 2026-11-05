#!/bin/sh
# Intl load matrix (DateTimeFormat, NumberFormat, toLocaleString, localeCompare): 10 cases x 3 scales (1x, 10x, 100x), each in a fresh process with caps.
#   tools/js/intl_load.sh [OUTPUT_FILE]
# The caps: 45 s wall time and 1 GiB of address space per run. A case that hits a cap, or whose checksum is wrong,
# fails the run. The checksum is the same computation done in Rust.
set -u
out=${1:-/dev/stdout}
cargo build --release -q -p bare-js --example intl_load || exit 1
bin=target/release/examples/intl_load
status=0
: > "$out"
for scale in 1 10 100; do
  for case in make format parts locale plain nf_format nf_locale nf_locale_opts cmp cmp_opts; do
    if ! ( ulimit -v 1048576; timeout 45 "$bin" "$case" "$scale" ) >> "$out" 2>&1; then
      echo "FAILED: case=$case scale=$scale" | tee -a "$out" >&2
      status=1
    fi
  done
done
exit $status
