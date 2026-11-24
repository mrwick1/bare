#!/usr/bin/env bash
# Where the CPU of a journey goes, by browser subsystem: `perf record` of bare-drive, then classify.py and jscat.py.
#   subsystem_profile.sh run [--runs N] [--out DIR] [--rate HZ] [--no-sudo] [--keep] [--secrets FILE] [--replay DIR] [--var K=V]... BARE_DRIVE STEPS
#   subsystem_profile.sh report SCRIPT.txt...      only the tables, from `perf script -F tid,period,ip,sym` files
#
# STEPS is a steps file, or the name of a journey in tools/drive/public (its `--var URL=` comment gives URL). Each run is a new process.
# --secrets FILE goes to bare-drive as it is (EMAIL=, PASSWORD=): make it in /dev/shm and delete it after, this script never reads a vault.
# --replay DIR sets BARE_NET_REPLAY=DIR and BARE_SEED=1 (a journey recorded by journeys.sh: DIR is CORPUS/<journey>), so the network input is the same in each run.
# BARE_* variables of the caller (BARE_CLOCK, ...) reach bare-drive. A run that does not pass is an error: its shares do not count.
# perf runs as `sudo -n perf record`, and the journey runs as you (`setpriv`). With perf_event_paranoid=2 (archMachine) `perf record --call-graph fp` fails for a user.
# --no-sudo runs perf as you, for a machine with perf_event_paranoid of 1 or less.
# --keep keeps OUT/runN.data (perf.data) and OUT/runN.txt (perf script, 100 to 500 MB) after the tables. Tables: OUT/report.txt.
#
# The profiling build (one tree, its own CARGO_TARGET_DIR; frame pointers make --call-graph fp work, without them the stacks are cut and the collector is undercounted):
#   CARGO_PROFILE_RELEASE_STRIP=false CARGO_PROFILE_RELEASE_DEBUG=1 RUSTFLAGS="-C force-frame-pointers=yes" cargo build --release -p bare-drive
# The defaults, 4 kHz `cpu-clock`, are the method of 2026-11-18. A journey of one second gives about 4,000 samples, so the shares are rough (about 1 point at 4,000 samples, more at fewer):
# use more runs, or look at instruction counts (tools/bench/journeys.sh run) for a change of cost. Shares hold under load, wall times do not.
set -eu
here=$(cd "$(dirname "$0")" && pwd)

report() {
  { python3 -I "$here/classify.py" "$@"; echo; python3 -I "$here/jscat.py" "$@"; }
}

[ $# -ge 1 ] || { sed -n '2,5p' "$0" | sed 's/^# \{0,1\}//' >&2; exit 2; }
cmd=$1; shift
[ "$cmd" != report ] || { report "$@"; exit; }
[ "$cmd" = run ] || { echo "unknown command $cmd (run, report)" >&2; exit 2; }

runs=3 rate=4000 out= sudo=1 keep= secrets= replay= vars=()
while [ $# -gt 0 ]; do
  case $1 in
    --runs) runs=$2; shift 2;; --rate) rate=$2; shift 2;; --out) out=$2; shift 2;;
    --no-sudo) sudo=; shift;; --keep) keep=1; shift;;
    --secrets) secrets=$2; shift 2;; --replay) replay=$2; shift 2;; --var) vars+=(--var "$2"); shift 2;;
    --*) echo "unknown option $1" >&2; exit 2;; *) break;;
  esac
done
[ $# -eq 2 ] || { echo "need BARE_DRIVE and STEPS" >&2; exit 2; }
drive=$(readlink -f "$1") steps=$2
if [ ! -f "$steps" ]; then
  steps=$here/../drive/public/$2.steps
  [ -f "$steps" ] || { echo "no steps file or public journey $2" >&2; exit 2; }
  url=$(grep -o -m1 -e '--var URL=[^ ]*' "$steps" | cut -d= -f2-)
  [ -z "$url" ] || vars+=(--var "URL=$url")
fi
out=${out:-$(mktemp -d -t subsystem-profile.XXXXXX)}; mkdir -p "$out"
envs=(HOME="$HOME" PATH="$PATH" XDG_RUNTIME_DIR="${XDG_RUNTIME_DIR:-}" $(env | grep '^BARE_' || true))
[ -z "$replay" ] || envs+=(BARE_SEED=1 BARE_NET_REPLAY="$replay")
args=(--quiet --timeout 60000 ${secrets:+--secrets "$secrets"} "${vars[@]}")
perf=(perf record -q -F "$rate" -e cpu-clock --call-graph fp)
runas=(env "${envs[@]}")
if [ -n "$sudo" ]; then
  perf=(sudo -n "${perf[@]}"); runas=(setpriv --reuid="$(id -u)" --regid="$(id -g)" --init-groups "${runas[@]}")
fi

scripts=()
for i in $(seq 1 "$runs"); do
  "${perf[@]}" -o "$out/run$i.data" -- "${runas[@]}" "$drive" "${args[@]}" --out "$out/drive$i" "$steps" > "$out/run$i.stdout" 2> "$out/run$i.stderr" || true
  [ -z "$sudo" ] || sudo -n chown "$(id -u):$(id -g)" "$out/run$i.data"
  if ! grep -q '"ok": *true' "$out/run$i.stdout"; then
    echo "run $i did not pass; see $out/run$i.stdout and $out/run$i.stderr" >&2; exit 1
  fi
  perf script -i "$out/run$i.data" -F tid,period,ip,sym > "$out/run$i.txt" 2> /dev/null
  [ -n "$keep" ] || rm -f "$out/run$i.data"
  scripts+=("$out/run$i.txt")
  echo "run $i passed" >&2
done
report "${scripts[@]}" | tee "$out/report.txt"
[ -n "$keep" ] || rm -f "${scripts[@]}"
