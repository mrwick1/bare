import json
import os
import sys
import tempfile
import threading
import time
import unittest

from common import tk  # noqa: F401  (sets the import path)
import transcript as tr

FAKE = [sys.executable, os.path.join(os.path.dirname(os.path.abspath(__file__)), "fake_mcp.py")]


def ev(typ, t, **kw):
    return {"v": 1, "type": typ, "t": t, **kw}


def req(i, t=0.0, **kw):
    return ev("request", t, id=i, method="tools/call", bytes=50, tool="open", session="main", **kw)


def resp(i, t=0.1, **kw):
    return ev("response", t, id=i, bytes=80, is_error=False, **kw)


class Validate(unittest.TestCase):
    def test_valid_transcript_has_no_problems(self):
        self.assertEqual(tr.validate([req(1), resp(1), ev("final_answer", 0.2, text="x")]), [])

    def test_request_without_response(self):
        self.assertIn("request id 2 has no response", tr.validate([req(1), resp(1), req(2, 0.2)]))

    def test_response_to_unknown_request_and_second_response(self):
        p = tr.validate([req(1), resp(1), resp(1, 0.2), resp(9, 0.3)])
        self.assertTrue(any("second response to request id 1" in x for x in p), p)
        self.assertTrue(any("unknown request id 9" in x for x in p), p)

    def test_replies_may_come_in_any_order(self):
        self.assertEqual(tr.validate([req(1), req(2, 0.01), resp(2, 0.02), resp(1, 0.03)]), [])

    def test_missing_fields_and_bad_version_and_time(self):
        p = tr.validate([{"type": "request", "t": 0, "id": 1}, ev("response", "x", id=1), {"v": 1, "type": "wat", "t": 1}, ev("final_answer", 2)])
        joined = "\n".join(p)
        for needle in ("schema version", "request needs id, method and bytes", "t is missing", "unknown type", "final_answer needs text"):
            self.assertIn(needle, joined)

    def test_time_must_not_go_back(self):
        self.assertTrue(any("goes back" in x for x in tr.validate([req(1, 1.0), resp(1, 0.5)])))

    def test_load_skips_and_reports_malformed_lines(self):
        with tempfile.TemporaryDirectory() as d:
            p = os.path.join(d, "t.jsonl")
            with open(p, "w") as f:
                f.write(json.dumps(req(1)) + "\n{broken\n[1]\n\n" + json.dumps(resp(1)) + "\n")
            events, problems = tr.load(p)
        self.assertEqual(len(events), 2)
        self.assertEqual(problems, ["line 2: not JSON", "line 3: not an object"])


class Metrics(unittest.TestCase):
    def test_counts_come_from_the_events(self):
        m = tr.metrics([req(1, 0), resp(1, 0.5, ), req(2, 1.0, arguments={}), resp(2, 2.0)])
        self.assertEqual((m["tool_calls"], m["request_bytes"], m["response_bytes"], m["elapsed_s"], m["sessions"]), (2, 100, 160, 2.0, ["main"]))

    def test_empty_transcript_gives_null_elapsed_not_zero(self):
        self.assertIsNone(tr.metrics([])["elapsed_s"])

    def test_missing_counters_are_null_with_a_reason(self):
        r = tr.resources(None, None, None, None, "no perf")
        self.assertIsNone(r["cpu_ms"])
        self.assertIsNone(r["peak_rss_kib"])
        self.assertIsNone(r["instructions"])
        self.assertIn("not reaped", r["reason"])
        self.assertEqual(r["instructions_reason"], "no perf")


class Redaction(unittest.TestCase):
    def test_secrets_keys_and_state_files(self):
        red = tr.Redactor(lambda: ["pw-abc123", "sid-deadbeef"])
        out = red.value({"text": "pw-abc123", "password": "x", "file": "/tmp/state.json", "nested": [{"token": "t", "ok": "Cookie sid=sid-deadbeef; a=1"}], "n": 3})
        s = json.dumps(out)
        for bad in ("pw-abc123", "sid-deadbeef", "/tmp/state.json"):
            self.assertNotIn(bad, s)
        self.assertEqual(out["n"], 3)


class Collector(unittest.TestCase):
    def start(self, mode="echo", deadline=None):
        d = tempfile.mkdtemp()
        self.addCleanup(lambda: __import__("shutil").rmtree(d, ignore_errors=True))
        rec = tr.Recorder(os.path.join(d, "t.jsonl"))
        c = tr.McpClient(FAKE, rec, env={**os.environ, "FAKE_MODE": mode}, deadline=deadline)
        self.addCleanup(rec.close)
        self.addCleanup(c.stop)
        return c, rec, d

    def test_recorder_never_overwrites(self):
        c, rec, d = self.start()
        with self.assertRaises(FileExistsError):
            tr.Recorder(os.path.join(d, "t.jsonl"))

    def test_correlates_replies_that_arrive_out_of_order(self):
        c, rec, _ = self.start("reorder")
        c.initialize()
        out = {}
        ts = [threading.Thread(target=lambda n=n: out.__setitem__(n, c.call("open", timeout=5, url=f"u{n}"))) for n in (1, 2)]
        for t in ts:
            t.start()
            time.sleep(0.1)
        for t in ts:
            t.join(5)
        self.assertEqual(len(out), 2)
        ids = {e["id"]: e for e in rec.events if e["type"] == "request" and e.get("tool")}
        for n, (text, err) in out.items():
            rid = next(i for i, e in ids.items() if e["arguments"] == {"url": f"u{n}"})
            self.assertEqual(text, f"ok (1 ms) #{rid}")  # each caller got the reply of its own id
        self.assertEqual(tr.validate(rec.events), [])

    def test_request_and_response_events_hold_ids_session_time_and_bytes(self):
        c, rec, _ = self.start()
        c.initialize()
        c.call("act", session="s2", op="click")
        rq = [e for e in rec.events if e["type"] == "request"][-1]
        rs = [e for e in rec.events if e["type"] == "response"][-1]
        self.assertEqual((rq["tool"], rq["session"], rq["id"], rs["id"]), ("act", "s2", rs["id"], rq["id"]))
        self.assertGreater(rq["bytes"], 0)
        self.assertGreater(rs["bytes"], 0)
        self.assertGreaterEqual(rs["t"], rq["t"])

    def test_deadline_keeps_the_partial_transcript_and_reaps_the_process(self):
        c, rec, _ = self.start("hang", deadline=time.monotonic() + 1.0)
        c.initialize()
        with self.assertRaises(tr.DeadlineError):
            c.call("open", timeout=30, url="x")
        c.stop(grace=0.3)
        self.assertIsNotNone(c.proc.pid)
        with self.assertRaises(ProcessLookupError):
            os.kill(c.proc.pid, 0)  # gone, not a zombie: it was reaped
        problems = tr.validate(rec.events)
        self.assertEqual(problems, ["request id 2 has no response"])  # the partial failure stays visible
        self.assertIsNotNone(c.usage)


if __name__ == "__main__":
    unittest.main()
