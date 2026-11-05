#!/bin/sh
# Compare the fdlibm port (crates/js/src/ieee754.rs) with Node bit for bit on about 75,000 inputs.
# tools/js/math_gen.mjs makes the inputs and the Node results. Math.hypot is checked by tools/js/number_check.sh.
#   tools/js/math_check.sh
set -eu
here=$(dirname "$0")
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
cargo build --release -q -p bare-js --example math_check
node "$here/math_gen.mjs" > "$tmp/rows"
target/release/examples/math_check < "$tmp/rows"
