#!/bin/sh
# Bounded 1x/10x/100x lexer load runs. Each case and scale runs in a fresh process with
# a 120 s time limit and a 1 GiB address-space limit. See crates/js/examples/lex_load.rs.
#   tools/js_lex_load.sh [runs]
set -eu
# LEX_PKG=bare-js2 runs the lexer of the new engine (crates/js2/examples).
pkg=${LEX_PKG:-bare-js}
example=lex_load
[ "$pkg" != bare-js2 ] || example=js2_lex_load
runs=${1:-5}
cargo build --release -q -p "$pkg" --example "$example"
bin=${CARGO_TARGET_DIR:-target}/release/examples/$example
for scale in 1 10 100; do
  for case in small repeat comments strings malformed; do
    ( ulimit -v 1048576; timeout 120 "$bin" $case $scale $runs )
  done
  ( ulimit -v 1048576; timeout 120 "$bin" strings $scale $runs decode )
done
