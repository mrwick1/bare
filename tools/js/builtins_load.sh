#!/bin/sh
# Bounded 1x/10x/100x load matrix for the JSON.parse, RegExp and String bindings: 8 cases x 3 scales, each in a fresh
# process with 45 s wall time and 1 GiB of address space. A cap hit or a wrong result fails the run.
#   tools/js/builtins_load.sh [OUTPUT_FILE]
set -u
out=${1:-/dev/stdout}
cargo build --release -q -p bare-js --example builtins_load || exit 1
bin=target/release/examples/builtins_load
status=0
: > "$out"
for scale in 1 10 100; do
  for case in json scan replace split strings routes zero gc; do
    if ! ( ulimit -v 1048576; timeout 45 "$bin" "$case" "$scale" ) >> "$out" 2>&1; then
      echo "FAILED: case=$case scale=$scale" | tee -a "$out" >&2
      status=1
    fi
  done
done
exit $status
