#!/bin/sh
# Instructions in user mode of one command: the minimum of 3 runs. The cost rows of docs/resources.md use it.
#   tools/js/instr.sh COMMAND [ARGS...]
# The glibc thread cache holds 7 freed blocks for each size by default. More frees go to slower paths whose cost
# depends on what the heap held before, so an unrelated change at start moves a row by up to 2% (docs/resources.md).
# A large cache removes that. It also raises the peak RSS, so use this script for instructions only, never for memory.
export GLIBC_TUNABLES=glibc.malloc.tcache_count=65535
min=
for _ in 1 2 3; do
  n=$(perf stat -x, -e instructions:u -- "$@" 2>&1 >/dev/null) || { echo "FAILED: $*" >&2; exit 1; }
  n=$(echo "$n" | awk -F, '/instructions:u/ { print $1 }')
  [ -z "$min" ] || [ "$n" -lt "$min" ] && min=$n
done
echo "$min"
