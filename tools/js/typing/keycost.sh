#!/bin/sh
# The cost of one key on the React page of tools/js/typing (gate G4): M instructions for each key and the peak RSS.
#   tools/js/typing/keycost.sh OUTDIR
# OUTDIR holds the page. It is built with build.sh when index.html is missing. The binary is `type_load` of the tree that
# CARGO_TARGET_DIR names (default: target), built here with cargo.
# Instructions for each key: (instructions:u of 50 keys minus instructions:u of 0 keys) / 50, minimum of 3 runs each
# (tools/js/instr.sh, glibc with the tcache tunable), for the textarea `#a1` and the first input `#f0`. The run with 0 keys
# is the load and the mount of the page; it is printed too.
# Peak RSS: the maximum resident set of a run of `#a1` with 20 keys and with 50 keys. It uses no tunable and no MEM=1.
# The environment passes through to the program, so BARE_JS_EXEC=tree|auto|byte=... works once the engine reads it.
# Needs perf and python3. Budget: RSS 40 MiB at 50 keys.
# BARE_TMP (default /data/bare-tmp, else ~/bare-tmp) holds `bin/heavy`; the cargo build and each measured run wait in it for
# a free machine slot.
set -eu
[ -d "${BARE_TMP:-}" ] || { [ -d /data/bare-tmp ] && BARE_TMP=/data/bare-tmp || BARE_TMP=$HOME/bare-tmp; }
export BARE_TMP TMPDIR=${TMPDIR:-$BARE_TMP/tmp}
mkdir -p "$TMPDIR"
heavy=$BARE_TMP/bin/heavy; [ -x "$heavy" ] || heavy=
[ $# -eq 1 ] || { sed -n '2,13p' "$0"; exit 2; }
here=$(dirname "$0")
out=$(realpath -m "$1")
[ -f "$out/index.html" ] || "$here/build.sh" "$out"
$heavy cargo build --release -q -p bare --example type_load
bin=${CARGO_TARGET_DIR:-target}/release/examples/type_load
echo "page $out, ${BARE_JS_EXEC:+BARE_JS_EXEC=$BARE_JS_EXEC, }$(git -C "$here" rev-parse --short HEAD 2>/dev/null || echo no-git)"
for sel in '#a1' '#f0'; do
  k0=$($heavy "$here/../instr.sh" "$bin" "$out" "$sel" 0)
  k50=$($heavy "$here/../instr.sh" "$bin" "$out" "$sel" 50)
  awk -v s="$sel" -v a="$k0" -v b="$k50" 'BEGIN { printf "%s: %.1f M for each key (50 keys %.1f M minus 0 keys %.1f M, divided by 50); load and mount %.1f M\n", s, (b - a) / 50e6, b / 1e6, a / 1e6, a / 1e6 }'
done
for n in 20 50; do
  $heavy python3 -I -c 'import resource, subprocess, sys
subprocess.run(sys.argv[1:], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=True)
print("#a1: peak RSS %.1f MiB at %s keys" % (resource.getrusage(resource.RUSAGE_CHILDREN).ru_maxrss / 1024, sys.argv[4]))' "$bin" "$out" '#a1' "$n"
done
