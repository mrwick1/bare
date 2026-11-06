#!/bin/sh
# Run the sticky load rows: 1, 100 and 1000 sticky boxes, the first paint and 100 scroll steps (time, peak RSS).
# Usage: CARGO_TARGET_DIR=/data/bare-tmp/sticky-target tools/js/sticky_load.sh
set -e
cargo build --release -p bare --example sticky_load
for n in 1 100 1000; do
  "${CARGO_TARGET_DIR:-target}/release/examples/sticky_load" "$n" 100
done
