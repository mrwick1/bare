#!/bin/sh
# test262 gate (G5): run the subset with bare-js and compare the pass count of each directory with the baseline.
#   tools/js/test262_check.sh [-j JOBS] [-g REGEX] [-u] [-o OUTDIR] [-p PREV_RESULTS]
# The checkout is not in the repository. Clone it at the commit that the first line `# commit SHA` of
# tools/fixtures/js/test262-subset.txt names (about 270 MB, 54,000 test files):
#   git clone --depth 1 https://github.com/tc39/test262 $BARE_TMP/test262   (or: git fetch --depth 1 origin SHA, then checkout)
# TEST262 names another path. The script stops when the checkout is at another commit.
# Files: test262-subset.txt (the directories to run, below test/), test262-skip.txt (the rules of the runner; read
# crates/js/examples/test262_run.rs), test262-baseline.txt (`directory pass fail skip` for each directory that holds tests).
# A test with no flag runs sloppy and strict. The step budget of a test (20 M) is its timeout. A directory process that
# dies or runs for more than T262_DIR_TIMEOUT seconds (default 600) fails the test that it ran, and the run goes on.
# Gate: the exit status is 1 when a directory has fewer passing tests than the baseline, or a baseline directory is missing.
# Directories with more passing tests are printed (newly passing). With -p PREV_RESULTS (a results.txt of an earlier run) the
# tests that changed are printed one by one.
#   -j JOBS   directories in parallel (default BARE_HEAVY_SLOTS, else 3); each one runs inside `heavy`
#   -g REGEX  only the directories that match (grep -E); nothing is checked for the others
#   -u        write the counts as the new baseline (not with -g)
#   -o DIR    keep the results in DIR (default $BARE_TMP/test262-out): results.txt has one line for each test
# Runtime: see docs/progress.md, "Gates and baselines". Needs GNU find and timeout.
set -u
here=$(dirname "$0")
fix=$here/../fixtures/js
[ -d "${BARE_TMP:-}" ] || { [ -d /data/bare-tmp ] && BARE_TMP=/data/bare-tmp || BARE_TMP=$HOME/bare-tmp; }
export BARE_TMP TMPDIR=${TMPDIR:-$BARE_TMP/tmp}
mkdir -p "$TMPDIR"
heavy=$BARE_TMP/bin/heavy; [ -x "$heavy" ] || heavy=
T262=${TEST262:-$BARE_TMP/test262}
export T262
runner=${TEST262_RUN:-${CARGO_TARGET_DIR:-target}/release/examples/test262_run}

# Worker: `test262_check.sh --dir DIR OUTFILE` runs the tests in DIR (relative to the checkout) and writes their results.
if [ "${1:-}" = --dir ]; then
  dir=$2 out=$3
  w=$(mktemp -d "$TMPDIR/t262.XXXXXX")
  trap 'rm -rf "$w"' EXIT
  (cd "$T262" && ls "$dir" | grep -E '\.js$' | grep -v '_FIXTURE' | sed "s|^|$dir/|") > "$w/list"
  : > "$out"
  while [ -s "$w/list" ]; do
    timeout "${T262_DIR_TIMEOUT:-600}" "$runner" "$T262" "$fix/test262-skip.txt" "$w/list" > "$w/raw" 2>/dev/null
    rc=$?
    grep -v '^START ' "$w/raw" >> "$out"
    last=$(tail -n 1 "$w/raw")
    [ $rc -eq 0 ] && break
    case $last in
      "START "*) bad=${last#START } ;;
      *) break ;;
    esac
    printf 'FAIL %s\tprocess died or timed out (status %s)\n' "$bad" "$rc" >> "$out"
    awk -v d="$bad" 'f { print } $0 == d { f = 1 }' "$w/list" > "$w/next"
    mv "$w/next" "$w/list"
  done
  exit 0
fi

jobs=${BARE_HEAVY_SLOTS:-3} pat= update= outdir=$BARE_TMP/test262-out prev=
while getopts j:g:uo:p: o; do
  case $o in
    j) jobs=$OPTARG ;; g) pat=$OPTARG ;; u) update=1 ;; o) outdir=$OPTARG ;; p) prev=$OPTARG ;;
    *) sed -n '2,22p' "$0"; exit 2 ;;
  esac
done
[ -z "$update" ] || [ -z "$pat" ] || { echo "-u does not go with -g" >&2; exit 2; }
sub=$fix/test262-subset.txt base=$fix/test262-baseline.txt
pin=$(sed -n 's/^# commit //p' "$sub" | head -n 1)
have=$(git -C "$T262" rev-parse HEAD 2>/dev/null) || { echo "no test262 checkout at $T262 (see the header)" >&2; exit 2; }
[ "$have" = "$pin" ] || { echo "test262 at $have, the subset pins $pin" >&2; exit 2; }
if [ -z "${TEST262_RUN:-}" ]; then
  $heavy cargo build --release -q -p bare-js --example test262_run || exit 1
fi
mkdir -p "$outdir/parts"; rm -f "$outdir/parts"/*
start=$(date +%s)
entries=$(grep -Ev '^(#|$)' "$sub")
(cd "$T262/test" && for e in $entries; do find "$e" -type f -name '*.js' ! -name '*_FIXTURE*' -printf 'test/%h\n'; done) | sort -u | grep -E -e "${pat:-.}" > "$outdir/dirs"
echo "$(wc -l < "$outdir/dirs") directories, $jobs jobs" >&2
n=0
while read -r d; do
  n=$((n + 1))
  printf '%s\0%s\0' "$d" "$outdir/parts/$n"
done < "$outdir/dirs" | xargs -0 -n 2 -P "$jobs" ${heavy:+"$heavy"} "$0" --dir
cat "$outdir/parts"/* | sort -k2,2 > "$outdir/results.txt"
awk -F'\t' '{ split($1, a, " "); p = a[2]; sub("^test/", "", p); sub("/[^/]*$", "", p); c[p " " a[1]]++; d[p] = 1 }
  END { for (p in d) printf "%s %d %d %d\n", p, c[p " PASS"], c[p " FAIL"], c[p " SKIP"] }' "$outdir/results.txt" | sort > "$outdir/counts.txt"
secs=$(( $(date +%s) - start ))

if [ -n "$update" ]; then
  { echo "# test262 pass counts for each directory: directory, pass, fail, skip. Commit: $pin."
    echo "# Written by tools/js/test262_check.sh -u. Do not edit by hand."
    cat "$outdir/counts.txt"; } > "$base"
  echo "baseline written: $base"
fi
awk -v secs="$secs" '{ p += $2; f += $3; s += $4; split($1, a, "/"); k = a[1] "/" a[2]; gp[k] += $2; gf[k] += $3; gs[k] += $4 }
  END { for (k in gp) printf "  %-40s pass %6d fail %6d skip %5d\n", k, gp[k], gf[k], gs[k] | "sort"; close("sort"); printf "total: pass %d, fail %d, skip %d, in %d s\n", p, f, s, secs }' "$outdir/counts.txt"
echo "fail reasons (top 12):"
awk -F'\t' '$1 ~ /^FAIL/ { r = $2; gsub(/[0-9]+/, "N", r); sub(/^(sloppy|strict): /, "", r); print substr(r, 1, 70) }' "$outdir/results.txt" | sort | uniq -c | sort -rn | head -12

# The gate.
awk -v pat="${pat:-}" '
  FNR == NR { if ($0 !~ /^#/ && (pat == "" || $1 ~ pat)) { bp[$1] = $2 } next }
  { np[$1] = $2 }
  END {
    bad = 0
    for (d in bp) {
      if (!(d in np)) { print "MISSING directory: " d; bad++ }
      else if (np[d] < bp[d]) { printf "LOST %d passing in %s (%d -> %d)\n", bp[d] - np[d], d, bp[d], np[d]; bad++ }
      else if (np[d] > bp[d]) { printf "newly passing +%d in %s (%d -> %d)\n", np[d] - bp[d], d, bp[d], np[d] }
    }
    for (d in np) if (!(d in bp)) printf "new directory (not in the baseline): %s, %d passing\n", d, np[d]
    printf "gate: %s\n", bad ? bad " directories lost tests" : "ok"
    exit bad ? 1 : 0
  }' "$base" "$outdir/counts.txt" | sort | tee "$outdir/gate.txt"
rc=$?
if [ -n "$prev" ] && [ -f "$prev" ]; then
  echo "changed tests against $prev:"
  awk -F'\t' 'FNR == NR { split($1, a, " "); was[a[2]] = a[1]; next }
    { split($1, a, " "); if (a[2] in was && was[a[2]] != a[1]) print was[a[2]] " -> " a[1] "  " a[2] }' "$prev" "$outdir/results.txt" | sort | head -200
fi
grep -q '^gate: ok' "$outdir/gate.txt" || rc=1
exit $rc
