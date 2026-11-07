#!/bin/sh
# The cost rule (gate G3): run every row of cost_rows.txt on two built trees and list the rows that grew.
#   tools/js/cost_matrix.sh [-j JOBS] [-g REGEX] [-t PERCENT] [-a] [-J] [-o RAWFILE] BASE_DIR NEW_DIR
# BASE_DIR and NEW_DIR are CARGO_TARGET_DIR directories that hold `release/examples/*` and `release/bare-stress`.
# Build each tree with: cargo build --release --examples -p bare-js -p bare && cargo build --release -p bare-stress
# Each row runs 3 times with tools/js/instr.sh (minimum of `perf stat -e instructions:u`) on both trees under glibc (with the
# tcache tunable that instr.sh sets). The jemalloc runs (LD_PRELOAD=/usr/lib/libjemalloc.so.2) occur only for a row that is
# over PERCENT under glibc, because the rule needs both. Counts of BASE_DIR are cached in BASE_DIR/cost-cache, keyed by the
# row and the binary's mtime, so a second run against the same base measures only NEW_DIR.
# A row breaks the rule when it is over PERCENT (default 0.5) under BOTH allocators. The exit status is 1 then, and also
# when a row fails to run. Rows over the limit under one allocator only are listed as noise candidates.
#   -j JOBS   rows in parallel (default 4). Instruction counts do not depend on it (docs/progress.md, "Gates and baselines").
#   -g REGEX  run only the rows that match (grep -E), for example 'eval_load|fib'
#   -a        print the whole table, not only the summary and the flagged rows
#   -J        run jemalloc on every row
#   -o FILE   keep the raw results (one tab-separated line for each row)
# Runtime of all 803 rows with -j4 on an idle machine: about 10 minutes with -J; about half of that without -J, and about
# a quarter when the base counts are cached (an 18-row subset: 57 s, 28 s and 12 s). Use -g for a subset.
# Needs GNU xargs, perf and awk. Parallel rows share the CPU but not the counters.
# Environment: BARE_TMP (default /data/bare-tmp, else ~/bare-tmp) holds the temp files and `bin/heavy`. JEMALLOC (default
# /usr/lib/libjemalloc.so.2, else /usr/lib64/libjemalloc.so.2). All rows run inside one `heavy` slot (BARE_HEAVY_SLOTS).
# `heavy` inside `heavy` takes no second slot, so the caller can wrap this script too.
# BASE_DIR can be the output of `base-build` (the shared build of origin/main).
set -u
here=$(dirname "$0")
jobs=4 pat= pct=0.5 all= raw=
PCT=${PCT:-0.5} FULL=${FULL:-}
rows=$here/cost_rows.txt
[ -d "${BARE_TMP:-}" ] || { [ -d /data/bare-tmp ] && BARE_TMP=/data/bare-tmp || BARE_TMP=$HOME/bare-tmp; }
export BARE_TMP
if [ -z "${JEMALLOC:-}" ]; then
  JEMALLOC=/usr/lib/libjemalloc.so.2; [ -e "$JEMALLOC" ] || JEMALLOC=/usr/lib64/libjemalloc.so.2
fi
export JEMALLOC
heavy=$BARE_TMP/bin/heavy; [ -x "$heavy" ] || heavy=

# Worker: `cost_matrix.sh --one BASE NEW ROW` prints "ROW <tab> glibc_base glibc_new jemalloc_base jemalloc_new".
if [ "${1:-}" = --one ]; then
  base=$2 new=$3 row=$4
  set -- $row
  envs=
  while case $1 in *=*) true ;; *) false ;; esac; do envs="$envs $1"; shift; done
  exe=$1; shift
  line=$row
  # Count one tree under one allocator. Base counts are cached in the base tree, keyed by the row and the binary's mtime.
  count() {
    c=
    if [ "$2" = "$base" ]; then
      key=$(printf '%s %s %s' "$1" "$(stat -c %Y "$2/release/$exe" 2>/dev/null)" "$row" | md5sum | cut -c1-16)
      c=$2/cost-cache/$key
      if [ -s "$c" ]; then cat "$c"; return; fi
    fi
    if [ "$1" = jemalloc ]; then
      n=$(LD_PRELOAD=$JEMALLOC "$here/instr.sh" env $envs "$2/release/$exe" $rest 2>/dev/null)
    else
      n=$("$here/instr.sh" env $envs "$2/release/$exe" $rest 2>/dev/null)
    fi
    n=${n:-FAIL}
    if [ -n "${c:-}" ] && [ "$n" != FAIL ]; then mkdir -p "${c%/*}" && echo "$n" > "$c.$$" && mv "$c.$$" "$c"; fi
    c=
    echo "$n"
  }
  rest=$*
  run() { count "$1" "$2"; }
  gb=$(run glibc "$base") gn=$(run glibc "$new")
  # The rule fails a row only when it is worse under both allocators, so jemalloc runs only for a row over the limit under glibc.
  if [ -z "$FULL" ] && [ "$gb" != FAIL ] && [ "$gn" != FAIL ] && awk -v b="$gb" -v n="$gn" -v p="$PCT" 'BEGIN { exit !((n - b) * 100 / b <= p) }'; then
    jb=- jn=-
  else
    jb=$(run jemalloc "$base") jn=$(run jemalloc "$new")
  fi
  line="$line$(printf '\t%s' "$gb" "$gn" "$jb" "$jn")"
  echo "$line"
  exit 0
fi

while getopts j:g:t:aJo: o; do
  case $o in
    j) jobs=$OPTARG ;; g) pat=$OPTARG ;; t) pct=$OPTARG ;; a) all=1 ;; J) FULL=1 ;; o) raw=$OPTARG ;;
    *) sed -n '2,17p' "$0"; exit 2 ;;
  esac
done
shift $((OPTIND - 1))
[ $# -eq 2 ] || { sed -n '2,17p' "$0"; exit 2; }
base=$(realpath "$1") new=$(realpath "$2")
mkdir -p "$BARE_TMP/tmp"; tmp=$(mktemp -d "$BARE_TMP/tmp/cost-matrix.XXXXXX") || exit 1
trap 'rm -rf "$tmp"' EXIT HUP INT TERM
grep -Ev '^(#|$)' "$rows" | grep -E -e "${pat:-.}" > "$tmp/rows" || { echo "no row matches '$pat'" >&2; exit 2; }
echo "$(wc -l < "$tmp/rows") rows, $jobs jobs, limit +$pct% under both allocators" >&2
start=$(date +%s)
PCT=$pct FULL=$FULL ${heavy:+"$heavy"} xargs -d '\n' -P "$jobs" -n 1 "$0" --one "$base" "$new" < "$tmp/rows" > "$tmp/raw"
[ -z "$raw" ] || cp "$tmp/raw" "$raw"
sort -t'	' -k1,1 "$tmp/raw" | awk -F'\t' -v pct="$pct" -v all="$all" -v secs="$(( $(date +%s) - start ))" '
function ch(b, n) { return (n - b) * 100 / b }
{
  if ($2 == "FAIL" || $3 == "FAIL" || $4 == "FAIL" || $5 == "FAIL") { fail[++nf] = $1; next }
  g = ch($2, $3); n++
  if ($4 == "-") { j = -1e9; line[n] = sprintf("%-52s %14d %14d %+8.3f%% %14s %14s %9s", $1, $2, $3, g, "-", "-", "skipped") }
  else { j = ch($4, $5); sj += j; nj++; line[n] = sprintf("%-52s %14d %14d %+8.3f%% %14d %14d %+8.3f%%", $1, $2, $3, g, $4, $5, j) }
  gs[n] = g; js[n] = j; row[n] = $1
  if (g > pct && j > pct) both[++nb] = n; else if (g > pct || j > pct) one[++no] = n
  sg += g
}
END {
  if (all) { printf "%-52s %14s %14s %9s %14s %14s %9s\n", "row", "glibc base", "glibc new", "change", "jemalloc base", "jemalloc new", "change"; for (i = 1; i <= n; i++) print line[i] }
  if (no) { print "\nOver +" pct "% under one allocator only (noise candidates):"; for (i = 1; i <= no; i++) print "  " line[one[i]] }
  if (nb) { print "\nOVER +" pct "% UNDER BOTH:"; for (i = 1; i <= nb; i++) print "  " line[both[i]] }
  for (i = 1; i <= nf; i++) print "FAILED to run: " fail[i]
  printf "\n%d rows in %d s. Mean change: glibc %+.3f%%, jemalloc %+.3f%% (%d rows measured). Over under one allocator: %d. Over under both: %d. Failed: %d.\n", n, secs, n ? sg / n : 0, nj ? sj / nj : 0, nj, no, nb, nf
  exit (nb || nf) ? 1 : 0
}'
