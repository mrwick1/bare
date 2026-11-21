"""The journey triage runner. Use it through `triage.sh`, which documents the options and the columns."""
import argparse
import concurrent.futures as cf
import json
import os
import re
import signal
import subprocess
import sys
import tempfile
import threading
import time

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(os.path.dirname(HERE))
PUBLIC = os.path.join(ROOT, "tools", "drive", "public")
STAGING_STEPS = os.path.join(ROOT, "tools", "drive", "staging-journey.steps")
TYPING_BUILD = os.path.join(ROOT, "tools", "js", "typing", "build.sh")
TYPING_DIR = os.environ.get("TRIAGE_TYPING_DIR", "/data/bare-tmp/triage/typing-page")


def first_line(text):
    for line in text.splitlines():
        if line.strip():
            return line.strip()[:300]
    return ""


def classify(text, rc, killed):
    """Return (class, cause) for the text of a failed run."""
    if killed:
        return "timeout", "timeout"
    low = text.lower()
    if "cannot run scripts yet" in low:
        return "not_ready", "not_ready"
    m = re.search(r'Unsupported\("([^"]+)"\)', text) or re.search(r"not supported yet:? ?(.*)", text)
    if m:
        name = (m.group(1) or "").strip().strip("[]\"'")[:60] or "feature"
        return "unsupported:" + name, "unsupported:" + name
    if rc is not None and rc < 0 or "panicked at" in text:
        sig = -rc if rc is not None and rc < 0 else 0
        return "crash", "crash:" + (signal.Signals(sig).name if sig else "panic")
    norm = re.sub(r"'[^']*'|\"[^\"]*\"|`[^`]*`|\d+", "", first_line(text))[:100].strip()
    if "syntaxerror" in low:
        return "syntax", "syntax:" + norm
    if "typeerror" in low:
        return "type_error", "type_error:" + norm
    if "timed out" in low or "timeout" in low:
        return "timeout", "timeout"
    return "other", "other:" + norm


def run(cmd, env, timeout):
    """Run cmd; return (rc, wall_ms, peak_rss_kib, stdout, stderr, killed). The rusage is the child's and its waited descendants."""
    with tempfile.TemporaryFile("w+") as out, tempfile.TemporaryFile("w+") as err:
        t0 = time.time()
        p = subprocess.Popen(cmd, env=env, stdout=out, stderr=err, stdin=subprocess.DEVNULL, start_new_session=True)
        killed = []

        def kill():
            killed.append(1)
            try:
                os.killpg(p.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass

        timer = threading.Timer(timeout, kill)
        timer.start()
        _, status, ru = os.wait4(p.pid, 0)
        timer.cancel()
        p.returncode = 0  # reaped by wait4
        wall = (time.time() - t0) * 1000
        rc = os.waitstatus_to_exitcode(status)
        out.seek(0)
        err.seek(0)
        return rc, wall, ru.ru_maxrss, out.read(), err.read(), bool(killed)


def drive_result(path, stdout):
    """(ok, message) from the --out JSON or stdout of bare-drive."""
    try:
        j = json.loads(stdout)
    except ValueError:
        return None, ""
    msg = (j.get("failure") or {}).get("message", "")
    if not msg and not j.get("ok"):
        msg = first_line(json.dumps(j.get("console", [])))
    return bool(j.get("ok")), msg


def workloads(args, bindir):
    rows = []
    drive = os.path.join(bindir, "bare-drive")
    for steps in sorted(os.listdir(PUBLIC)):
        if not steps.endswith(".steps"):
            continue
        path = os.path.join(PUBLIC, steps)
        m = re.search(r"--var URL=(\S+)", open(path).read())
        rows.append(("public:" + steps[:-6], "drive", [drive, "--quiet", "--var", "URL=" + (m.group(1) if m else ""), path], None))
    rows.append(("typing", "typing", [os.path.join(bindir, "examples", "type_load"), TYPING_DIR, "#a1", "50"], None))
    base, query, secrets = (os.environ.get(k) for k in ("STAGING_BASE", "STAGING_QUERY", "STAGING_SECRETS"))
    if base and query and secrets and os.path.isfile(secrets):
        rows.append(("staging", "drive", [drive, "--quiet", "--secrets", secrets, "--var", "BASE=" + base, "--var", "QUERY=" + query, STAGING_STEPS], None))
    else:
        rows.append(("staging", "drive", None, "set STAGING_BASE, STAGING_QUERY and STAGING_SECRETS (a file with EMAIL= and PASSWORD=) to run the staging journey"))
    return [r for r in rows if re.search(args.grep, r[0])] if args.grep else rows


def once(kind, cmd, env, timeout):
    rc, wall, rss, out, err, killed = run(cmd, env, timeout)
    if kind == "drive":
        ok, msg = drive_result(None, out)
        if ok is None:
            ok, msg = False, first_line(err)
        text = msg + "\n" + err
    else:
        ok = rc == 0 and " value 50 chars" in out
        text = err + "\n" + out
        msg = first_line(err) or first_line(out)
    if ok and rc == 0:
        return True, "", "", wall, rss, rc, ""
    cls, cause = classify(text if not killed else "", rc, killed)
    return False, cls, cause, wall, rss, rc, (msg or first_line(text) or "exit %s" % rc)


def one(w, env, timeout):
    name, kind, cmd, skip = w
    if skip:
        return name, ("skip", "", "", 0, 0, "", skip)
    res = once(kind, cmd, env, timeout)
    if not res[0] and not (res[1] == "not_ready" or res[1].startswith("unsupported")) and kind == "drive" and name != "staging":
        res = once(kind, cmd, env, timeout)  # a network flake
    ok, cls, cause, wall, rss, rc, msg = res
    return name, ("pass" if ok else "fail", cls, cause, round(wall), rss, rc, msg.replace("\t", " ").replace("\n", " "))


def main():
    ap = argparse.ArgumentParser(add_help=False)
    ap.add_argument("-e", dest="engine", default="auto")
    ap.add_argument("-j", dest="jobs", type=int, default=1)
    ap.add_argument("-o", dest="out", default="triage.tsv")
    ap.add_argument("-g", dest="grep")
    ap.add_argument("-b", dest="bin", default=os.path.join(os.environ.get("CARGO_TARGET_DIR", "target"), "release"))
    ap.add_argument("-T", dest="timeout", type=float, default=300)
    ap.add_argument("-h", action="store_true")
    args = ap.parse_args()
    if args.h:
        print(open(os.path.join(HERE, "triage.sh")).read().split("exec python3")[0])
        return 0
    bindir = os.path.abspath(args.bin)
    for need in ("bare-drive", os.path.join("examples", "type_load")):
        if not os.access(os.path.join(bindir, need), os.X_OK):
            sys.exit("missing %s/%s: cargo build --release -p bare-drive --bin bare-drive --example type_load -p bare (or pass -b DIR)" % (bindir, need))
    rows = workloads(args, bindir)
    if any(r[0] == "typing" for r in rows) and not os.path.isfile(os.path.join(TYPING_DIR, "index.html")):
        sys.stderr.write("building the typing page into %s\n" % TYPING_DIR)
        if subprocess.run([TYPING_BUILD, TYPING_DIR, "60"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL).returncode:
            rows = [(n, k, None, "the typing page could not be built (tools/js/typing/build.sh needs npm)") if n == "typing" else (n, k, c, s) for n, k, c, s in rows]
    env = dict(os.environ, BARE_JS_EXEC=args.engine)
    results = {}
    with cf.ThreadPoolExecutor(max(1, args.jobs)) as pool:
        for name, r in pool.map(lambda w: one(w, env, args.timeout), rows):
            results[name] = r
            print("%-8s %-30s %-24s %7s ms %8s KiB  %s" % (r[0], name, r[1], r[3], r[4], r[6][:100]), flush=True)
    with open(args.out, "w") as f:
        f.write("workload\tstatus\tclass\twall_ms\tpeak_rss_kib\texit\terror\n")
        for name, r in results.items():
            f.write("\t".join(str(x) for x in (name, r[0], r[1], r[3], r[4], r[5], r[6])) + "\n")
    queue = {}
    for name, r in results.items():
        if r[0] == "fail":
            queue.setdefault(r[2], []).append(name)
    qpath = re.sub(r"(\.tsv)?$", ".queue.tsv", args.out, count=1)
    with open(qpath, "w") as f:
        f.write("cause\tblocks\tworkloads\n")
        for cause, names in sorted(queue.items(), key=lambda kv: (-len(kv[1]), kv[0])):
            f.write("%s\t%d\t%s\n" % (cause, len(names), ",".join(names)))
    c = lambda s: sum(r[0] == s for r in results.values())
    print("engine %s  pass %d  fail %d  skip %d  causes %d  -> %s, %s" % (args.engine, c("pass"), c("fail"), c("skip"), len(queue), args.out, qpath))
    return 1 if c("fail") else 0


sys.exit(main())
