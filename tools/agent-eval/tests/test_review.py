"""Regression checks for lifecycle evidence and malformed transcript inputs."""
import unittest
from common import tk, fx
import run as runner
import transcript as tr


def events(calls):
    out = []
    for i, (tool, session) in enumerate(calls, 1):
        out.extend([
            {"v": 1, "type": "request", "t": i, "id": i, "method": "tools/call", "tool": tool, "session": session, "bytes": 10, "arguments": {"name": "saved"} if tool == "ensure_session" else {}},
            {"v": 1, "type": "response", "t": i + 0.1, "id": i, "bytes": 10, "is_error": False},
        ])
    return out


class Review(unittest.TestCase):
    def ctx(self, state):
        return tk.Ctx(world=fx.make_world(1), base="", state=state)

    def failures(self, task, ctx, trace):
        checks, _ = runner.grade(tk.TASKS[task], ctx, trace, tr.validate(trace))
        return [c["name"] for c in checks if not c["ok"]]

    def test_repeated_account_reads_do_not_prove_restore(self):
        ctx = self.ctx({"logins_ok": [{"user": "alice"}], "account_views": [{"sid": "same"}] * 2})
        self.assertIn("session_restore_requested", self.failures("login_restore", ctx, events([("read", "main")] * 2)))

    def test_two_ensure_calls_are_restore_evidence(self):
        ctx = self.ctx({"logins_ok": [{"user": "alice"}], "account_views": [{"sid": "same"}] * 2})
        self.assertEqual(self.failures("login_restore", ctx, events([("ensure_session", "main")] * 2)), [])

    def test_single_live_session_does_not_prove_two_sessions(self):
        ctx = self.ctx({"logins_ok": [{"user": "alice"}, {"user": "bob"}], "notes": [{"user": "alice", "text": "Alice note 1"}, {"user": "bob", "text": "Bob note 1"}]})
        self.assertIn("independent_live_sessions", self.failures("two_sessions", ctx, events([("open", "main"), ("open", "main")])))
        self.assertEqual(self.failures("two_sessions", ctx, events([("open", "alice"), ("open", "bob")])), [])
        self.assertIn("independent_live_sessions", self.failures("two_sessions", ctx, events([("open", "alice"), ("close", "alice"), ("open", "bob")])))

    def test_empty_trace_cannot_prove_a_completed_task(self):
        ctx = self.ctx({"logins_ok": [{"user": "alice"}], "account_views": [{"sid": "same"}] * 2})
        self.assertIn("tool_calls_present", self.failures("login_restore", ctx, []))

    def test_container_ids_are_reported_without_crashing(self):
        for bad in ([], {}):
            trace = events([("read", "main")])
            for event in trace:
                event["id"] = bad
            self.assertTrue(tr.validate(trace))
            self.assertEqual(tr.metrics(trace)["tool_calls"], 1)
