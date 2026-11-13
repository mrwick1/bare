#!/usr/bin/env python3
"""The suite benchmark (docs/design-suite.md, section 5.3; success criterion 7 of docs/perf-goal.md).

usage: run.py BARE_DRIVE [--suite DIR] [--variants V0,V1,V2,V3,V4] [--k 2,4,8,12] [--runs 3] [--login-ms N] [--js-kb N] [--css-kb N]
              [--keep DIR] [--only TEXT] [--v0-binary PATH]

It starts the fixture server (server.py) and runs the variants against it, RUNS times each. For each run: wall time, the sum of the
instructions of all processes (`perf stat -e instructions:u`), peak RSS of each process, the tests that passed, and the login POSTs
that the server counted. A run does not count when a test failed or when the login POSTs are not the expected number.

  V0  baseline: a new bare-drive process for each test, one after another; every test logs in
  V1  one warm worker, every test logs in (`--workers 1 --login-each`)
  V2  one warm worker, the login runs once and the shared tests start from it (`--workers 1`)
  V3  V0 with K processes at a time (a pool of processes, a new process for each test, every test logs in)
  V4  V2 with K workers (`--workers K`): warm workers, one shared login
--v0-binary runs the cold variants (V0, V3) with another bare-drive, for example the frozen baseline commit; the step files still come from BARE_DRIVE.
The default suite is the one of gen_suite.py (tools/suite/tests). Output: one JSON line for each run, then a table.
"""
import argparse
import json
import os
import statistics
import subprocess
import sys
import tempfile
import threading
import time
import urllib.request
from queue import Empty, Queue

HERE = os.path.dirname(os.path.abspath(__file__))
EMAIL, PASSWORD = "suite.user@example.test", "suite pass 1!"


def stats(port):
    return json.load(urllib.request.urlopen(f"http://127.0.0.1:{port}/__stats"))


def instructions(path):
    try:
        for line in open(path):
            if "instructions" in line:
                return int(line.split(",")[0])
    except (OSError, ValueError):
        pass
    return None


def base_cmd(binary, port):
    return [binary, "--quiet", "--var", f"BASE=http://127.0.0.1:{port}", "--var", f"EMAIL={EMAIL}", "--var", f"PASSWORD={PASSWORD}"]


def run_cold_process(binary, port, steps, work, tag):
    """One bare-drive process on a self-contained step file, under perf stat. Returns (ok, instructions, peak KiB)."""
    perf = os.path.join(work, f"perf.{tag}")
    p = subprocess.run(
        ["perf", "stat", "-x,", "-e", "instructions:u", "-o", perf, "--"] + base_cmd(binary, port) + [steps],
        capture_output=True, text=True,
    )
    try:
        j = json.loads(p.stdout.splitlines()[0])
        ok, peak = bool(j["ok"]), j["runs"][-1]["peak_rss_kib"]
    except (IndexError, ValueError, KeyError):
        ok, peak = False, 0
    return ok, instructions(perf), peak


def cold_pool(binary, port, files, k, work):
    """The files, K processes at a time: a queue, and each thread takes the next file."""
    q = Queue()
    for f in files:
        q.put(f)
    res = []
    lock = threading.Lock()

    def worker():
        while True:
            try:
                f = q.get_nowait()
            except Empty:
                return
            r = run_cold_process(binary, port, f, work, os.path.basename(f))
            with lock:
                res.append(r)

    ts = [threading.Thread(target=worker) for _ in range(k)]
    [t.start() for t in ts]
    [t.join() for t in ts]
    return res


def one_run(variant, k, args, port, cold_files, work):
    before = stats(port)
    load0 = os.getloadavg()[0]
    t0 = time.perf_counter()
    peaks, passed, instr, workers = [], 0, None, k
    n = len(cold_files)
    if variant in ("V0", "V3"):
        workers = 1 if variant == "V0" else k
        res = cold_pool(args.v0_binary or args.binary, port, cold_files, workers, work)
        passed = sum(1 for r in res if r[0])
        instr = sum(r[1] for r in res) if all(r[1] is not None for r in res) else None
        peaks = [r[2] for r in res]
        wall = time.perf_counter() - t0
        expect_logins = n
    else:
        out = tempfile.mkdtemp(dir=work)
        perf = os.path.join(work, "perf.suite")
        cmd = ["perf", "stat", "-x,", "-e", "instructions:u", "-o", perf, "--"] + base_cmd(args.binary, port)
        cmd += ["--suite", args.suite, "--out", out, "--workers", str(1 if variant in ("V1", "V2") else k)]
        if variant == "V1":
            cmd.append("--login-each")
        if args.only:
            cmd += ["--only", args.only]
        p = subprocess.run(cmd, capture_output=True, text=True)
        wall = time.perf_counter() - t0
        try:
            j = json.loads(p.stdout)
            passed = j["passed"]
            peaks = [s["peak_kib"] for s in j["worker_stats"]]
            workers = j["workers"]
            expect_logins = j["logins_expected"] if variant != "V1" else n
            n = j["tests"]
        except (ValueError, KeyError):
            expect_logins = -1
        instr = instructions(perf)
    after = stats(port)
    logins = after["login_posts"] - before["login_posts"]
    ok = passed == n and logins == expect_logins
    return {
        "variant": variant, "k": workers, "ok": ok, "passed": passed, "tests": n, "wall_s": round(wall, 3),
        "instr_M": None if instr is None else round(instr / 1e6), "peak_kib_max": max(peaks, default=0),
        "peak_kib_sum": sum(peaks), "processes": len(peaks), "login_posts": logins, "login_posts_expected": expect_logins,
        "load_start": round(load0, 1), "load_end": round(os.getloadavg()[0], 1),
    }


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("binary")
    ap.add_argument("--suite", default=os.path.join(HERE, "tests"))
    ap.add_argument("--variants", default="V0,V1,V2,V3,V4")
    ap.add_argument("--k", default="2,4,8,12")
    ap.add_argument("--runs", type=int, default=3)
    ap.add_argument("--login-ms", type=int, default=0)
    ap.add_argument("--js-kb", type=int, default=300)
    ap.add_argument("--css-kb", type=int, default=60)
    ap.add_argument("--keep", default=None)
    ap.add_argument("--only", default=None)
    ap.add_argument("--v0-binary", default=None)
    args = ap.parse_args()
    args.binary = os.path.abspath(args.binary)
    args.v0_binary = os.path.abspath(args.v0_binary) if args.v0_binary else None
    work = args.keep or tempfile.mkdtemp(prefix="suite-bench-")
    os.makedirs(work, exist_ok=True)
    srv = subprocess.Popen(
        [sys.executable, "-I", os.path.join(HERE, "server.py"), "--login-ms", str(args.login_ms), "--js-kb", str(args.js_kb), "--css-kb", str(args.css_kb)],
        stdout=subprocess.PIPE, text=True,
    )
    port = int(srv.stdout.readline().split()[1])
    try:
        cold = os.path.join(work, "cold")
        subprocess.run([args.binary, "--suite", args.suite, "--cold", cold], check=True, capture_output=True)
        cold_files = sorted(os.path.join(cold, f) for f in os.listdir(cold))
        if args.only:
            cold_files = [f for f in cold_files if args.only in f]
        rows = []
        # A first run of V0 that is not counted warms the page cache of the binary and the server's code.
        one_run("V0", 1, args, port, cold_files[:2], work)
        for v in args.variants.split(","):
            for k in ([1] if v in ("V0", "V1", "V2") else [int(x) for x in args.k.split(",")]):
                for _ in range(args.runs):
                    r = one_run(v, k, args, port, cold_files, work)
                    rows.append(r)
                    print(json.dumps(r), flush=True)
        print("\nvariant  K  runs  ok   wall s (median, min-max)      instr M (median, min-max)      peak RSS MiB (max worker)  load")
        keys = sorted({(r["variant"], r["k"]) for r in rows})
        base = None
        for v, k in keys:
            rs = [r for r in rows if r["variant"] == v and r["k"] == k]
            good = [r for r in rs if r["ok"]]
            w = [r["wall_s"] for r in good] or [0]
            i = [r["instr_M"] for r in good if r["instr_M"] is not None] or [0]
            print(f"{v:7} {k:3} {len(rs):4} {len(good):3}  {statistics.median(w):7.3f} ({min(w):.3f}-{max(w):.3f})  {statistics.median(i):9.0f} ({min(i):.0f}-{max(i):.0f})  {max(r['peak_kib_max'] for r in rs) / 1024:8.1f}  {rs[0]['load_start']}-{rs[-1]['load_end']}")
            if v == "V0":
                base = (statistics.median(w), statistics.median(i))
        if base and base[0]:
            print("\nagainst V0 (median):  wall x, instructions x")
            for v, k in keys:
                good = [r for r in rows if r["variant"] == v and r["k"] == k and r["ok"]]
                if good and v != "V0":
                    w, i = statistics.median(r["wall_s"] for r in good), statistics.median(r["instr_M"] for r in good)
                    print(f"{v:7} {k:3}  {base[0] / w:5.2f}  {base[1] / i:5.2f}")
    finally:
        srv.terminate()


main()
