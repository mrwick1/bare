#!/bin/sh
# Select and option load matrix: 3 cases x 3 sizes (10, 1,000 and 10,000 options), each in a fresh process with caps.
#   tools/js/select_load.sh [release|debug] [OUTPUT_FILE]
# The caps: 60 s wall time and 1 GiB of address space per run. A case that hits a cap, or whose check fails, fails the run.
set -u
mode=${1:-release}
tmp=
if [ "$#" -gt 1 ]; then
  out=$2
else
  tmp=$(mktemp) || exit 1
  out=$tmp
  trap 'rm -f "$tmp"' EXIT HUP INT TERM
fi
case $mode in
  release) cargo build --release -q -p bare --example select_load || exit 1
           bin=${CARGO_TARGET_DIR:-target}/release/examples/select_load; stack=8 ;;
  debug) cargo build -q -p bare --example select_load || exit 1
         bin=${CARGO_TARGET_DIR:-target}/debug/examples/select_load; stack=64 ;;
  *) echo "mode: release or debug" >&2; exit 2 ;;
esac
status=0
: > "$out"
for n in 10 1000 10000; do
  for case in value react churn; do
    if ! ( ulimit -v 1048576; SELECT_LOAD_STACK_MIB=$stack timeout 60 "$bin" "$case" "$n" ) >> "$out" 2>&1; then
      echo "FAILED: case=$case n=$n mode=$mode" | tee -a "$out" >&2
      status=1
    fi
  done
done
rows=$(grep -c 'checksum=ok' "$out" || true)
[ "$rows" -eq 9 ] || status=1
[ -z "$tmp" ] || cat "$out"
echo "mode=$mode rows=$rows expected=9 status=$status" >&2
exit $status
