#!/usr/bin/env python3
"""Reproducible browser task evaluation for `bare-mcp`. Python stdlib only; no model client, no credential lookup, no paid run.

Commands (details: docs/agent-evaluation.md):
  list                              the tasks and their prompts
  smoke  --bare-mcp BIN --out DIR   run scripted tool sequences through real stdio MCP and grade them. A SCRIPTED run is a protocol and grader check, not an agent result.
  fixture --task T --out DIR        start the loopback fixture for an external agent run, write the prompt and inputs, stop at the deadline or on SIGINT/SIGTERM
                                    and write the fixture record (state.json)
  score  --task T --transcript F --state F --out DIR    grade an externally supplied transcript against the fixture record
  report DIR                        aggregate the result.json files under DIR into summary.json and print it

Exit status: 0 all requested tasks passed, 1 a task failed, 2 bad usage or an invalid input.
"""

import argparse
import json
import os
import platform
import secrets
import signal
import sys
import threading
import time

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)

import fixture as fx  # noqa: E402
import tasks as tk  # noqa: E402
import transcript as tr  # noqa: E402

SCHEMA = 1


def write_new(path, obj):
    """Write JSON to a file that must not exist yet."""
    with open(path, "x", encoding="utf-8") as f:
        json.dump(obj, f, indent=2, sort_keys=True)
        f.write("\n")


def run_id(task, seed):
    return f"{task}-s{seed}-{time.strftime('%Y%m%dT%H%M%S', time.gmtime())}-{secrets.token_hex(2)}"


def git_head():
    import subprocess

    try:
        return subprocess.run(["git", "-C", os.path.dirname(HERE), "rev-parse", "HEAD"], capture_output=True, text=True, timeout=10).stdout.strip() or None
    except (OSError, subprocess.SubprocessError):
        return None


def manifest(task, seed, rid, kind, args, binary=None, base=None):
    m = {
        "v": SCHEMA,
        "run_id": rid,
        "task": {"id": task.id, "version": tk.VERSION, "title": task.title},
        "fixture": {"seed": seed, "base_url": base, "note": "loopback port differs per run; the data depends on the seed only"},
        "browser": None,
        "runner": {"kind": kind},
        "viewport": {"width": args.width, "height": args.height},
        "timeouts": {"call_s": getattr(args, "call_timeout", None), "run_deadline_s": args.deadline},
        "machine": {"platform": platform.platform(), "cpus": os.cpu_count(), "python": platform.python_version()},
        "started_utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
    }
    if binary:
        sha = getattr(args, "browser_sha", None) or git_head()
        m["browser"] = {
            "binary": os.path.abspath(binary), "binary_sha256": tr.file_sha256(binary), "binary_bytes": os.path.getsize(binary),
            "git_sha": sha, "git_sha_source": "--browser-sha" if getattr(args, "browser_sha", None) else "HEAD of the checkout that holds this script (not proven to match the binary)",
        }
    if kind == "scripted":
        m["runner"].update(name="scripted sequence", model=None, note="a fixed tool call list; not a model agent")
    else:
        keys = ("runner_name", "model", "prompt_file", "tool_setup")
        r = {k: getattr(args, k, None) for k in keys}
        if r["prompt_file"]:
            r["prompt_sha256"] = tr.file_sha256(r["prompt_file"])
            r["prompt_file"] = os.path.basename(r["prompt_file"])
        r["recorded"] = all(r.get(k) for k in ("runner_name", "model", "tool_setup")) and "prompt_sha256" in r
        r["note"] = "complete" if r["recorded"] else "incomplete: a real-agent result needs runner, model, prompt and tool setup; it stays unverified"
        m["runner"].update(r)
    return m


def tokens_record(path):
    if not path:
        return {"available": False, "reason": "no token usage supplied by the caller or the harness; never estimated from bytes"}
    with open(path) as f:
        d = json.load(f)
    bad = [k for k in ("input_tokens", "output_tokens") if not isinstance(d.get(k), int) or isinstance(d.get(k), bool) or d[k] < 0]
    if bad:
        raise ValueError(f"--tokens-json needs non-negative integers: {bad}")
    return {"available": True, "input_tokens": d["input_tokens"], "output_tokens": d["output_tokens"], "source": "supplied by the caller"}


def grade(task, ctx, events, problems):
    """Run the grader and the transcript checks. Gives (checks, duplicates)."""
    checks = [tk.check("transcript_valid", not problems, "; ".join(problems[:5]) + (f" (+{len(problems) - 5} more)" if len(problems) > 5 else ""))]
    if task.needs_answer and not (ctx.answer or "").strip():
        checks.append(tk.check("final_answer_present", False, "this task needs the agent's final answer in the transcript"))
    checks += task.grade(ctx)
    dups = task.duplicates(ctx)
    for k, n in dups.items():
        checks.append(tk.check(f"no_repeat_{k}", n == 0, f"{n} repeated {k} action(s)"))
    return checks, dups


def result_of(task, rid, kind, checks, dups, events, problems, res, tokens, state):
    failed = [c for c in checks if not c["ok"]]
    return {
        "v": SCHEMA, "run_id": rid, "task": task.id, "task_version": tk.VERSION, "runner_kind": kind,
        "completed": not failed, "checks": checks, "failed_assertions": len(failed),
        "duplicate_actions": dups, "duplicate_submissions": sum(dups.values()),
        "transcript_problems": problems, "metrics": tr.metrics(events), "resources": res, "tokens": tokens,
        "fixture_requests": state.get("requests"),
    }


# --- smoke ---


def perf_instructions(path):
    try:
        for line in open(path):
            f = line.split(",")
            if len(f) > 2 and f[2].startswith("instructions") and f[0].isdigit():
                return int(f[0]), None
        return None, "perf stat gave no instruction count (counter unsupported or not permitted)"
    except OSError:
        return None, "perf stat wrote no output"


def smoke_task(task, seed, args, out):
    tdir = os.path.join(out, task.id)
    os.makedirs(tdir)  # fails when it exists: no silent overwrite
    rid = run_id(task.id, seed)
    f = fx.Fixture(seed)
    start = time.monotonic()
    deadline = start + args.deadline
    ctx = tk.Ctx(world=f.world, base=f.base, state={}, downloads_dir=os.path.join(tdir, "downloads"), inputs_dir=os.path.join(tdir, "inputs"))
    os.makedirs(ctx.downloads_dir)
    os.makedirs(ctx.inputs_dir)
    ctx.upload_path = os.path.join(ctx.inputs_dir, f.world["upload_name"])
    with open(ctx.upload_path, "wb") as fh:
        fh.write(f.world["upload"])
    man = manifest(task, seed, rid, "scripted", args, args.bare_mcp, f.base)
    write_new(os.path.join(tdir, "manifest.json"), man)
    rec = tr.Recorder(os.path.join(tdir, "transcript.jsonl"), tr.Redactor(f.secrets))
    home = os.path.join(tdir, "home")
    os.makedirs(home)
    argv = [os.path.abspath(args.bare_mcp)]
    perf_out = None
    if args.perf:
        perf_out = os.path.join(tdir, "perf.txt")
        argv = ["perf", "stat", "-x,", "-e", "instructions:u", "-o", perf_out] + argv
    env = {"PATH": os.environ.get("PATH", ""), "HOME": home, "BARE_SEED": "1"}
    client = tr.McpClient(argv, rec, env=env, deadline=deadline)
    runner_error = None

    def call(tool, **a):
        if tool in ("open", "ensure_session"):
            a.setdefault("width", args.width)
            a.setdefault("height", args.height)
            a.setdefault("timeout_ms", args.call_timeout * 1000)
        return client.call(tool, timeout=args.call_timeout, **a)

    try:
        client.initialize()
        task.script(ctx, call)
    except tr.DeadlineError as e:
        runner_error = f"deadline: {e}"
    except Exception as e:  # keep the partial transcript of any runner failure
        runner_error = f"runner error: {type(e).__name__}: {e}"
    if runner_error:
        rec.add("note", text=runner_error)
    if ctx.answer is not None:
        rec.add("final_answer", text=ctx.answer)
    client.stop()
    perf_n, perf_why = perf_instructions(perf_out) if perf_out else (None, "not requested (--perf)")
    res = tr.resources(client.usage, client.status, client.hwm_kib, perf_n, perf_why)
    rec.close()
    ctx.state = f.state()
    events, problems = tr.load(os.path.join(tdir, "transcript.jsonl"))
    problems = problems + tr.validate(events)
    write_new(os.path.join(tdir, "state.json"), ctx.state)
    checks, dups = grade(task, ctx, events, problems)
    if runner_error:
        checks.append(tk.check("run_finished", False, runner_error))
    f.stop()
    result = result_of(task, rid, "scripted", checks, dups, events, problems, res, tokens_record(None), ctx.state)
    result["tokens"]["reason"] = "scripted run: no model"
    result["metrics"]["elapsed_s"] = round(time.monotonic() - start, 3) if result["metrics"]["elapsed_s"] is None else result["metrics"]["elapsed_s"]
    write_new(os.path.join(tdir, "result.json"), result)
    return result


def cmd_smoke(args):
    if not os.access(args.bare_mcp, os.X_OK):
        return die(f"--bare-mcp {args.bare_mcp!r} is not an executable file")
    ids = args.task or list(tk.TASKS)
    if err := unknown(ids):
        return die(err)
    out = args.out
    if os.path.exists(out) and os.listdir(out):
        return die(f"{out} exists and is not empty; give a new --out directory")
    os.makedirs(out, exist_ok=True)
    results = [smoke_task(tk.TASKS[i], args.seed, args, out) for i in ids]
    return finish(out, results)


# --- fixture + score for an external agent ---


def cmd_fixture(args):
    task = tk.TASKS.get(args.task)
    if task is None:
        return die(unknown([args.task]))
    tdir = os.path.join(args.out, task.id)
    if os.path.exists(tdir) and os.listdir(tdir):
        return die(f"{tdir} exists and is not empty; give a new --out directory")
    for d in ("inputs", "downloads"):
        os.makedirs(os.path.join(tdir, d))
    f = fx.Fixture(args.seed)
    ctx = tk.Ctx(world=f.world, base=f.base, state={}, downloads_dir=os.path.abspath(os.path.join(tdir, "downloads")), inputs_dir=os.path.abspath(os.path.join(tdir, "inputs")))
    ctx.upload_path = os.path.join(ctx.inputs_dir, f.world["upload_name"])
    with open(ctx.upload_path, "wb") as fh:
        fh.write(f.world["upload"])
    rid = run_id(task.id, args.seed)
    write_new(os.path.join(tdir, "manifest.json"), manifest(task, args.seed, rid, "external", args, args.bare_mcp, f.base))
    prompt = task.prompt(ctx)
    with open(os.path.join(tdir, "prompt.txt"), "x") as fh:
        fh.write(prompt + "\n")
    info = {"task": task.id, "base_url": f.base, "prompt": prompt, "downloads_dir": ctx.downloads_dir, "task_dir": os.path.abspath(tdir), "sessions": list(task.sessions)}
    print(json.dumps(info), flush=True)
    stop = threading.Event()
    for s in (signal.SIGINT, signal.SIGTERM):
        signal.signal(s, lambda *_: stop.set())
    stop.wait(args.deadline)
    write_new(os.path.join(tdir, "state.json"), f.state())
    f.stop()
    print(f"fixture record written to {os.path.join(tdir, 'state.json')}", file=sys.stderr)
    return 0


def cmd_score(args):
    task = tk.TASKS.get(args.task)
    if task is None:
        return die(unknown([args.task]))
    try:
        state = json.load(open(args.state))
        events, problems = tr.load(args.transcript)
        tokens = tokens_record(args.tokens_json)
    except (OSError, ValueError) as e:
        return die(f"cannot read an input: {e}")
    if "seed" not in state:
        return die("the fixture record has no seed")
    problems = problems + tr.validate(events)
    world = fx.make_world(state["seed"])
    answer = open(args.answer_file).read() if args.answer_file else next((e["text"] for e in reversed(events) if e.get("type") == "final_answer" and isinstance(e.get("text"), str)), None)
    ctx = tk.Ctx(world=world, base="", state=state, answer=answer, downloads_dir=args.downloads_dir)
    checks, dups = grade(task, ctx, events, problems)
    rid = run_id(task.id, state["seed"])
    tdir = os.path.join(args.out, task.id)
    os.makedirs(tdir, exist_ok=True)
    man = manifest(task, state["seed"], rid, "external", args, args.bare_mcp)
    res = {"scope": "not collected by this command", "cpu_ms": None, "peak_rss_kib": None, "instructions": None, "reason": "an external run: the runner of the agent owns the process; supply its own measurements outside this record"}
    result = result_of(task, rid, "external", checks, dups, events, problems, res, tokens, state)
    result["agent_recorded"] = man["runner"]["recorded"]
    if not man["runner"]["recorded"]:
        result["note"] = man["runner"]["note"]
    write_new(os.path.join(tdir, "score-manifest.json"), man)
    write_new(os.path.join(tdir, "result.json"), result)
    return finish(args.out, [result])


# --- reporting ---


def summarize(results):
    def total(fn):
        vals = [fn(r) for r in results]
        return None if any(v is None for v in vals) else sum(vals)

    cpu = total(lambda r: r["resources"]["cpu_ms"])
    rss = [r["resources"]["peak_rss_kib"] for r in results]
    tok = [r["tokens"] for r in results]
    return {
        "v": SCHEMA, "task_version": tk.VERSION, "runner_kinds": sorted({r["runner_kind"] for r in results}),
        "tasks": len(results), "completed": sum(r["completed"] for r in results), "failed": [r["task"] for r in results if not r["completed"]],
        "failed_assertions": sum(r["failed_assertions"] for r in results), "duplicate_submissions": sum(r["duplicate_submissions"] for r in results),
        "tool_calls": sum(r["metrics"]["tool_calls"] for r in results), "response_bytes": sum(r["metrics"]["response_bytes"] for r in results),
        "elapsed_s": round(sum(r["metrics"]["elapsed_s"] or 0 for r in results), 3),
        "cpu_ms": cpu, "cpu_ms_note": None if cpu is not None else "null: a task has no CPU measurement (see its resources.reason)",
        "peak_rss_kib_max": None if any(v is None for v in rss) else max(rss),
        "resource_scope": "sum or max over per-task processes; each task has its own bare-mcp process",
        "tokens": {"available": all(t["available"] for t in tok), "input_tokens": sum(t.get("input_tokens", 0) for t in tok) if all(t["available"] for t in tok) else None, "output_tokens": sum(t.get("output_tokens", 0) for t in tok) if all(t["available"] for t in tok) else None},
        "per_task": [{"task": r["task"], "completed": r["completed"], "failed_assertions": r["failed_assertions"], "duplicates": r["duplicate_actions"], "tool_calls": r["metrics"]["tool_calls"], "response_bytes": r["metrics"]["response_bytes"], "elapsed_s": r["metrics"]["elapsed_s"], "cpu_ms": r["resources"]["cpu_ms"], "peak_rss_kib": r["resources"]["peak_rss_kib"]} for r in results],
    }


def finish(out, results):
    for r in results:
        mark = "PASS" if r["completed"] else "FAIL"
        m = r["metrics"]
        print(f"{mark} {r['task']:17} calls={m['tool_calls']:3} resp_bytes={m['response_bytes']:7} elapsed={m['elapsed_s']}s dup={r['duplicate_submissions']}")
        for c in r["checks"]:
            if not c["ok"]:
                print(f"     failed: {c['name']}: {c['detail']}")
    summary = summarize(results)
    path = os.path.join(out, "summary.json")
    if os.path.exists(path):  # a second command into the same folder: keep the first summary
        path = os.path.join(out, f"summary-{secrets.token_hex(2)}.json")
    write_new(path, summary)
    with open(os.path.join(out, f"results-{secrets.token_hex(2)}.jsonl"), "x") as f:
        for r in results:
            f.write(json.dumps(r, sort_keys=True) + "\n")
    print(f"completed {summary['completed']}/{summary['tasks']}; results in {out}")
    return 0 if not summary["failed"] else 1


def cmd_report(args):
    results = []
    for root, _, files in sorted(os.walk(args.dir)):
        if "result.json" in files:
            results.append(json.load(open(os.path.join(root, "result.json"))))
    if not results:
        return die(f"no result.json under {args.dir}")
    print(json.dumps(summarize(results), indent=2, sort_keys=True))
    return 0 if all(r["completed"] for r in results) else 1


def cmd_list(_):
    for t in tk.TASKS.values():
        print(f"{t.id}: {t.title}")
    return 0


def unknown(ids):
    bad = [i for i in ids if i not in tk.TASKS]
    return f"unknown task {bad}; known: {', '.join(tk.TASKS)}" if bad else None


def die(msg):
    print(f"agent-eval: {msg}", file=sys.stderr)
    return 2


def parser():
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = p.add_subparsers(dest="cmd", required=True)

    def common(s, deadline=120):
        s.add_argument("--seed", type=int, default=1, help="fixture seed (default 1)")
        s.add_argument("--width", type=int, default=1280)
        s.add_argument("--height", type=int, default=800)
        s.add_argument("--deadline", type=float, default=deadline, help="run deadline in seconds")
        s.add_argument("--browser-sha", help="git SHA of the browser build (default: HEAD of this checkout, marked as unverified)")

    def agent(s):
        s.add_argument("--runner-name")
        s.add_argument("--model")
        s.add_argument("--prompt-file")
        s.add_argument("--tool-setup", dest="tool_setup", help="text: the tool set and harness of the run")

    s = sub.add_parser("smoke", help="scripted protocol smoke run")
    common(s)
    s.add_argument("--bare-mcp", required=True)
    s.add_argument("--out", required=True)
    s.add_argument("--task", action="append")
    s.add_argument("--call-timeout", type=int, default=20, help="seconds for one tool call")
    s.add_argument("--perf", action="store_true", help="count user instructions with perf stat")
    s.set_defaults(fn=cmd_smoke)
    s = sub.add_parser("fixture", help="start the fixture for an external agent")
    common(s, 900)
    agent(s)
    s.add_argument("--task", required=True)
    s.add_argument("--out", required=True)
    s.add_argument("--bare-mcp", help="binary the agent will use (recorded in the manifest)")
    s.set_defaults(fn=cmd_fixture)
    s = sub.add_parser("score", help="score an external transcript")
    common(s)
    agent(s)
    s.add_argument("--task", required=True)
    s.add_argument("--transcript", required=True)
    s.add_argument("--state", required=True, help="state.json written by the fixture command")
    s.add_argument("--out", required=True)
    s.add_argument("--answer-file")
    s.add_argument("--downloads-dir")
    s.add_argument("--tokens-json", help='{"input_tokens": N, "output_tokens": N} from the harness; without it tokens are unavailable')
    s.add_argument("--bare-mcp")
    s.set_defaults(fn=cmd_score)
    s = sub.add_parser("report")
    s.add_argument("dir")
    s.set_defaults(fn=cmd_report)
    s = sub.add_parser("list")
    s.set_defaults(fn=cmd_list)
    return p


def main(argv=None):
    args = parser().parse_args(argv)
    try:
        return args.fn(args)
    except FileExistsError as e:
        return die(f"refusing to overwrite {e.filename}")


if __name__ == "__main__":
    sys.exit(main())
