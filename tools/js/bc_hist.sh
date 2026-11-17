#!/bin/sh
# Report of the bytecode lane: executed operations, pairs, triples and functions (feature `bc-hist` of bare-js).
#   tools/js/bc_hist.sh [-e EXAMPLE] ARGS...     default example: type_load. BARE_JS_EXEC is kept (default byte).
# The instrumented build goes to $HIST_TARGET (default /tmp/bare-hist-target), so the normal build is not disturbed.
ex=type_load
[ "$1" = -e ] && { ex=$2; shift 2; }
export CARGO_TARGET_DIR=${HIST_TARGET:-/tmp/bare-hist-target}
cargo build --release -q -p bare --example "$ex" --features bare-js/bc-hist || exit 1
BARE_JS_EXEC=${BARE_JS_EXEC:-byte} "$CARGO_TARGET_DIR/release/examples/$ex" "$@" 2>&1 >/dev/null | grep '^bchist'
