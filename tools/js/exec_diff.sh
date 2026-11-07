#!/bin/sh
# Differential gate (G6): run every fixture of tools/fixtures/js/*-cases.txt with its runner under BARE_JS_EXEC=tree, auto and
# byte, and compare the outputs. They must be equal. The load examples that check their own result (eval, gen, async, class,
# proxy; scale 1) run in each mode too: their exit status must be 0 in every mode.
#   tools/js/exec_diff.sh [-m "MODES"] [-g REGEX] [TARGET_DIR]
# TARGET_DIR is a CARGO_TARGET_DIR (default: CARGO_TARGET_DIR, else target). The runners are built there first, inside `heavy`.
# -m changes the modes (default "tree auto byte"). -g keeps
# only the pairs whose line "RUNNER CASESFILE" matches. The engine reads BARE_JS_EXEC only after step B3 of the engine plan; until
# then all three modes run the same engine and the outputs are equal.
# The pairs below are the runners of the *_check.sh scripts (the oracle is Node there, here it is the tree mode).
# Not here: parse-cases.txt (parser only) and numeric-cases.txt (read by numeric_oracle.mjs).
set -u
here=$(dirname "$0")
modes="tree auto byte" pat=.
while getopts m:g: o; do
  case $o in m) modes=$OPTARG ;; g) pat=$OPTARG ;; *) sed -n '2,12p' "$0"; exit 2 ;; esac
done
shift $((OPTIND - 1))
[ -d "${BARE_TMP:-}" ] || { [ -d /data/bare-tmp ] && BARE_TMP=/data/bare-tmp || BARE_TMP=$HOME/bare-tmp; }
export BARE_TMP TMPDIR=${TMPDIR:-$BARE_TMP/tmp}
mkdir -p "$TMPDIR"
heavy=$BARE_TMP/bin/heavy; [ -x "$heavy" ] || heavy=
export CARGO_TARGET_DIR=${1:-${CARGO_TARGET_DIR:-target}}
fix=$here/../fixtures/js
pairs='async_check adapter-async-cases.txt
async_check async-cases.txt
async_check asyncgen-cases.txt
async_check interact-cases.txt
browser_check browser-event-cases.txt
dom_check dom-cases.txt
eval_check adapter-cases.txt
eval_check arrobj-cases.txt
eval_check bigint-cases.txt
eval_check binary-cases.txt
eval_check builtins1-cases.txt
eval_check builtins-cases.txt
eval_check class-cases.txt
eval_check date-cases.txt
eval_check eval-cases.txt
eval_check generator-cases.txt
eval_check intl-cases.txt
eval_check intl-num-cases.txt
eval_check iterator-cases.txt
eval_check number-cases.txt
eval_check revive-cases.txt
eval_check statements-cases.txt
eval_check stringify-cases.txt
eval_check subclass-cases.txt
eval_check tagged-cases.txt
eval_check typed-cases.txt
eval_check wrapper-cases.txt
fetch_check fetch-cases.txt
fetch_check streams-cases.txt
navigation_check navigation-url-cases.txt
promise_check matchall-cases.txt
promise_check promise-cases.txt
promise_check promise-combinator-cases.txt
promise_check proxy-cases.txt
promise_check reflect-cases.txt
promise_check sc-cases.txt
promise_check seam-cases.txt
promise_check segmenter-cases.txt
promise_check subclass-promise-cases.txt
promise_check weak-cases.txt
scheduler_check scheduler-cases.txt'
$heavy cargo build --release -q -p bare-js --example eval_check --example async_check --example promise_check \
  --example eval_load --example gen_load --example async_load --example class_load --example proxy_load || exit 1
$heavy cargo build --release -q -p bare --example browser_check --example dom_check --example fetch_check \
  --example navigation_check --example scheduler_check || exit 1
tmp=$(mktemp -d "$TMPDIR/exec-diff.XXXXXX")
trap 'rm -rf "$tmp"' EXIT HUP INT TERM
bin=$CARGO_TARGET_DIR/release/examples
: > "$tmp/n"; : > "$tmp/bad"
echo "$pairs" | grep -E -e "$pat" | while read -r runner cases; do
  first=
  for m in $modes; do
    BARE_JS_EXEC=$m "$bin/$runner" "$fix/$cases" > "$tmp/out.$m" 2>/dev/null
    echo "rc=$?" >> "$tmp/out.$m"
    if [ -z "$first" ]; then first=$m
    elif ! cmp -s "$tmp/out.$first" "$tmp/out.$m"; then
      echo "DIFFERENT: $runner $cases: $first against $m"; diff "$tmp/out.$first" "$tmp/out.$m" | head -10
      echo x >> "$tmp/bad"
    fi
  done
  echo x >> "$tmp/n"
done
grep -E 'examples/(eval|gen|async|class|proxy)_load .* 1$' "$here/cost_rows.txt" | grep -E -e "$pat" | while read -r row; do
  set -- $row
  for m in $modes; do
    if ! BARE_JS_EXEC=$m "$bin/${1#examples/}" "$2" "$3" > /dev/null 2>&1; then
      echo "FAILED: $row under BARE_JS_EXEC=$m"; echo x >> "$tmp/bad"
    fi
  done
  echo x >> "$tmp/n"
done
n=$(wc -l < "$tmp/n" 2>/dev/null || echo 0)
bad=$(wc -l < "$tmp/bad" 2>/dev/null || echo 0)
echo "exec_diff: $n runs of fixtures and load rows, modes: $modes, differences: $bad"
[ "$bad" -eq 0 ]
