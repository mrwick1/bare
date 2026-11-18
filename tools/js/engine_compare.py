#!/usr/bin/env python3
"""Compare Bare with QuickJS and Node on the `eval_load` cases: user instructions and peak RSS.

  python3 -I tools/js/engine_compare.py [--cases LIST] [--scales LIST] [--runs N] [--engines LIST] [--qjs PATH] [--out TSV] EVAL_LOAD

EVAL_LOAD is a release build of `crates/js/examples/eval_load` (`cargo build --release -p bare-js --example eval_load`).
Engines: bare-tree, bare-auto, quickjs, node-jitless and node by default; bare-new once the new engine exists. `bare-*` set BARE_JS_EXEC.
Each case comes from `eval_load CASE SCALE --source`, so all engines run the same program; the result of every run is
checked against the expected result. QuickJS and Node run it through an indirect `eval` and print it. They run in the
script's directory with a relative file name: the name is part of each `Error` stack, and on QuickJS the `throws` case
moved from 85 M to 99 M instructions with the length of the path (2026-11-18).
Instructions: `perf stat -e instructions:u`, the minimum of N runs (default 3). Peak RSS: the median of N runs through
`rss_peak.c`, a small C parent (a Python parent would add its own peak: a forked child keeps it through `exec`).
Without --qjs, QuickJS 2026-06-04 is downloaded once, checked against its SHA-256 and built in $BARE_TMP.
Each count includes the start and the end of the process; Bare's `eval_load` also runs one `collect` at the end.
Results and method of 2026-11-18: docs/research-quickjs-reference.md.
"""
import argparse, hashlib, json, os, shutil, statistics, subprocess, sys, tempfile, urllib.request

QJS_URL = "https://bellard.org/quickjs/quickjs-2026-06-04.tar.xz"
QJS_SHA = "b376e839b322978313d929fd20663b11ba58b75df5a46c126dd19ea2fa70ad2a"
HERE = os.path.dirname(os.path.abspath(__file__))
TMP = os.environ.get("BARE_TMP") or ("/data/bare-tmp" if os.path.isdir("/data/bare-tmp") else os.path.expanduser("~/bare-tmp"))


def build_qjs():
    d = os.path.join(TMP, "quickjs-2026-06-04")
    qjs = os.path.join(d, "qjs")
    if os.path.exists(qjs):
        return qjs
    os.makedirs(TMP, exist_ok=True)
    tar = os.path.join(TMP, "quickjs-2026-06-04.tar.xz")
    if not os.path.exists(tar):
        urllib.request.urlretrieve(QJS_URL, tar)
    if hashlib.sha256(open(tar, "rb").read()).hexdigest() != QJS_SHA:
        sys.exit(f"{tar}: SHA-256 does not match")
    subprocess.run(["tar", "-xf", tar, "-C", TMP], check=True)
    subprocess.run(["make", "-j8", "qjs"], cwd=d, check=True, stdout=subprocess.DEVNULL)
    return qjs


def build_rss():
    out = os.path.join(TMP, "bin", "rss_peak")
    src = os.path.join(HERE, "rss_peak.c")
    if not os.path.exists(out) or os.path.getmtime(out) < os.path.getmtime(src):
        os.makedirs(os.path.dirname(out), exist_ok=True)
        subprocess.run(["cc", "-O2", "-o", out, src], check=True)
    return out


def run_perf(cmd, env, cwd):
    fd, pf = tempfile.mkstemp(dir=TMP)
    os.close(fd)
    p = subprocess.run(["perf", "stat", "-x,", "-e", "instructions:u", "-o", pf, "--"] + cmd, capture_output=True, env=env, cwd=cwd)
    n = next((int(l.split(",")[0]) for l in open(pf) if "instructions" in l), None)
    os.unlink(pf)
    return p, n


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("eval_load")
    ap.add_argument("--cases", default="loop,calls,fib,closures,objects,webpack,throws,concat")
    ap.add_argument("--scales", default="1,10")
    ap.add_argument("--runs", type=int, default=3)
    ap.add_argument("--engines", default="bare-tree,bare-auto,quickjs,node-jitless,node")
    ap.add_argument("--qjs")
    ap.add_argument("--out")
    a = ap.parse_args()
    engines = a.engines.split(",")
    node = shutil.which("node")
    if not node:
        engines = [e for e in engines if not e.startswith("node")]
    a.eval_load = os.path.abspath(a.eval_load)
    qjs = os.path.abspath(a.qjs or build_qjs()) if "quickjs" in engines else None
    rss = build_rss()
    work = tempfile.mkdtemp(dir=TMP)
    rows = []
    for c in a.cases.split(","):
        for s in a.scales.split(","):
            want, src = subprocess.run([a.eval_load, c, s, "--source"], capture_output=True, text=True, check=True).stdout.split("\n", 1)
            js = f"{c}_{s}.js"
            with open(os.path.join(work, js), "w") as f:
                f.write("console.log(String((0, eval)(" + json.dumps(src.rstrip("\n")) + ")));\n")
            for e in engines:
                env = dict(os.environ)
                if e.startswith("bare-"):
                    cmd, env["BARE_JS_EXEC"] = [a.eval_load, c, s], e[5:]
                elif e == "quickjs":
                    cmd = [qjs, js]
                else:
                    cmd = [node] + (["--jitless"] if e == "node-jitless" else []) + [js]
                ins, kib = [], []
                for _ in range(a.runs):
                    p, n = run_perf(cmd, env, work)
                    out = p.stdout.decode()
                    ok = ("checksum=ok" in out) if e.startswith("bare-") else out.strip() == want.strip()
                    if p.returncode != 0 or not ok or n is None:
                        print(f"FAILED: {e} {c} {s}: {(out + p.stderr.decode()).strip()[:200]}", file=sys.stderr)
                        break
                    ins.append(n)
                    r = subprocess.run([rss] + cmd, capture_output=True, text=True, env=env, cwd=work)
                    kib.append(int(r.stdout.strip() or 0))
                if len(ins) == a.runs:
                    rows.append((c, int(s), e, min(ins), (max(ins) - min(ins)) / min(ins) * 100, statistics.median(kib) / 1024))
    shutil.rmtree(work)
    base = {(c, s): i for c, s, e, i, _, _ in rows if e == "quickjs"}
    lines = ["case\tscale\tengine\tinstr_M\tspread_%\trss_MiB\tvs_quickjs"]
    for c, s, e, i, sp, m in rows:
        q = base.get((c, s))
        lines.append(f"{c}\t{s}\t{e}\t{i / 1e6:.1f}\t{sp:.2f}\t{m:.1f}\t{f'{i / q:.2f}x' if q else '-'}")
    text = "\n".join(lines) + "\n"
    if a.out:
        open(a.out, "w").write(text)
    sys.stdout.write(text)


if __name__ == "__main__":
    main()
