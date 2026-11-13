"""The journey benchmark with recorded network inputs. Use it through `journeys.sh`.

  journeys.sh record  [--corpus DIR] [--only TEXT] [--tries N] BARE_DRIVE    record each public journey from the live sites, then fill what a fast replay asks for
                      (--only: update only these; N tries for the live recording, default 4; fill rounds stop at a run with no miss, at most 6)
  journeys.sh run     [--corpus DIR] [--only TEXT] [--runs N] [--pss] [--out TSV] BARE_DRIVE    replay each journey N times (cold: a new process each time)
  journeys.sh compare [--corpus DIR] [--only TEXT] [--runs N] [--pss] [--out TSV] BASE_DRIVE NEW_DRIVE    replay on both binaries, runs interleaved

The corpus is machine-local and never in the repository: DIR/<journey>/ holds the replies of one journey (`crates/net/src/replay.rs`), DIR/status.tsv the
result of the recording. A replay is faster than the live run, so a page asks for files that the live run never asked for before the process ended: after the
live recording, `record` replays with BARE_NET_FILL (a missing reply goes to the network and is recorded) until a run has no miss. A journey that did not pass
when it was recorded is not measured, and `run` says so. `run` counts the misses of each replay (a request with no recorded reply): a replay with a miss did not do the recorded work. Each run is one `perf stat -e instructions:u`
over one `bare-drive` process: columns journey, run, pass, instructions, cpu_ms (user+system), wall_ms, peak_rss_kib (`wait4`), pss_kib (peak of a sample every
20 ms of `smaps_rollup`, only with --pss: reading it costs time), misses. Instruction counts hold under load. Wall time holds only on an idle machine.
"""

import glob, json, os, re, shutil, statistics, subprocess, sys, tempfile, time

HERE = os.path.dirname(os.path.abspath(__file__))
STEPS = os.path.join(HERE, "..", "drive", "public")
CORPUS = "/data/bare-tmp/corpus/main"


def journeys(only):
    out = []
    for path in sorted(glob.glob(os.path.join(STEPS, "*.steps"))):
        name = os.path.basename(path)[:-6]
        m = re.search(r"--var URL=(\S+)", open(path).read())
        if m and only in name:
            out.append((name, path, m.group(1)))
    return out


def drive(binary, path, url, env, perf=None, pss=False):
    """One cold run. Gives (passed, instructions, cpu_ms, wall_ms, peak_rss_kib, pss_kib, misses)."""
    cmd = [binary, "--quiet", "--var", "URL=" + url, path]
    if perf:
        cmd = ["perf", "stat", "-x,", "-e", "instructions:u", "-o", perf] + cmd
    start = time.monotonic()
    errlog = tempfile.TemporaryFile()
    p = subprocess.Popen(cmd, env={**os.environ, **env}, stdout=subprocess.PIPE, stderr=errlog)
    assert p.stdout
    peak_pss = 0
    if pss:
        while p.poll() is None:
            peak_pss = max(peak_pss, pss_of(p.pid))
            time.sleep(0.02)
    out = p.stdout.read()
    _, status, usage = os.wait4(p.pid, 0)
    p.returncode = os.waitstatus_to_exitcode(status)
    wall = (time.monotonic() - start) * 1000
    try:
        ok = json.loads(out)["ok"] and p.returncode == 0
    except ValueError:
        ok = False
    instr = 0
    if perf:
        for line in open(perf):
            f = line.split(",")
            if len(f) > 2 and "instructions" in f[2] and f[0].isdigit():
                instr = int(f[0])
    errlog.seek(0)
    misses = errlog.read().count(b"no recorded reply")
    errlog.close()
    return ok, instr, (usage.ru_utime + usage.ru_stime) * 1000, wall, usage.ru_maxrss, peak_pss, misses


def pss_of(pid):
    """Peak PSS of the benchmark process: the one that `perf` started (a child of `perf`), or the process itself."""
    best = 0
    for d in [pid] + children(pid):
        try:
            for line in open(f"/proc/{d}/smaps_rollup"):
                if line.startswith("Pss:"):
                    best = max(best, int(line.split()[1]))
        except OSError:
            pass
    return best


def children(pid):
    try:
        return [int(c) for t in os.listdir(f"/proc/{pid}/task") for c in open(f"/proc/{pid}/task/{t}/children").read().split()]
    except OSError:
        return []


def args(argv, flags):
    opt = {"corpus": CORPUS, "only": "", "runs": 3, "tries": 4, "pss": False, "out": None}
    while argv and argv[0].startswith("--"):
        k = argv.pop(0)[2:]
        if k not in flags:
            sys.exit(f"unknown option --{k}")
        opt[k] = True if k == "pss" else (type(opt[k])(argv.pop(0)) if opt[k] is not None else argv.pop(0))
    return opt, argv


def record(opt, binary):
    os.makedirs(opt["corpus"], exist_ok=True)
    rows = {}
    status = os.path.join(opt["corpus"], "status.tsv")
    if os.path.exists(status):
        rows = {l.split("\t")[0]: l for l in open(status).read().splitlines()[1:]}
    for name, path, url in journeys(opt["only"]):
        ok, d = False, os.path.join(opt["corpus"], name)
        for _ in range(opt["tries"]):  # a flaky live site gets more tries (some sites answer a journey with another page now and then)
            shutil.rmtree(d, ignore_errors=True)
            ok = drive(binary, path, url, {"BARE_NET_RECORD": d})[0]
            if ok:
                break
        for round_ in range(6 if ok else 0):
            r = drive(binary, path, url, {"BARE_NET_FILL": d})
            ok = r[0]
            print(f"{name:32} fill {round_ + 1}: {'pass' if ok else 'FAIL'} {r[6]} misses", flush=True)
            if r[6] == 0 or not ok:
                break
        files = len(os.listdir(d)) if os.path.isdir(d) else 0
        size = sum(os.path.getsize(os.path.join(d, f)) for f in os.listdir(d)) if files else 0
        print(f"{name:32} {'pass' if ok else 'FAIL'} {files:4} replies {size // 1024:7} KiB", flush=True)
        rows[name] = f"{name}\t{int(ok)}\t{files}\t{size}"
    with open(status, "w") as f:
        f.write("journey\tlive_pass\treplies\tbytes\n" + "\n".join(rows[k] for k in sorted(rows)) + "\n")


def recorded(opt):
    """The journeys that have a recording that passed live."""
    status = os.path.join(opt["corpus"], "status.tsv")
    if not os.path.exists(status):
        sys.exit(f"no recording in {opt['corpus']}: run `journeys.sh record` first")
    good = {l.split("\t")[0] for l in open(status).read().splitlines()[1:] if l.split("\t")[1] == "1"}
    todo = [j for j in journeys(opt["only"]) if j[0] in good]
    for name, _, _ in journeys(opt["only"]):
        if name not in good:
            print(f"skipped {name}: it did not pass when it was recorded", file=sys.stderr)
    return todo


def measure(opt, binaries):
    """Rows (label, journey, run, pass, instructions, cpu_ms, wall_ms, rss, pss, misses). The binaries run one after the other for each run."""
    rows = []
    perf = tempfile.mkstemp(prefix="journeys-perf-")[1]
    for name, path, url in recorded(opt):
        for run in range(1, opt["runs"] + 1):
            for label, binary in binaries:
                r = drive(binary, path, url, {"BARE_NET_REPLAY": os.path.join(opt["corpus"], name)}, perf, opt["pss"])
                rows.append((label, name, run, int(r[0]), r[1], round(r[2]), round(r[3]), r[4], r[5] or "", r[6]))
                print(f"{label:6} {name:32} run {run} {'pass' if r[0] else 'FAIL'} {r[1] / 1e6:10.1f} Minstr {r[3]:7.0f} ms {r[4] // 1024:5} MiB" + (f"  {r[6]} MISSES" if r[6] else ""), flush=True)
    if os.path.exists(perf):
        os.remove(perf)
    return rows


def write(rows, out):
    if out:
        with open(out, "w") as f:
            f.write("binary\tjourney\trun\tpass\tinstructions\tcpu_ms\twall_ms\tpeak_rss_kib\tpss_kib\tmisses\n")
            f.writelines("\t".join(map(str, r)) + "\n" for r in rows)


def spread(xs):
    m = statistics.median(xs)
    return (max(xs) - min(xs)) / m * 100 if m else 0


def compare(rows, a="base", b="new"):
    """Per journey and in total: the median of the runs of each binary, the change, and the spread (max - min over the median) of each."""
    by = {}
    for label, name, _, ok, instr, _cpu, wall, rss, pss, _misses in rows:
        by.setdefault(name, {}).setdefault(label, []).append((ok, instr, wall, rss, pss))
    print(f"\n{'journey':32} {'Minstr ' + a:>12} {'Minstr ' + b:>12} {'change':>8} {'spread ' + a:>9} {'spread ' + b:>9} {'MiB ' + a:>8} {'MiB ' + b:>8} {'change':>8}  pass")
    tot = {a: 0, b: 0}
    rss_tot = {a: 0, b: 0}
    worst = {a: 0, b: 0}
    for name, d in by.items():
        if a not in d or b not in d:
            continue
        m = {k: statistics.median(r[1] for r in d[k]) for k in (a, b)}
        rs = {k: statistics.median(r[3] for r in d[k]) / 1024 for k in (a, b)}
        sp = {k: spread([r[1] for r in d[k]]) for k in (a, b)}
        ok = {k: all(r[0] for r in d[k]) for k in (a, b)}
        for k in (a, b):
            tot[k] += m[k]
            rss_tot[k] += rs[k]
            worst[k] = max(worst[k], sp[k])
        print(f"{name:32} {m[a] / 1e6:12.1f} {m[b] / 1e6:12.1f} {pct(m[a], m[b]):>8} {sp[a]:8.1f}% {sp[b]:8.1f}% {rs[a]:8.1f} {rs[b]:8.1f} {pct(rs[a], rs[b]):>8}  {'ok' if ok[a] and ok[b] else a + '=' + str(ok[a]) + ' ' + b + '=' + str(ok[b])}")
    print(f"{'TOTAL (sum of medians)':32} {tot[a] / 1e6:12.1f} {tot[b] / 1e6:12.1f} {pct(tot[a], tot[b]):>8} {'max ' + format(worst[a], '.1f') + '%':>9} {'max ' + format(worst[b], '.1f') + '%':>9} {rss_tot[a]:8.1f} {rss_tot[b]:8.1f} {pct(rss_tot[a], rss_tot[b]):>8}")


def pct(a, b):
    return f"{(b - a) / a * 100:+.1f}%" if a else "n/a"


def main():
    if len(sys.argv) < 3:
        sys.exit(__doc__)
    mode, argv = sys.argv[1], sys.argv[2:]
    opt, argv = args(argv, ("corpus", "only", "runs", "tries", "pss", "out"))
    if mode == "record" and len(argv) == 1:
        record(opt, argv[0])
    elif mode == "run" and len(argv) == 1:
        rows = measure(opt, [("bin", argv[0])])
        write(rows, opt["out"])
        for name in dict.fromkeys(r[1] for r in rows):
            xs = [r[4] for r in rows if r[1] == name]
            print(f"{name:32} median {statistics.median(xs) / 1e6:10.1f} Minstr  spread {spread(xs):5.1f}%")
    elif mode == "compare" and len(argv) == 2:
        rows = measure(opt, [("base", argv[0]), ("new", argv[1])])
        write(rows, opt["out"])
        compare(rows)
    else:
        sys.exit(__doc__)


main()
