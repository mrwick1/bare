#!/usr/bin/env python3
"""Count who forces style and layout (docs/progress.md, "Who forces style and layout"; docs/design-suite.md, section 3.5).

usage: force_count.py BARE_DRIVE [--only TEXT] [--suite DIR]

Runs the public journeys (`tools/drive/public/*.steps`, live) or, with `--suite DIR`, the suite with one worker against the fixture server
(`tools/suite/server.py`). Each process runs under `perf stat -e instructions:u` with `BARE_FORCE_STATS=1`: `crates/bare/src/force.rs`
writes one `FS` line to stderr for each style pass and each layout pass. The script adds them up by caller and prints two tables:
the passes by caller, and the layout of frames that no geometry step used (what a styles-only frame would not lay out).
Run it inside `heavy`. A journey that fails is left out.
"""
import collections
import glob
import json
import os
import re
import subprocess
import sys
import tempfile

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
SRC = {}


def source(path, line):
    """The text of a source line, to name a `file:line` caller."""
    if path not in SRC:
        try:
            SRC[path] = open(os.path.join(ROOT, path)).read().split("\n")
        except OSError:
            SRC[path] = []
    lines = SRC[path]
    return lines[line - 1].strip() if 0 < line <= len(lines) else ""


def parse(text):
    """Events of one process: ('style'|'layout', outcome, caller, n, instr) and ('frame', caller, asked)."""
    out = []
    for raw in text.splitlines():
        t = raw.split()
        if len(t) < 4 or t[0] != "FS":
            continue
        if t[1] == "frame":
            out.append(("frame", " ".join(t[2:-1]), t[-1] == "1"))
        else:
            out.append((t[1], t[2], " ".join(t[3:-2]), int(t[-2]), int(t[-1])))
    return out


def style_only(caller):
    """Does the caller of a frame read only styles? The frame requests that are an `inspect` call (the closure reads `frame.styles`)."""
    m = re.match(r"(.*\.rs):(\d+)$", caller)
    return bool(m) and "inspect(" in source(m.group(1), int(m.group(2)))


def run_process(cmd, env_extra=None):
    perf = tempfile.mkstemp()[1]
    env = dict(os.environ, BARE_FORCE_STATS="1", **(env_extra or {}))
    p = subprocess.run(["perf", "stat", "-x,", "-e", "instructions:u", "-o", perf, "--"] + cmd, capture_output=True, text=True, env=env)
    total = 0
    try:
        for line in open(perf):
            if "instructions" in line:
                total = int(line.split(",")[0])
        os.unlink(perf)
    except (OSError, ValueError):
        pass
    return p, total


def journeys(binary, only):
    for steps in sorted(glob.glob(os.path.join(ROOT, "tools/drive/public/*.steps"))):
        name = os.path.basename(steps)[:-6]
        if only and only not in name:
            continue
        url = re.search(r"--var URL=(\S+)", open(steps).read())
        if not url:
            continue
        p, total = run_process([binary, "--quiet", "--var", "URL=" + url.group(1), steps])
        try:
            ok = json.loads(p.stdout).get("ok")
        except ValueError:
            ok = False
        yield name, ok, total, parse(p.stderr)


def suite(binary, suite_dir, only):
    srv = subprocess.Popen([sys.executable, "-I", os.path.join(ROOT, "tools/suite/server.py")], stdout=subprocess.PIPE, text=True)
    try:
        port = (srv.stdout.readline() if srv.stdout else "").split()[1]
        out = tempfile.mkdtemp()
        cmd = [binary, "--quiet", "--var", f"BASE=http://127.0.0.1:{port}", "--var", "EMAIL=suite.user@example.test", "--var", "PASSWORD=suite pass 1!"]
        cmd += ["--suite", suite_dir, "--out", out, "--workers", "1"] + (["--only", only] if only else [])
        p, total = run_process(cmd)
        try:
            j = json.loads(p.stdout)
            ok = j["passed"] == j["tests"]
        except (ValueError, KeyError):
            ok = False
        yield "suite", ok, total, parse(p.stderr)
    finally:
        srv.kill()


def main():
    args = sys.argv[1:]
    if not args:
        sys.exit(__doc__)
    binary = os.path.abspath(args.pop(0))
    only = args[args.index("--only") + 1] if "--only" in args else None
    suite_dir = os.path.abspath(args[args.index("--suite") + 1]) if "--suite" in args else None
    runs = suite(binary, suite_dir, only) if suite_dir else journeys(binary, only)
    by = collections.defaultdict(lambda: collections.defaultdict(lambda: [0, 0, 0]))  # caller -> kind/outcome -> [passes, n, instr]
    frames = collections.defaultdict(lambda: [0, 0])  # caller -> [requests, requests that found no frame or a stale one]
    all_total, wasted, wasted_n, layouts = 0, 0, 0, 0
    rows, failed = [], []
    for name, ok, total, events in runs:
        if not ok:
            failed.append(name)
            print(f"# {name}: failed, left out", file=sys.stderr)
            continue
        all_total += total
        pending = 0  # the instructions of the last frame layout that no geometry step has used
        for e in events:
            if e[0] == "frame":
                frames[e[1]][0] += 1
                frames[e[1]][1] += e[2]
                if not style_only(e[1]):
                    pending = 0
                continue
            kind, outcome, caller, n, instr = e
            row = by[caller][f"{kind} {outcome}"]
            row[0] += 1
            row[1] += n
            row[2] += instr
            if kind == "layout" and outcome != "hit" and re.search(r"\.rs:\d+$", caller):
                layouts += 1
                if pending:
                    wasted += pending
                    wasted_n += 1
                pending = instr if style_only(caller) else 0
        if pending:
            wasted += pending
            wasted_n += 1
        rows.append((name, total))
    kinds = ["style rules", "style compute", "style restyle", "style hit", "layout full", "layout kept", "layout hit"]
    print(f"{len(rows)} runs counted, {len(failed)} failed ({' '.join(failed)}). Total {all_total / 1e6:,.0f} M instructions.\n")
    print("| caller | " + " | ".join(kinds) + " | style M | layout M | % of total |")
    print("|" + " --- |" * (len(kinds) + 4))
    table = []
    for caller, k in by.items():
        sm = sum(k[x][2] for x in kinds if x.startswith("style"))
        lm = sum(k[x][2] for x in kinds if x.startswith("layout"))
        table.append((sm + lm, caller, k, sm, lm))
    for _, caller, k, sm, lm in sorted(table, reverse=True):
        m = re.match(r"(.*\.rs):(\d+)$", caller)
        label = f"{caller} `{source(m.group(1), int(m.group(2)))[:50]}`" if m else caller
        cells = " | ".join((f"{k[x][0]} ({k[x][1]}) {k[x][2] / 1e6:.1f} M" if k[x][1] else f"{k[x][0]} {k[x][2] / 1e6:.1f} M") if x in k else "" for x in kinds)
        print(f"| {label} | {cells} | {sm / 1e6:.1f} | {lm / 1e6:.1f} | {100 * (sm + lm) / all_total:.2f} |")
    print("\n(Cell: passes (elements styled, or boxes laid out) and instructions. A restyle counts the elements whose style changed. `rules` is the plan of the sheets and the build of the rule set before a style pass; it styles no element.)")
    print("\n| frame request | requests | found no frame or a stale one |\n| --- | --- | --- |")
    for caller, (n, built) in sorted(frames.items(), key=lambda x: -x[1][0]):
        print(f"| {caller} | {n} | {built} |")
    print(f"\nFrame layouts: {layouts}. Laid out for a styles-only request and never used by a geometry step: {wasted_n} layouts, {wasted / 1e6:.1f} M instructions ({100 * wasted / all_total:.2f}% of total).")
    print("\n| run | M instructions |\n| --- | --- |")
    for name, total in rows:
        print(f"| {name} | {total / 1e6:.1f} |")


if __name__ == "__main__":
    main()
