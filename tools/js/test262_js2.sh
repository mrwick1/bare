#!/bin/sh
# Run the pinned Test262 subset on Vex. Reports results without enforcing a baseline.
# Usage: tools/js/test262_js2.sh [-j JOBS] [-g REGEX] [-o OUTDIR]
# The runner verifies compiled bytecode. TEST262_RUN overrides the runner binary.
# TEST262 overrides the checkout.
# T262_DIR_TIMEOUT sets the worker timeout in seconds (default 120).
# OUTDIR contains new.txt (per-test results) and counts.txt (pass/fail/skip by directory).
set -eu
here=$(dirname "$0")
fix=$here/../fixtures/js
[ -d "${BARE_TMP:-}" ] || { [ -d /data/bare-tmp ] && BARE_TMP=/data/bare-tmp || BARE_TMP=$HOME/bare-tmp; }
export BARE_TMP TMPDIR=${TMPDIR:-$BARE_TMP/tmp}
mkdir -p "$TMPDIR"
T262=${TEST262:-$BARE_TMP/test262}
export T262
runner=${TEST262_RUN:-${CARGO_TARGET_DIR:-target}/release/examples/test262_run}

# Worker: `test262_js2.sh --dir DIR OUTFILE` runs the tests in DIR (relative to the checkout) and writes their results.
if [ "${1:-}" = --dir ]; then
  dir=$2 out=$3
  w=$(mktemp -d "$TMPDIR/t262.XXXXXX")
  trap 'rm -rf "$w"' EXIT
  (cd "$T262" && ls "$dir" | grep -E '\.js$' | grep -v '_FIXTURE' | sed "s|^|$dir/|") > "$w/list"
  : > "$out"
  while [ -s "$w/list" ]; do
    timeout "${T262_DIR_TIMEOUT:-120}" "$runner" "$T262" "$fix/test262-js2-skip.txt" "$w/list" > "$w/raw" 2>/dev/null && rc=0 || rc=$?
    sed '/^START /d' "$w/raw" >> "$out"
    last=$(tail -n 1 "$w/raw")
    [ $rc -eq 0 ] && break
    case $last in
      "START "*) bad=${last#START } ;;
      *) echo "Test262 worker failed for $dir (status $rc)" >&2; exit 1 ;;
    esac
    printf 'FAIL %s\tprocess died or timed out (status %s)\n' "$bad" "$rc" >> "$out"
    awk -v d="$bad" 'f { print } $0 == d { f = 1 }' "$w/list" > "$w/next"
    mv "$w/next" "$w/list"
  done
  exit 0
fi

jobs=4 pat= outdir=$TMPDIR/test262-js2-out
while getopts j:g:o: o; do
  case $o in
    j) jobs=$OPTARG ;; g) pat=$OPTARG ;; o) outdir=$OPTARG ;;
    *) sed -n '2,6p' "$0" >&2; exit 2 ;;
  esac
done
sub=$fix/test262-js2-subset.txt
pin=$(sed -n 's/^# commit //p' "$fix/test262-subset.txt" | head -n 1)
have=$(git -C "$T262" rev-parse HEAD 2>/dev/null) || { echo "no test262 checkout at $T262 (see the header)" >&2; exit 2; }
[ "$have" = "$pin" ] || { echo "test262 at $have, the subset pins $pin" >&2; exit 2; }
if [ -z "${TEST262_RUN:-}" ]; then
  cargo build --release -q -p bare-vex --features verify-compiled --example test262_run || exit 1
fi
mkdir -p "$outdir"
parts=$(mktemp -d "$outdir/parts.XXXXXX")
trap 'rm -rf "$parts"' EXIT
start=$(date +%s)
entries=$(grep -E '^[^#-]' "$sub")
skips=$(sed -n 's/^-//p' "$sub" | sed 's|^|test/|' | paste -sd'|')
(cd "$T262/test" && for e in $entries; do find "$e" -type f -name '*.js' ! -name '*_FIXTURE*' -printf 'test/%h\n'; done) | sort -u \
  | grep -E -e "${pat:-.}" | { if [ -n "$skips" ]; then grep -Ev -e "^($skips)(/|\$)"; else cat; fi; } > "$outdir/dirs"

[ -s "$outdir/dirs" ] || { echo "No Test262 directories matched" >&2; exit 2; }
n=0
while read -r d; do
  n=$((n + 1))
  printf '%s\0%s\0' "$d" "$parts/$n"
done < "$outdir/dirs" | xargs -0 -n 2 -P "$jobs" "$0" --dir
cat "$parts"/* | LC_ALL=C sort -k2,2 > "$outdir/new.txt"
awk -F'\t' '{ split($1, a, " "); p = a[2]; sub("^test/", "", p); sub("/[^/]*$", "", p); c[p " " a[1]]++; d[p] = 1 }
  END { for (p in d) printf "%s %d %d %d\n", p, c[p " PASS"], c[p " FAIL"], c[p " SKIP"] }' "$outdir/new.txt" | LC_ALL=C sort > "$outdir/counts.txt"
awk '{ pass += $2; fail += $3; skip += $4 }
  END { printf "%d passed, %d failed, %d skipped\n", pass, fail, skip }' "$outdir/counts.txt"
echo "finished in $(( $(date +%s) - start )) s; results in $outdir"
