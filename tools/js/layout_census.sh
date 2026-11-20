#!/bin/sh
# Check the layout census against valgrind massif (plan step M3). Runs COMMAND under massif with BARE_JS_STATS=1, then prints the
# `layout` line of the engine and the engine bytes of the massif snapshot where they peak.
#   tools/js/layout_census.sh [--host-parse] COMMAND [ARGS...]
# Build the binary with symbols first: CARGO_PROFILE_RELEASE_STRIP=false CARGO_PROFILE_RELEASE_DEBUG=1 cargo build --release ...
# --host-parse: the program keeps its own copy of the syntax tree (the examples do), so massif_engine.py leaves that copy out.
# The census counts the bytes that the engine asks of the allocator (no malloc headers), as massif's useful heap does.
# Set BARE_JS_EXEC (tree, auto, new) as for the example. It takes minutes for a big run: massif is 20 to 50 times slower.
set -eu
here=$(dirname "$0")
flag=
if [ "${1:-}" = --host-parse ]; then flag=--host-parse; shift; fi
tmp=$(mktemp -d)
BARE_JS_STATS=1 valgrind --tool=massif --pages-as-heap=no --depth=60 --threshold=0.1 --detailed-freq=1 --max-snapshots=400 \
  --massif-out-file="$tmp/m.out" "$@" > "$tmp/out.txt" 2> "$tmp/err.txt" || { cat "$tmp/err.txt" >&2; exit 1; }
grep -h '^layout ' "$tmp/err.txt" "$tmp/out.txt" | cut -c1-600 || true
python3 -I "$here/massif_engine.py" "$tmp/m.out" $flag
rm -rf "$tmp"
