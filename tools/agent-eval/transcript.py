"""The MCP transcript: a collector that talks to an owned `bare-mcp` over stdio, the JSONL event format, its validation and its metrics. Python stdlib only.

Transcript schema v1: one JSON object per line, each with `v` (1), `type` and `t` (seconds on the monotonic clock since the first event, never goes back).
  request        id, method, bytes (the encoded line, with its newline), tool and session (tools/call only), arguments (redacted)
  response       id, bytes, is_error (bool), error (a JSON-RPC error object, or absent), text (redacted, cut to MAX_TEXT chars), text_chars (the full length)
  final_answer   text: what the agent reported at the end (extraction tasks)
  note           text: a runner event (deadline, stop). It is not an MCP event.
A response is matched to its request by `id`, never by order. `validate()` lists the problems of a transcript; `metrics()` counts it.
Redaction: `Redactor` replaces known secrets, values of keys that name a credential, and the `file` argument of state tools, so a transcript holds no
authentication state. The data in a transcript is synthetic.
"""

import hashlib
import json
import os
import queue
import re
import signal
import subprocess
import threading
import time

SCHEMA = 1
MAX_TEXT = 4000
SECRET_KEYS = re.compile(r"password|passwd|token|secret|cookie|authorization|credential|session_id|sid", re.I)
EXCLUDED_KEYS = {"file", "secrets_file"}  # paths of files that hold live credentials: never recorded
EVENT_TYPES = ("request", "response", "final_answer", "note")


def valid_id(value):
    return isinstance(value, (str, int)) and not isinstance(value, bool)


class Redactor:
    """Replace secrets in text and values. `source` is a function that gives the secret strings known now (it can grow while a run goes on)."""

    def __init__(self, source=lambda: []):
        self.source = source

    def text(self, s):
        for secret in sorted((x for x in self.source() if x), key=len, reverse=True):
            s = s.replace(secret, "[redacted]")
        return re.sub(r"\b(sid|token|session)=[^\s;&\"']+", r"\1=[redacted]", s)

    def value(self, v, key=""):
        if key in EXCLUDED_KEYS:
            return "[excluded]"
        if SECRET_KEYS.search(key) and isinstance(v, (str, int)):
            return "[redacted]"
        if isinstance(v, dict):
            return {k: self.value(x, k) for k, x in v.items()}
        if isinstance(v, list):
            return [self.value(x, key) for x in v]
        return self.text(v) if isinstance(v, str) else v


class Recorder:
    """Write transcript events to a new file (an existing file is never overwritten). Thread safe."""

    def __init__(self, path, redactor=None):
        self.f = open(path, "x", encoding="utf-8")  # "x": fails when the file exists
        self.t0 = time.monotonic()
        self.red = redactor or Redactor()
        self.lock = threading.Lock()
        self.events = []

    def add(self, typ, **kw):
        with self.lock:
            ev = {"v": SCHEMA, "type": typ, "t": round(time.monotonic() - self.t0, 6), **kw}
            self.events.append(ev)
            self.f.write(json.dumps(ev, separators=(",", ":")) + "\n")
            self.f.flush()
            return ev

    def close(self):
        self.f.close()


class DeadlineError(Exception):
    pass


class McpClient:
    """A stdio MCP client for one owned process. A reader thread routes each reply to its request by id, so replies may come in any order."""

    def __init__(self, argv, recorder, env=None, deadline=None, perf_out=None):
        self.rec = recorder
        self.deadline = deadline  # monotonic time at which the whole run stops
        self.next_id = 0
        self.pending = {}
        self.lock = threading.Lock()
        self.proc = subprocess.Popen(argv, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, env=env, start_new_session=True)
        self.usage = None  # rusage of the process, set by stop()
        self.stopped = False
        self.hwm_kib = None  # VmHWM of the process, read by stop() before it ends
        self.status = None
        self.reader = threading.Thread(target=self._read, daemon=True)
        self.reader.start()

    def _read(self):
        for raw in self.proc.stdout:
            try:
                msg = json.loads(raw)
            except ValueError:
                self.rec.add("note", text="unparsable output line", bytes=len(raw))
                continue
            with self.lock:
                slot = self.pending.pop(msg.get("id"), None) if valid_id(msg.get("id")) else None
            res = msg.get("result") or {}
            text = "".join(c.get("text", "") for c in res.get("content", []) if isinstance(c, dict))
            ev = {"id": msg.get("id"), "bytes": len(raw), "is_error": bool(res.get("isError")) or "error" in msg}
            if "error" in msg:
                ev["error"] = self.rec.red.value(msg["error"])
            ev["text"], ev["text_chars"] = self.rec.red.text(text)[:MAX_TEXT], len(text)
            if slot is None:
                ev["unmatched"] = True
            self.rec.add("response", **ev)
            if slot is not None:
                slot.put(msg)

    def request(self, method, params=None, timeout=30.0):
        """Send one request and wait for its reply. Gives the full reply message. Raises DeadlineError when the run deadline or `timeout` ends first."""
        with self.lock:
            self.next_id += 1
            rid = self.next_id
            slot = queue.Queue(1)
            self.pending[rid] = slot
        msg = {"jsonrpc": "2.0", "id": rid, "method": method, "params": params or {}}
        line = json.dumps(msg) + "\n"
        ev = {"id": rid, "method": method, "bytes": len(line.encode())}
        if method == "tools/call":
            ev["tool"] = (params or {}).get("name")
            args = (params or {}).get("arguments") or {}
            ev["session"] = args.get("session", "main")
            ev["arguments"] = self.rec.red.value(args)
        self.rec.add("request", **ev)
        try:
            self.proc.stdin.write(line.encode())
            self.proc.stdin.flush()
        except OSError as e:
            raise DeadlineError(f"server closed its input: {e}")
        wait = timeout if self.deadline is None else min(timeout, self.deadline - time.monotonic())
        try:
            return slot.get(timeout=max(wait, 0))
        except queue.Empty:
            raise DeadlineError("run deadline reached" if self.deadline is not None and time.monotonic() >= self.deadline else f"no reply to request {rid} in {timeout}s")

    def initialize(self):
        self.request("initialize", {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "agent-eval", "version": "1"}})
        self.proc.stdin.write(b'{"jsonrpc":"2.0","method":"notifications/initialized"}\n')
        self.proc.stdin.flush()

    def call(self, tool, timeout=30.0, **arguments):
        """tools/call. Gives (text, is_error)."""
        msg = self.request("tools/call", {"name": tool, "arguments": arguments}, timeout)
        res = msg.get("result") or {}
        text = "".join(c.get("text", "") for c in res.get("content", []) if isinstance(c, dict))
        return text, bool(res.get("isError")) or "error" in msg

    def _vm_hwm_kib(self):
        """Peak RSS of the live process from /proc (VmHWM). The wait4 value is not used: a forked child starts with the RSS of its parent in ru_maxrss.
        Under a wrapper such as `perf stat` the first child of the wrapper is the browser."""
        pid = self.proc.pid
        try:
            with open(f"/proc/{pid}/task/{pid}/children") as f:
                kids = f.read().split()
            if kids and os.path.basename(os.readlink(f"/proc/{pid}/exe")) != "bare-mcp":
                pid = int(kids[0])
            with open(f"/proc/{pid}/status") as f:
                for line in f:
                    if line.startswith("VmHWM:"):
                        return int(line.split()[1])
        except (OSError, ValueError):
            pass
        return None

    def stop(self, grace=3.0):
        """End the process: close its input, ask it to stop, kill it after `grace` seconds, and reap it. Keeps its rusage in `self.usage`."""
        if self.stopped:
            return
        self.stopped = True
        self.hwm_kib = self._vm_hwm_kib()
        try:
            self.proc.stdin.close()
        except OSError:
            pass
        done = (0, 0, None)
        end = time.monotonic() + grace
        while time.monotonic() < end:
            done = os.wait4(self.proc.pid, os.WNOHANG)
            if done[0]:
                break
            time.sleep(0.02)
        else:
            for sig in (signal.SIGTERM, signal.SIGKILL):
                try:
                    os.killpg(self.proc.pid, sig)
                except ProcessLookupError:
                    break
                time.sleep(0.2)
                done = os.wait4(self.proc.pid, os.WNOHANG)
                if done[0]:
                    break
            else:
                done = os.wait4(self.proc.pid, 0)
        if done[0]:
            self.status, self.usage = os.waitstatus_to_exitcode(done[1]), done[2]
        self.proc.returncode = self.status if self.status is not None else 0  # already reaped here: stop Popen from waiting again
        self.reader.join(2)
        self.proc.stdout.close()


# --- reading, validating and counting a transcript ---


def load(path):
    """Read a transcript file. Gives (events, problems): a line that is not a JSON object is a problem and is skipped."""
    events, problems = [], []
    with open(path, encoding="utf-8") as f:
        for n, line in enumerate(f, 1):
            if not line.strip():
                continue
            try:
                ev = json.loads(line)
            except ValueError:
                problems.append(f"line {n}: not JSON")
                continue
            if not isinstance(ev, dict):
                problems.append(f"line {n}: not an object")
                continue
            events.append(ev)
    return events, problems


def validate(events):
    """The problems of a transcript, as readable strings. An empty list is a valid transcript."""
    problems, requests, answered, last_t = [], {}, set(), None
    for n, ev in enumerate(events, 1):
        at = f"event {n}"
        if ev.get("v") != SCHEMA:
            problems.append(f"{at}: schema version {ev.get('v')!r}, expected {SCHEMA}")
        typ = ev.get("type")
        if typ not in EVENT_TYPES:
            problems.append(f"{at}: unknown type {typ!r}")
            continue
        t = ev.get("t")
        if not isinstance(t, (int, float)) or isinstance(t, bool):
            problems.append(f"{at}: t is missing or not a number")
        else:
            if last_t is not None and t < last_t:
                problems.append(f"{at}: t goes back ({t} < {last_t})")
            last_t = t
        if typ in ("request", "response") and not valid_id(ev.get("id")):
            problems.append(f"{at}: id must be a string or integer")
            continue
        if typ == "request":
            if "id" not in ev or "method" not in ev or not isinstance(ev.get("bytes"), int):
                problems.append(f"{at}: request needs id, method and bytes")
            elif ev["id"] in requests:
                problems.append(f"{at}: request id {ev['id']} used twice")
            else:
                requests[ev["id"]] = ev
        elif typ == "response":
            rid = ev.get("id")
            if rid not in requests:
                problems.append(f"{at}: response to unknown request id {rid!r}")
            elif rid in answered:
                problems.append(f"{at}: second response to request id {rid}")
            if not isinstance(ev.get("bytes"), int) or not isinstance(ev.get("is_error"), bool):
                problems.append(f"{at}: response needs bytes and is_error")
            answered.add(rid)
        elif typ == "final_answer" and not isinstance(ev.get("text"), str):
            problems.append(f"{at}: final_answer needs text")
    for rid in requests:
        if rid not in answered:
            problems.append(f"request id {rid} has no response")
    return problems


def metrics(events):
    """Counts from the events. Only what the transcript holds: no estimate of tokens or CPU."""
    reqs = [e for e in events if e.get("type") == "request"]
    resps = [e for e in events if e.get("type") == "response"]
    calls = [e for e in reqs if e.get("method") == "tools/call"]
    ts = [e["t"] for e in events if isinstance(e.get("t"), (int, float))]
    by_tool = {}
    for e in calls:
        by_tool[e.get("tool")] = by_tool.get(e.get("tool"), 0) + 1
    ids = {e["id"] for e in calls if valid_id(e.get("id"))}
    return {
        "tool_calls": len(calls),
        "tool_calls_by_tool": by_tool,
        "tool_errors": sum(1 for e in resps if e.get("is_error") and valid_id(e.get("id")) and e.get("id") in ids),
        "sessions": sorted({e.get("session") for e in calls if e.get("session")}),
        "request_bytes": sum(e.get("bytes", 0) for e in reqs),
        "response_bytes": sum(e.get("bytes", 0) for e in resps),
        "elapsed_s": round(max(ts) - min(ts), 6) if ts else None,
    }


def resources(usage, status, hwm_kib=None, perf_instructions=None, perf_reason="not requested"):
    """CPU from the wait4 rusage and peak RSS from VmHWM of the one owned process. A value that was not collected is null and has a reason, never zero.
    Scope: the whole process life (one bare-mcp process serves one task in a smoke run), so this is process-wide and also the total of that one task."""
    out = {"scope": "bare-mcp process; one process per task", "exit_status": status}
    out["cpu_ms"] = None if usage is None else round((usage.ru_utime + usage.ru_stime) * 1000, 3)
    out["peak_rss_kib"] = hwm_kib
    out["reason"] = "; ".join(r for r in (None if usage is not None else "cpu: process was not reaped by the runner", None if hwm_kib is not None else "peak rss: /proc VmHWM unreadable (not Linux, or the process ended early)") if r) or None
    out["instructions"] = perf_instructions
    out["instructions_reason"] = None if perf_instructions is not None else perf_reason
    return out


def file_sha256(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()
