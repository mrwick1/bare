#!/bin/sh
# Form control and click activation load matrix: 3 cases x 3 scales (1x, 10x, 100x), each in a fresh process with caps.
#   tools/js/form_load.sh [release|debug] [OUTPUT_FILE]
# The caps: 45 s wall time and 1 GiB of address space per run. The VM thread has 8 MiB of stack in release and 64 MiB in
# debug. A case that hits a cap, or whose model check fails, fails the run. 9 processes in total for each mode.
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
  release) cargo build --release -q -p bare --example form_load || exit 1
           bin=target/release/examples/form_load; stack=8 ;;
  debug) cargo build -q -p bare --example form_load || exit 1
         bin=target/debug/examples/form_load; stack=64 ;;
  *) echo "mode: release or debug" >&2; exit 2 ;;
esac
status=0
: > "$out"
for scale in 1 10 100; do
  for case in submit gc gates; do
    if ! ( ulimit -v 1048576; FORM_LOAD_STACK_MIB=$stack timeout 45 "$bin" "$case" "$scale" ) >> "$out" 2>&1; then
      echo "FAILED: case=$case scale=$scale mode=$mode" | tee -a "$out" >&2
      status=1
    fi
  done
done
rows=$(grep -c 'checksum=ok' "$out" || true)
[ "$rows" -eq 9 ] || status=1
[ -z "$tmp" ] || cat "$out"
echo "mode=$mode rows=$rows expected=9 status=$status" >&2
exit $status
