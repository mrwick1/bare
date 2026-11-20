#!/bin/sh
# Bounded 1x/10x/100x lexer load runs. Each case and scale runs in a fresh process with
# a 120 s time limit and a 1 GiB address-space limit. See crates/js/examples/lex_load.rs.
#   tools/js_lex_load.sh [runs]
set -eu
# LEX_PKG=bare-js2 runs the lexer of the new engine (crates/js2/examples).
pkg=${LEX_PKG:-bare-js}
runs=${1:-5}
cargo build --release -q -p "$pkg" --example lex_load
for scale in 1 10 100; do
  for case in small repeat comments strings malformed; do
    ( ulimit -v 1048576; timeout 120 "${CARGO_TARGET_DIR:-target}"/release/examples/lex_load $case $scale $runs )
  done
  ( ulimit -v 1048576; timeout 120 "${CARGO_TARGET_DIR:-target}"/release/examples/lex_load strings $scale $runs decode )
done
