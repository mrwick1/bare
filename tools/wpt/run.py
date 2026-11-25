#!/usr/bin/env python3
"""Run a set of WPT tests in bare and count the results.

usage: run.py BARE_DRIVE WPT_DIR GLOB [--skip TEXT,...] [--out DIR] [--json FILE]

WPT_DIR is a checkout of web-platform-tests with `resources/`. GLOB is relative to it, for example
`css/css-display/display-contents-*.html`. A file that ends in `-ref.html` is not a test.

Each test runs in a new bare-drive process, over a local HTTP server on WPT_DIR:
- A script test (it loads `testharness.js`): the server gives its own `testharnessreport.js`. It keeps the results of
  `add_completion_callback` in `window.__wpt`. The test passes when the harness and every subtest pass.
- A reftest (`<link rel=match>`): bare takes a screenshot of the test and of its reference. The test passes when the two PNG
  files are the same bytes. Both pages render in bare: this checks bare against itself, not against Chrome.
  `rel=mismatch` passes when the files differ.
- Any other test (a crash test): it passes when the page loads.

Output: one line for each test (PASS, FAIL or ERROR, and the name), then the counts. --json writes the same results.
"""
import argparse
import functools
import glob
import http.server
import json
import os
import re
import subprocess
import sys
import tempfile
import threading

REPORT = b"""add_completion_callback(function (tests, status) {
  window.__wpt = { status: status.status, tests: tests.map(function (t) { return [t.name, t.status]; }) };
});
"""


class Handler(http.server.SimpleHTTPRequestHandler):
    def do_GET(self):
        if self.path.split("?")[0] == "/resources/testharnessreport.js":
            self.send_response(200)
            self.send_header("Content-Type", "text/javascript")
            self.send_header("Content-Length", str(len(REPORT)))
            self.end_headers()
            self.wfile.write(REPORT)
            return
        super().do_GET()

    def log_message(self, format, *args):
        pass


def drive(binary, steps, out):
    with tempfile.NamedTemporaryFile("w", suffix=".steps", delete=False) as f:
        f.write(steps)
    try:
        r = subprocess.run([binary, "--out", out, "--size", "800x600", f.name], capture_output=True, text=True, timeout=60)
        return r.returncode, r.stdout + r.stderr
    except subprocess.TimeoutExpired:
        return -1, "timeout"
    finally:
        os.unlink(f.name)


def run_one(binary, base, rel, html, work):
    url = f"{base}/{rel}"
    out = os.path.join(work, re.sub(r"[^A-Za-z0-9_-]", "_", rel))
    os.makedirs(out, exist_ok=True)
    if "testharness.js" in html:
        js = "window.__wpt ? (console.log('WPT ' + JSON.stringify(window.__wpt)), true) : false"
        code, text = drive(binary, f"timeout 10000\nopen {url}\nwait js \"{js}\"\n", out)
        m = re.search(r"WPT (\{.*?\]\]\})", text)
        if not m:
            return "ERROR", text[-300:]
        res = json.loads(m.group(1).replace('\\"', '"'))
        bad = [n for n, s in res["tests"] if s != 0]
        ok = res["status"] == 0 and not bad and res["tests"]
        return ("PASS" if ok else "FAIL"), f"{len(res['tests']) - len(bad)}/{len(res['tests'])} subtests"
    m = re.search(r"<link[^>]+rel=[\"']?(match|mismatch)[\"']?[^>]*>", html)
    if m:
        href = re.search(r"href=[\"']?([^\"' >]+)", m.group(0)).group(1)
        ref = os.path.normpath(os.path.join(os.path.dirname(rel), href))
        code, text = drive(binary, f"timeout 10000\nopen {url}\nshot test\nopen {base}/{ref}\nshot ref\n", out)
        a, b = os.path.join(out, "test.png"), os.path.join(out, "ref.png")
        if code != 0 or not (os.path.exists(a) and os.path.exists(b)):
            return "ERROR", text[-300:]
        same = open(a, "rb").read() == open(b, "rb").read()
        return ("PASS" if same == (m.group(1) == "match") else "FAIL"), ref
    code, text = drive(binary, f"timeout 10000\nopen {url}\nexpect js \"true\"\n", out)
    return ("PASS" if code == 0 else "ERROR"), text[-300:] if code else ""


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("binary")
    ap.add_argument("wpt")
    ap.add_argument("glob")
    ap.add_argument("--skip", default="")
    ap.add_argument("--out")
    ap.add_argument("--json")
    a = ap.parse_args()
    skip = [s for s in a.skip.split(",") if s]
    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), functools.partial(Handler, directory=a.wpt))
    threading.Thread(target=server.serve_forever, daemon=True).start()
    base = f"http://127.0.0.1:{server.server_address[1]}"
    work = a.out or tempfile.mkdtemp(prefix="wpt-")
    results = {}
    for path in sorted(glob.glob(os.path.join(a.wpt, a.glob))):
        rel = os.path.relpath(path, a.wpt)
        if rel.endswith("-ref.html") or any(s in rel for s in skip):
            continue
        html = open(path, encoding="utf-8", errors="replace").read()
        verdict, note = run_one(os.path.abspath(a.binary), base, rel, html, work)
        results[rel] = verdict
        print(f"{verdict:5} {rel}  {note if verdict != 'ERROR' else note.splitlines()[-1:] }", flush=True)
    server.shutdown()
    counts = {k: sum(1 for v in results.values() if v == k) for k in ("PASS", "FAIL", "ERROR")}
    print(f"total {len(results)}: " + ", ".join(f"{k} {v}" for k, v in counts.items()))
    if a.json:
        json.dump(results, open(a.json, "w"), indent=1, sort_keys=True)


if __name__ == "__main__":
    sys.exit(main())
