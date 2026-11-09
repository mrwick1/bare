#!/usr/bin/env bash
# Run the public-site journeys (*.steps here) with a built bare-drive. This is an explicit job: it needs the network, so `cargo test` never runs it.
#   tools/drive/public/run.sh [--repeat N] [--out DIR] [--only TEXT] BARE_DRIVE_BINARY
# Each journey gives OUT/<name>.json (the last attempt). The URL of a journey is its `--var URL=...` in the comment at the top of the file.
# A failed journey runs once more (network flake); two failures in a row are a failure. A journey marked `# needs-rsa` is skipped, not failed,
# when it fails with a TLS error. Exit code 0 only when no journey failed. --repeat N runs the steps N times in one process (leaks: see the RSS).
set -u
here=$(cd "$(dirname "$0")" && pwd)
repeat=1 out="" only=""
while [ $# -gt 1 ]; do
  case $1 in
    --repeat) repeat=$2 ;;
    --out) out=$2 ;;
    --only) only=$2 ;;
    *) break ;;
  esac
  shift 2
done
bin=${1:?usage: run.sh [--repeat N] [--out DIR] [--only TEXT] BARE_DRIVE_BINARY}
out=${out:-$(mktemp -d)}
mkdir -p "$out"
export HERE="$here" BIN="$bin" OUT="$out" REPEAT="$repeat" ONLY="$only"
python3 -I - <<'PY'
import glob, json, os, re, subprocess, sys, time
here, binary, out, repeat, only = os.environ["HERE"], os.environ["BIN"], os.environ["OUT"], os.environ["REPEAT"], os.environ["ONLY"]
TLS = re.compile(r"tls|handshake|certificate|rsa", re.I)
rows, failed, flakes = [], 0, 0

def attempt(steps, url, name):
    path = os.path.join(out, name + ".json")
    cmd = [binary, "--quiet", "--var", "URL=" + url, "--repeat", repeat, "--out", os.path.join(out, name), steps]
    t = time.time()
    p = subprocess.run(cmd, capture_output=True, text=True, timeout=600)
    try:
        j = json.loads(p.stdout)
    except ValueError:
        j = {"ok": False, "failure": {"message": "no JSON: " + (p.stderr or "")[-300:]}}
    j["wall_ms"] = (time.time() - t) * 1000
    with open(path, "w") as f:
        json.dump(j, f)
    return j

for steps in sorted(glob.glob(os.path.join(here, "*.steps"))):
    name = os.path.basename(steps)[:-6]
    if only not in name:
        continue
    text = open(steps).read()
    m = re.search(r"--var URL=(\S+)", text)
    url = m.group(1) if m else ""
    j = attempt(steps, url, name)
    flake = 0
    if not j["ok"]:
        j = attempt(steps, url, name)
        flake = 1 if j["ok"] else 0
    flakes += flake
    msg = (j.get("failure") or {}).get("message", "")
    if j["ok"]:
        status = "pass"
    elif "# needs-rsa" in text and TLS.search(msg + json.dumps(j.get("console", []))):
        status = "skip"
    else:
        status = "FAIL"
        failed += 1
    runs = j.get("runs") or [{}]
    rows.append((name, status, j.get("ms", j["wall_ms"]), runs[-1].get("peak_rss_kib", 0), len(j.get("network", [])), flake, msg))

print(f"{'journey':28} {'result':6} {'ms':>8} {'peak RSS KiB':>12} {'requests':>8} {'flake':>5}")
for n, s, ms, rss, req, fl, msg in rows:
    print(f"{n:28} {s:6} {ms:8.0f} {rss:12} {req:8} {fl:5}" + (f"  {msg[:200]}" if s != "pass" else ""))
c = lambda s: sum(r[1] == s for r in rows)
print(f"pass {c('pass')}  fail {c('FAIL')}  skip {c('skip')}  flakes {flakes}  out {out}")
sys.exit(1 if failed else 0)
PY
