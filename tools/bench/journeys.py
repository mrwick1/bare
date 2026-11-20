"""The journey benchmark with recorded network inputs. Use it through `journeys.sh`.

  journeys.sh record  [--corpus DIR] [--clock real|virtual] [--only TEXT] [--tries N] BARE_DRIVE    record each public journey from the live sites, then fill what a fast replay asks for
                      (--only: update only these; N tries for the live recording, default 4; fill rounds stop at two runs in a row with no miss, at most 10)
  journeys.sh fill    [--corpus DIR] [--only TEXT] BARE_DRIVE    only the fill rounds, on a recording that exists (the live sites are asked for the misses only)
  journeys.sh run     [--corpus DIR] [--only TEXT] [--runs N] [--pss] [--out TSV] BARE_DRIVE    replay each journey N times (cold: a new process each time)
  journeys.sh compare [--corpus DIR] [--only TEXT] [--runs N] [--pss] [--out TSV] BASE_DRIVE NEW_DRIVE    replay on both binaries, runs interleaved
                      (exit 1 when a journey has more replay misses on NEW than on BASE: that journey did not do the recorded work, whatever its pass column says)
  journeys.sh session [--corpus DIR] [--only TEXT] [--rounds N] [--out TSV] BARE_DRIVE    long session: each journey N times (default 20) in ONE process (`bare-drive --repeat N`), memory after each round
  journeys.sh ready   [--corpus DIR] [--only TEXT] [--runs N] [--out TSV] BARE_DRIVE    time until the first step of each journey is done and the page can act (default 5 runs), CPU and memory of all processes then

`--clock virtual` sets `BARE_CLOCK=virtual` (a page clock that replies move, `crates/net/src/vclock.rs`): the same work in every run. It is a clock for a replay, so the recording of the
journeys in a virtual corpus is a fill under that clock (a copy of `main`, then `journeys.sh fill --clock virtual`); with it, `--corpus` defaults to `virtual-load` (the journey ends at the `load` event of its last document). `--clock real` (the default) removes `BARE_CLOCK`.

The corpus is machine-local and never in the repository: DIR/<journey>/ holds the replies of one journey (`crates/net/src/replay.rs`), DIR/status.tsv the
result of the recording. `BARE_SEED=1` is set for every run (a fixed `Math.random` and `crypto` stream: a random id in a URL or a body would never match a recording). A replay is faster than the live run, so a page asks for files that the live run never asked for before the process ended: after the
live recording, `record` replays with BARE_NET_FILL (a missing reply goes to the network and is recorded) until two runs in a row have no miss. A journey that did not pass
when it was recorded is not measured, and `run` says so. `run` counts the misses of each replay (a request with no recorded reply): a replay with a miss did not do the recorded work. Each run is one `perf stat -e instructions:u`
over one `bare-drive` process: columns journey, run, pass, instructions, cpu_ms (user+system), wall_ms, peak_rss_kib (`wait4`), pss_kib (peak of a sample every
20 ms of `smaps_rollup`, only with --pss: reading it costs time), misses. Instruction counts hold under load. Wall time holds only on an idle machine.

`session` (plan step M4): one `bare-drive --repeat N` process per journey, so every round starts a new page in the same process and what a round leaves behind shows in the next. The columns
are journey, round, ok, ms, rss_kib (VmRSS after the round), peak_rss_kib (VmHWM after the round), heap_kib (glibc in-use bytes, empty elsewhere). The slope is a least-squares line (RSS, peak and heap) over rounds 2 to N
(round 1 pays the one-time costs of the process), in KiB per round, for RSS, for the peak and for the glibc heap in use (what is live, without allocator slack); the session total is the sum over the journeys. A journey that fails stops its own rounds: the rounds that ran are
used. A replay serves the Nth reply of one URL in order and then the last again, so a round after the first sees the last reply of a URL that the recording fetched once: the rounds repeat the work of the recording, not the live traffic.

`ready` (plan step M5): the page of a journey can act when the first step after `open` is done (`open` ends at once, at about 0 ms; the next step waits for the page: a click or an `expect` that passes). `ready_ms` is the time
from the start of the process to the stderr line of that step (the read of the line and the sample of /proc are inside it: about 1 ms). At that moment the tool also reads the CPU time (the run time of every thread, from `schedstat`) and the RSS of the process and all its descendants, and counts them (`procs`).
`total_cpu_ms` is the CPU of the whole run (`wait4`: the process and the descendants that it waited for). `load1` is the load average before the run, and the tool prints it again at the end:
the wall time holds only when the machine is idle (load1 near 0). The spread is (max - min) over the median of the runs of one journey.
"""

import glob, json, os, re, shutil, statistics, subprocess, sys, tempfile, time

HERE = os.path.dirname(os.path.abspath(__file__))
STEPS = os.path.join(HERE, "..", "drive", "public")
CORPUS = "/data/bare-tmp/corpus/main"
VIRTUAL_CORPUS = "/data/bare-tmp/corpus/virtual-load"


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
    errlog, outlog = tempfile.TemporaryFile(), tempfile.TemporaryFile()
    p = subprocess.Popen(cmd, env={**os.environ, "BARE_SEED": "1", **env}, stdout=outlog, stderr=errlog)
    peak_pss = 0
    while True:
        if pss:
            peak_pss = max(peak_pss, pss_of(p.pid))
        done, status, usage = os.wait4(p.pid, os.WNOHANG if pss else 0)
        if done:
            break
        time.sleep(0.02)
    p.returncode = os.waitstatus_to_exitcode(status)
    outlog.seek(0)
    out = outlog.read()
    outlog.close()
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


def tree(pid):
    """The process and all its descendants."""
    return [pid] + [d for c in children(pid) for d in tree(c)]


def tree_use(pids):
    """(CPU ms, RSS KiB) summed over the processes `pids` now. The CPU is the run time of every thread (`schedstat`, in ns). A process that has gone counts as 0."""
    cpu = rss = 0
    for pid in pids:
        try:
            for t in os.listdir(f"/proc/{pid}/task"):
                try:
                    cpu += int(open(f"/proc/{pid}/task/{t}/schedstat").read().split()[0])
                except OSError:
                    pass
            rss += int(open(f"/proc/{pid}/statm").read().split()[1]) * os.sysconf("SC_PAGE_SIZE") // 1024
        except (OSError, ValueError, IndexError):
            pass
    return cpu / 1e6, rss


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
    opt = {"corpus": None, "only": "", "runs": 3, "tries": 4, "pss": False, "out": None, "clock": "real", "rounds": 20}
    while argv and argv[0].startswith("--"):
        k = argv.pop(0)[2:]
        if k not in flags:
            sys.exit(f"unknown option --{k}")
        opt[k] = True if k == "pss" else (type(opt[k])(argv.pop(0)) if opt[k] is not None else argv.pop(0))
    if opt["clock"] not in ("real", "virtual"):
        sys.exit("--clock is real or virtual")
    if opt["clock"] == "virtual":
        os.environ["BARE_CLOCK"] = "virtual"
    else:
        os.environ.pop("BARE_CLOCK", None)
    opt["corpus"] = opt["corpus"] or (VIRTUAL_CORPUS if opt["clock"] == "virtual" else CORPUS)
    return opt, argv


def fill(binary, name, path, url, d, rounds=10):
    """Replay with BARE_NET_FILL until two runs in a row have no miss (the requests of a page change a little from run to run). Gives whether the last run passed."""
    ok, quiet = True, 0
    for i in range(rounds):
        r = drive(binary, path, url, {"BARE_NET_FILL": d})
        ok, quiet = r[0], quiet + 1 if r[6] == 0 else 0
        print(f"{name:32} fill {i + 1}: {'pass' if ok else 'FAIL'} {r[6]} misses", flush=True)
        if quiet == 2 or not ok:
            break
    return ok


def record(opt, binary, fill_only=False):
    os.makedirs(opt["corpus"], exist_ok=True)
    rows = {}
    status = os.path.join(opt["corpus"], "status.tsv")
    if os.path.exists(status):
        rows = {l.split("\t")[0]: l for l in open(status).read().splitlines()[1:]}
    for name, path, url in journeys(opt["only"]):
        ok, d = fill_only, os.path.join(opt["corpus"], name)
        for _ in range(0 if fill_only else opt["tries"]):  # a flaky live site gets more tries (some sites answer a journey with another page now and then)
            shutil.rmtree(d, ignore_errors=True)
            ok = drive(binary, path, url, {"BARE_NET_RECORD": d})[0]
            if ok:
                break
        if ok:
            ok = fill(binary, name, path, url, d)
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
    """Rows (label, journey, run, pass, instructions, cpu_ms, wall_ms, rss, pss, misses). The binaries run one after the other for each run.
    With --pss, one more run of each binary (run number 0) samples the PSS: reading `smaps_rollup` takes the memory lock of the process and
    changes the timing of its threads, so the runs that count instructions never sample."""
    rows = []
    perf = tempfile.mkstemp(prefix="journeys-perf-")[1]
    for name, path, url in recorded(opt):
        for run in range(0 if opt["pss"] else 1, opt["runs"] + 1):
            for label, binary in binaries:
                r = drive(binary, path, url, {"BARE_NET_REPLAY": os.path.join(opt["corpus"], name)}, perf, run == 0)
                rows.append((label, name, run, int(r[0]), r[1], round(r[2]), round(r[3]), r[4], r[5] or "", r[6]))
                print(f"{label:6} {name:32} run {run} {'pass' if r[0] else 'FAIL'} {r[1] / 1e6:10.1f} Minstr {r[3]:7.0f} ms {r[4] // 1024:5} MiB" + (f"  {r[6]} MISSES" if r[6] else ""), flush=True)
    if os.path.exists(perf):
        os.remove(perf)
    return rows


def session_run(binary, path, url, env, rounds):
    """One process, `rounds` rounds of one journey. Gives (the `runs` records of bare-drive, one for each round that ran, the exit code)."""
    cmd = [binary, "--quiet", "--repeat", str(rounds), "--var", "URL=" + url, path]
    r = subprocess.run(cmd, env={**os.environ, "BARE_SEED": "1", **env}, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)
    try:
        return json.loads(r.stdout)["runs"], r.returncode
    except (ValueError, KeyError):
        return [], r.returncode


def slope(ys):
    """Least-squares slope of ys over 0, 1, 2, ... (rounds 2 to N, so the caller passes the values from round 2). n/a below 2 points."""
    return statistics.linear_regression(range(len(ys)), ys).slope if len(ys) >= 2 else None


def session(opt, binary):
    rows, tot = [], {"rss": 0.0, "peak": 0.0, "heap": 0.0}
    print(f"{'journey':32} {'rounds':>6} {'MiB r1':>8} {'MiB last':>9} {'peak r1':>8} {'peak last':>9} {'rss MiB/round':>14} {'peak MiB/round':>15} {'heap MiB/round':>15}  pass")
    for name, path, url in recorded(opt):
        runs, code = session_run(binary, path, url, {"BARE_NET_REPLAY": os.path.join(opt["corpus"], name)}, opt["rounds"])
        for i, r in enumerate(runs, 1):
            rows.append((name, i, int(r["ok"]), r["ms"], r["rss_kib"], r["peak_rss_kib"], "" if r["heap_kib"] is None else r["heap_kib"]))
        if not runs:
            print(f"{name:32} no result (exit {code})", flush=True)
            continue
        s = {k: slope([r[k + "_kib"] for r in runs[1:]]) for k in ("rss", "peak_rss", "heap")}
        if runs[0]["heap_kib"] is None:
            s["heap"] = None
        fmt = lambda v: "n/a" if v is None else f"{v / 1024:+.3f}"
        for k, t in (("rss", "rss"), ("peak_rss", "peak"), ("heap", "heap")):
            tot[t] += (s[k] or 0) / 1024
        print(f"{name:32} {len(runs):6} {runs[0]['rss_kib'] / 1024:8.1f} {runs[-1]['rss_kib'] / 1024:9.1f} {runs[0]['peak_rss_kib'] / 1024:8.1f} {runs[-1]['peak_rss_kib'] / 1024:9.1f} {fmt(s['rss']):>14} {fmt(s['peak_rss']):>15} {fmt(s['heap']):>15}  {'ok' if len(runs) == opt['rounds'] and code == 0 else 'stopped at round ' + str(len(runs))}", flush=True)
    print(f"{'SUM over the journeys':32} {'':6} {'':8} {'':9} {'':8} {'':9} {tot['rss']:+14.3f} {tot['peak']:+15.3f} {tot['heap']:+15.3f}  (MiB per round, rounds 2 to {opt['rounds']})")
    if opt["out"]:
        with open(opt["out"], "w") as f:
            f.write("journey\tround\tok\tms\trss_kib\tpeak_rss_kib\theap_kib\n")
            f.writelines("\t".join(map(str, r)) + "\n" for r in rows)


def ready_run(binary, path, url, env):
    """One cold run with the step lines on. Gives (ok, ready_ms, ready_cpu_ms, ready_rss_kib, procs, total_cpu_ms, total_wall_ms, misses)."""
    out = tempfile.TemporaryFile()
    start = time.monotonic()
    p = subprocess.Popen([binary, "--var", "URL=" + url, path], env={**os.environ, "BARE_SEED": "1", **env}, stdout=out, stderr=subprocess.PIPE)
    ready, misses, opened = None, 0, False
    for line in p.stderr:
        step = re.match(rb"L\d+ (\w+) ", line)
        if step and ready is None and opened:  # the first step after `open`: `open` ends at once, and the next step waits for the page
            at = time.monotonic()
            pids = tree(p.pid)
            ready = ((at - start) * 1000, *tree_use(pids), len(pids))
        opened = opened or bool(step and step[1] == b"open")
        misses += b"no recorded reply" in line
    p.stderr.close()
    _, status, usage = os.wait4(p.pid, 0)
    p.returncode = os.waitstatus_to_exitcode(status)
    wall = (time.monotonic() - start) * 1000
    out.seek(0)
    try:
        ok = json.loads(out.read())["ok"] and p.returncode == 0
    except ValueError:
        ok = False
    out.close()
    return (ok, *(ready or (0, 0, 0, 0)), round((usage.ru_utime + usage.ru_stime) * 1000), wall, misses)


def ready(opt, binary):
    rows = []
    print(f"load average before: {os.getloadavg()[0]:.2f} (1 min) on {os.cpu_count()} cores")
    print(f"{'journey':32} {'ready_ms':>9} {'spread':>7} {'cpu_ms':>7} {'spread':>7} {'rss MiB':>8} {'procs':>5} {'total_cpu_ms':>12}  pass")
    worst, tot = 0.0, {"ready": 0.0, "cpu": 0.0, "rss": 0.0}
    worst_cpu = 0.0
    for name, path, url in recorded(opt):
        rs = []
        for run in range(1, opt["runs"] + 1):
            load = os.getloadavg()[0]
            r = ready_run(binary, path, url, {"BARE_NET_REPLAY": os.path.join(opt["corpus"], name)})
            rs.append(r)
            rows.append((name, run, int(r[0]), round(r[1], 1), r[2], r[3], r[4], r[5], round(r[6]), r[7], f"{load:.2f}"))
        med = lambda i: statistics.median(r[i] for r in rs)
        sp = spread([r[1] for r in rs])
        worst = max(worst, sp)
        spc = spread([r[2] for r in rs])
        worst_cpu = max(worst_cpu, spc)
        tot["ready"] += med(1)
        tot["cpu"] += med(2)
        tot["rss"] += med(3) / 1024
        print(f"{name:32} {med(1):9.1f} {sp:6.1f}% {med(2):7.1f} {spc:6.1f}% {med(3) / 1024:8.1f} {max(r[4] for r in rs):5} {med(5):12.0f}  {'ok' if all(r[0] for r in rs) else 'FAIL'}" + (f"  {max(r[7] for r in rs)} MISSES at most" if any(r[7] for r in rs) else ""), flush=True)
    names = list(dict.fromkeys(r[0] for r in rows))
    over = [sum(1 for n in names if spread([r[i] for r in rows if r[0] == n]) >= 5) for i in (3, 4)]
    print(f"{'SUM of the medians':32} {tot['ready']:9.1f} {'max ' + format(worst, '.1f') + '%':>7} {tot['cpu']:7.1f} {'max ' + format(worst_cpu, '.1f') + '%':>7} {tot['rss']:8.1f}   of {len(names)} journeys, spread >= 5%: {over[0]} by ready_ms, {over[1]} by cpu_ms")
    print(f"load average after: {os.getloadavg()[0]:.2f} (1 min) on {os.cpu_count()} cores")
    if opt["out"]:
        with open(opt["out"], "w") as f:
            f.write("journey\trun\tpass\tready_ms\tready_cpu_ms\tready_rss_kib\tprocs\ttotal_cpu_ms\ttotal_wall_ms\tmisses\tload1\n")
            f.writelines("\t".join(map(str, r)) + "\n" for r in rows)


def write(rows, out):
    if out:
        with open(out, "w") as f:
            f.write("binary\tjourney\trun\tpass\tinstructions\tcpu_ms\twall_ms\tpeak_rss_kib\tpss_kib\tmisses\n")
            f.writelines("\t".join(map(str, r)) + "\n" for r in rows)


def spread(xs):
    m = statistics.median(xs)
    return (max(xs) - min(xs)) / m * 100 if m else 0


def compare(rows, a="base", b="new"):
    """Per journey and in total: the median of the runs of each binary, the change, and the spread (max - min over the median) of each.
    Gives False when a journey of `b` has more replay misses than the same journey of `a` (see `miss_report`)."""
    by, extra, miss = {}, {}, {}
    for label, name, run, ok, instr, cpu, wall, rss, pss, misses in rows:
        extra.setdefault(label, {}).setdefault(name, []).append((cpu, wall, pss))
        if run:  # run 0 reads the PSS: its timing gives misses that the other runs do not
            miss.setdefault(name, {}).setdefault(label, []).append(misses)
        if run:  # run 0 is the PSS run (see `measure`)
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


    print("\nSums of the medians over the journeys (wall time holds only on an idle machine; PSS only with --pss):")
    for k, name in enumerate(("cpu_ms", "wall_ms", "pss_MiB")):
        t = {}
        for label, d in extra.items():
            vals = [statistics.median(r[k] for r in v if r[k] != "") if any(r[k] != "" for r in v) else 0 for v in d.values()]
            t[label] = sum(vals) / (1024 if k == 2 else 1)
        print(f"  {name:8} {a} {t[a]:10.1f}  {b} {t[b]:10.1f}  {pct(t[a], t[b])}")
    return miss_report(miss, a, b)


def miss_report(miss, a, b):
    """A replay miss is a request with no recorded reply: the journey did not do the recorded work, and may still pass. Lists each journey that has
    misses (the most of runs 1 to N, for each binary). A journey fails when `b` has more than `a`: a miss that `a` also has (a beacon whose body
    changes between runs) is known and does not fail it. Gives False when a journey fails."""
    seen = {n: {k: max(v) for k, v in d.items() if v} for n, d in miss.items()}
    seen = {n: d for n, d in seen.items() if any(d.values())}
    bad = [n for n, d in seen.items() if d.get(b, 0) > d.get(a, 0)]
    print(f"\nReplay misses (most of the runs, per binary): {'none' if not seen else ''}")
    for n, d in seen.items():
        print(f"  {n:32} {a} {d.get(a, 0):4}  {b} {d.get(b, 0):4}  {'FAILED: more misses than ' + a if n in bad else 'same or fewer, not a failure'}")
    if bad:
        print(f"FAILED: {len(bad)} journey(s) have replay misses that {a} does not have: {', '.join(bad)}", file=sys.stderr)
    return not bad


def pct(a, b):
    return f"{(b - a) / a * 100:+.1f}%" if a else "n/a"


def main():
    if len(sys.argv) < 3:
        sys.exit(__doc__)
    mode, argv = sys.argv[1], sys.argv[2:]
    opt, argv = args(argv, ("corpus", "only", "runs", "tries", "pss", "out", "clock", "rounds"))
    if mode in ("record", "fill") and len(argv) == 1:
        record(opt, argv[0], mode == "fill")
    elif mode == "run" and len(argv) == 1:
        rows = measure(opt, [("bin", argv[0])])
        write(rows, opt["out"])
        for name in dict.fromkeys(r[1] for r in rows):
            xs = [r[4] for r in rows if r[1] == name]
            misses = max(r[9] for r in rows if r[1] == name)
            print(f"{name:32} median {statistics.median(xs) / 1e6:10.1f} Minstr  spread {spread(xs):5.1f}%" + (f"  {misses} MISSES at most" if misses else ""))
    elif mode == "session" and len(argv) == 1:
        session(opt, argv[0])
    elif mode == "ready" and len(argv) == 1:
        opt["runs"] = opt["runs"] if "--runs" in sys.argv else 5
        ready(opt, argv[0])
    elif mode == "compare" and len(argv) == 2:
        rows = measure(opt, [("base", argv[0]), ("new", argv[1])])
        write(rows, opt["out"])
        if not compare(rows):
            sys.exit(1)
    else:
        sys.exit(__doc__)


main()
