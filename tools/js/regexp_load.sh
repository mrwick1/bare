#!/bin/sh
# Bounded 1x/10x/100x regexp loads. Each mode and scale runs in a fresh process with a 120 s limit
# and a 1 GiB address-space limit. Checksums come from Node (regexp_load_expect.mjs).
#   tools/js/regexp_load.sh [OUT_DIR]
set -eu
here=$(cd "$(dirname "$0")" && pwd)
out=${1:-$(mktemp -d)}
rustc --edition 2024 -O --crate-type rlib --crate-name bare_unicode "$here/../../crates/unicode/src/lib.rs" -o "$out/libbare_unicode.rlib"
rustc --edition 2024 -O --extern bare_unicode="$out/libbare_unicode.rlib" "$here/regexp_load.rs" -o "$out/regexp_load"
for scale in 1 10 100; do
  for mode in short caps near; do
    ( ulimit -v 1048576; timeout 120 "$out/regexp_load" $mode $scale "$(node "$here/regexp_load_expect.mjs" $mode)" )
  done
  for mode in budget compile; do
    ( ulimit -v 1048576; timeout 120 "$out/regexp_load" $mode $scale )
  done
done
