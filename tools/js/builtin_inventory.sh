#!/bin/sh
# Print the standard JS built-ins that bare-js lacks, compared with the built-ins of the local Node.
#   tools/js/builtin_inventory.sh
set -eu
here=$(dirname "$0")
root=$here/../..
cargo build --release -q --manifest-path "$root/Cargo.toml" -p bare-js --example eval_check
EVAL_CHECK_STEPS=50000000 "$root/target/release/examples/eval_check" -e "$(node "$here/builtin_inventory.mjs")" | sed 's/^string://; s/\\n/\n/g'
