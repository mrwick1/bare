#!/bin/bash
# idle_cpu.sh [BARE_BINARY] [SECONDS]: open `bare --window` in a headless cage compositor on a page with a pending fetch, and print
# the CPU time (utime+stime of /proc/PID/stat, all threads) and the RSS of the window process over the wait. Needs cage and python3.
# It never touches the desktop of a person: cage runs with the headless wlroots backend.
set -eu
bin=${1:-target/release/bare}; secs=${2:-10}
d=$(mktemp -d); trap 'kill $(jobs -p) 2>/dev/null || true; rm -rf "$d"' EXIT
cat > "$d/server.py" <<PY
import http.server, time
class H(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        if self.path == '/hang': time.sleep($secs + 5); body = b'late'
        else: body = b"<!doctype html><p id=out>none</p><script>fetch('/hang').then(r=>r.text()).then(t=>out.textContent=t)</script>"
        self.send_response(200); self.send_header('Content-Length', str(len(body))); self.send_header('Content-Type', 'text/html'); self.end_headers()
        try: self.wfile.write(body)
        except OSError: pass
    def log_message(self, *a): pass
s = http.server.ThreadingHTTPServer(('127.0.0.1', 0), H)
open('$d/port', 'w').write(str(s.server_address[1])); s.serve_forever()
PY
python3 "$d/server.py" & while [ ! -s "$d/port" ]; do sleep 0.05; done
export XDG_RUNTIME_DIR=${XDG_RUNTIME_DIR:-/run/user/$(id -u)}
env -u WAYLAND_DISPLAY WLR_BACKENDS=headless WLR_LIBINPUT_NO_DEVICES=1 WLR_RENDERER=pixman \
  BARE_WINDOW_STATS=1 cage -- "$bin" --window "http://127.0.0.1:$(cat "$d/port")/" 2> "$d/err" &
sleep 0.5
pid=""; for _ in $(seq 100); do pid=$(pgrep -nx "$(basename "$bin")" || true); [ -n "$pid" ] && break; sleep 0.1; done
[ -n "$pid" ] || { echo "the window process did not start"; cat "$d/err"; exit 1; }
sleep 2  # first frame and load
cpu() { awk '{ s = $0; sub(/.*\) /, "", s); split(s, f, " "); print (f[12] + f[13]) / 100 }' "/proc/$pid/stat"; }
rss() { awk '/VmRSS/ { print $2 / 1024 }' "/proc/$pid/status"; }
c0=$(cpu); r0=$(rss); sleep "$secs"; c1=$(cpu); r1=$(rss)
awk -v a="$c0" -v b="$c1" -v s="$secs" -v r0="$r0" -v r1="$r1" -v p="$pid" 'BEGIN { printf "pid %s: CPU %.3f s in %d s (%.2f%%); RSS %.1f MiB -> %.1f MiB\n", p, b - a, s, (b - a) / s * 100, r0, r1 }'
kill "$pid" 2>/dev/null || true
