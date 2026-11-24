#!/bin/sh
# Gate S2 (docs/internal/plan-engine-rewrite.md): test262 for the core census features and the S2.7 built-ins, on the new engine (bare-vex) and on
# the old one (bare-js), the same tests and the same rules. It reports pass and fail for each directory on both engines, and lists each
# test that passes on the old engine and fails on the new one, grouped by the cause of the failure.
#   tools/js/test262_js2.sh [-j JOBS] [-g REGEX] [-o OUTDIR]
# TEST262 names the checkout (default $BARE_TMP/test262). It must be at the commit of tools/fixtures/js/test262-subset.txt.
# Files: test262-js2-subset.txt (the directories), test262-js2-skip.txt (the skip and `allow:` rules; read
# crates/js-diff/examples/test262_run.rs). A test with no flag runs sloppy and strict. The step budget of a test is its timeout; a
# directory process that dies or runs for more than T262_DIR_TIMEOUT seconds (default 120) fails the test that it ran.
#   -j JOBS   directories in parallel (default 4)
#   -g REGEX  only the directories that match (grep -E)
#   -u        write counts.txt as one file for each area (first two path parts) in tools/fixtures/js/test262-js2-baseline/AREA.txt,
#             only the files whose content changed (not with -g)
#   -o DIR    keep the results in DIR (default $TMPDIR/test262-js2-out): old.txt and new.txt have one line for each test,
#             counts.txt the table, regress.txt the tests that pass on the old engine and fail on the new one with their cause (regress-by-cause.txt: sorted by cause)
# The exit status is 0 when the run finished; this tool reports, it does not gate. Needs GNU find and timeout.
set -u
here=$(dirname "$0")
fix=$here/../fixtures/js
[ -d "${BARE_TMP:-}" ] || { [ -d /data/bare-tmp ] && BARE_TMP=/data/bare-tmp || BARE_TMP=$HOME/bare-tmp; }
export BARE_TMP TMPDIR=${TMPDIR:-$BARE_TMP/tmp}
mkdir -p "$TMPDIR"
T262=${TEST262:-$BARE_TMP/test262}
export T262
runner=${TEST262_RUN:-${CARGO_TARGET_DIR:-target}/release/examples/test262_run}

# Worker: `test262_js2.sh --dir ENGINE DIR OUTFILE` runs the tests in DIR (relative to the checkout) and writes their results.
if [ "${1:-}" = --dir ]; then
  engine=$2 dir=$3 out=$4
  w=$(mktemp -d "$TMPDIR/t262.XXXXXX")
  trap 'rm -rf "$w"' EXIT
  (cd "$T262" && ls "$dir" | grep -E '\.js$' | grep -v '_FIXTURE' | sed "s|^|$dir/|") > "$w/list"
  : > "$out"
  while [ -s "$w/list" ]; do
    timeout "${T262_DIR_TIMEOUT:-120}" "$runner" "$engine" "$T262" "$fix/test262-js2-skip.txt" "$w/list" > "$w/raw" 2>/dev/null
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

jobs=4 pat= update= outdir=$TMPDIR/test262-js2-out
while getopts j:g:uo: o; do
  case $o in
    j) jobs=$OPTARG ;; g) pat=$OPTARG ;; u) update=1 ;; o) outdir=$OPTARG ;;
    *) sed -n '2,16p' "$0"; exit 2 ;;
  esac
done
[ -z "$update" ] || [ -z "$pat" ] || { echo "-u does not go with -g" >&2; exit 2; }
sub=$fix/test262-js2-subset.txt
pin=$(sed -n 's/^# commit //p' "$fix/test262-subset.txt" | head -n 1)
have=$(git -C "$T262" rev-parse HEAD 2>/dev/null) || { echo "no test262 checkout at $T262 (see the header)" >&2; exit 2; }
[ "$have" = "$pin" ] || { echo "test262 at $have, the subset pins $pin" >&2; exit 2; }
if [ -z "${TEST262_RUN:-}" ]; then
  cargo build --release -q -p bare-js-diff --example test262_run || exit 1
fi
mkdir -p "$outdir/parts"; rm -f "$outdir/parts"/*
start=$(date +%s)
entries=$(grep -E '^[^#-]' "$sub")
skips=$(sed -n 's/^-//p' "$sub" | sed 's|^|test/|' | paste -sd'|')
(cd "$T262/test" && for e in $entries; do find "$e" -type f -name '*.js' ! -name '*_FIXTURE*' -printf 'test/%h\n'; done) | sort -u \
  | grep -E -e "${pat:-.}" | { if [ -n "$skips" ]; then grep -Ev -e "^($skips)(/|\$)"; else cat; fi; } > "$outdir/dirs"
echo "$(wc -l < "$outdir/dirs") directories, $jobs jobs" >&2
for engine in old new; do
  n=0
  while read -r d; do
    n=$((n + 1))
    printf '%s\0%s\0%s\0' "$engine" "$d" "$outdir/parts/$engine.$n"
  done < "$outdir/dirs" | xargs -0 -n 3 -P "$jobs" "$0" --dir
  cat "$outdir/parts/$engine".* | sort -k2,2 > "$outdir/$engine.txt"
done
secs=$(( $(date +%s) - start ))

# One row for each directory: pass, fail and skip of each engine.
count() { awk -F'\t' '{ split($1, a, " "); p = a[2]; sub("^test/", "", p); sub("/[^/]*$", "", p); c[p " " a[1]]++; d[p] = 1 }
  END { for (p in d) printf "%s %d %d %d\n", p, c[p " PASS"], c[p " FAIL"], c[p " SKIP"] }' "$1" | LC_ALL=C sort -k1,1; }
count "$outdir/old.txt" > "$outdir/old-counts.txt"
count "$outdir/new.txt" > "$outdir/new-counts.txt"
LC_ALL=C join "$outdir/old-counts.txt" "$outdir/new-counts.txt" > "$outdir/counts.txt"
# Columns: directory, old pass, old fail, old skip, new pass, new fail, new skip.
echo "per top directory (old = bare-js, new = bare-vex):"
awk '{ split($1, a, "/"); k = (a[1] == "language") ? a[1] "/" a[2] : a[1] "/" a[2]; op[k] += $2; of[k] += $3; np[k] += $5; nf[k] += $6; sk[k] += $4
       P += $2; F += $3; NP += $5; NFL += $6; S += $4 }
  END { printf "  %-34s %6s %6s   %6s %6s\n", "directory", "old ok", "old no", "new ok", "new no"
        for (k in op) printf "  %-34s %6d %6d   %6d %6d\n", k, op[k], of[k], np[k], nf[k] | "sort"; close("sort")
        printf "  %-34s %6d %6d   %6d %6d   (skipped on both: %d)\n", "total", P, F, NP, NFL, S }' "$outdir/counts.txt"

# The tests that pass on the old engine and not on the new one, with the cause on the new one.
awk -F'\t' 'FNR == NR { split($1, a, " "); if (a[1] == "PASS") ok[a[2]] = 1; next }
  { split($1, a, " "); if (a[1] == "FAIL" && (a[2] in ok)) print a[2] "\t" $2 }' "$outdir/old.txt" "$outdir/new.txt" | sort > "$outdir/regress.raw"
# The cause: the message with the numbers and quoted names taken out, cut at 70 characters.
awk -F'\t' '{ r = $2; sub(/^(sloppy|strict): /, "", r); sub(/^Test262Error: /, "assert: ", r); gsub(/[0-9]+/, "N", r); gsub(/\x27[^\x27]*\x27/, "Q", r); gsub(/"[^"]*"/, "Q", r)
  print $1 "\t" substr(r, 1, 70) }' "$outdir/regress.raw" > "$outdir/regress.txt"
sort -t"$(printf '\t')" -k2,2 -k1,1 "$outdir/regress.txt" > "$outdir/regress-by-cause.txt"
echo "tests that pass on the old engine and fail on the new one: $(wc -l < "$outdir/regress.txt") (of $(grep -c '^PASS' "$outdir/old.txt") passing on the old engine)"
echo "new engine passes, old engine fails: $(awk -F'\t' 'FNR == NR { split($1, a, " "); if (a[1] == "PASS") ok[a[2]] = 1; next } { split($1, a, " "); if (a[1] == "FAIL" && (a[2] in ok)) n++ } END { print n + 0 }' "$outdir/new.txt" "$outdir/old.txt")"
# The causes in groups (the first rule that matches wins; the rest is "other").
awk -F'\t' '{ c = $2
  if (c ~ /wrapper objects/) g = "wrapper objects (new String, Number, Boolean)"
  else if (c ~ /script function as (toString|valueOf)/) g = "ToPrimitive cannot call a script valueOf or toString"
  else if (c ~ /Array method was called on a value that is not an array/) g = "Array methods on an array-like"
  else if (c ~ /^unsupported: /) g = "construct not built in the compiler"
  else if (c ~ /Function constructor/) g = "Function constructor"
  else if (c ~ /accessor/) g = "accessors (descriptors, copies)"
  else if (c ~ /regular expression literals/) g = "regexp literal"
  else if (c ~ /ReferenceError: [A-Za-z0-9]+ is not defined/) { g = c; sub(/^ReferenceError: /, "missing global: ", g); sub(/ is not defined.*/, "", g) }
  else if (c ~ /Expected a .* to be thrown but no exception/) g = "an expected error is not thrown"
  else if (c ~ /not a function/) g = "TypeError: not a function (a method is missing)"
  else if (c ~ /should be an own property|descriptor should|to be writable|hasOwnProperty/) g = "property attributes and own properties (length, name, ...)"
  else if (c ~ /^limit:/) g = "limit"
  else if (c ~ /^assert:/) g = "other assertion"
  else g = "other error"
  print g }' "$outdir/regress.txt" | sort | uniq -c | sort -rn > "$outdir/groups.txt"
echo "pass on the old engine and fail on the new one, by directory:"
awk -F'\t' '{ split($1, a, "/"); n[a[2] "/" a[3]]++ } END { for (k in n) printf "  %-40s %5d\n", k, n[k] }' "$outdir/regress.txt" | sort
echo "groups of causes (count, group):"
cat "$outdir/groups.txt"
echo "causes (top 40; count, cause):"
cut -f2 "$outdir/regress.txt" | sort | uniq -c | sort -rn | head -40
if [ -n "$update" ]; then
  # One file for each area (the first two path parts, e.g. built-ins/Promise), so that PRs on different areas never touch the same file.
  base=$fix/test262-js2-baseline
  rm -rf "$outdir/baseline"; mkdir -p "$outdir/baseline"
  awk -v o="$outdir/baseline" -v pin="$pin" '{ split($1, a, "/"); f = o "/" a[1] "/" a[2] ".txt"
    if (!(f in seen)) { seen[f] = 1; system("mkdir -p " o "/" a[1])
      print "# test262 counts of tools/js/test262_js2.sh for each directory: directory, old pass, old fail, old skip, new pass, new fail, new skip." > f
      print "# old = bare-js, new = bare-vex. Commit: " pin ". Written by tools/js/test262_js2.sh -u. Do not edit by hand." > f }
    print > f }' "$outdir/counts.txt"
  mkdir -p "$base"
  (cd "$outdir/baseline" && find . -name '*.txt' | sort) > "$outdir/baseline.list"
  (cd "$base" && find . -name '*.txt' | sort) | while read -r f; do
    grep -qxF "$f" "$outdir/baseline.list" || rm -f "$base/$f"
  done
  while read -r f; do
    mkdir -p "$base/$(dirname "$f")"
    cmp -s "$outdir/baseline/$f" "$base/$f" || cp "$outdir/baseline/$f" "$base/$f"
  done < "$outdir/baseline.list"
fi
echo "finished in $secs s; results in $outdir"
