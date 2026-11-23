"""Arms (#964): the snapshot mode of a run is recorded in the manifest, the state and the result, and the report groups by task and arm."""
import json
import os
import signal
import subprocess
import sys
import tempfile
import time
import unittest

from common import fx  # noqa: F401  (sets the import path)
import run as rn  # noqa: E402

HERE = os.path.dirname(os.path.abspath(__file__))
RUN = [sys.executable, os.path.join(HERE, "..", "run.py")]


def result(task, arm, done, calls, chars=0, tokens=None):
    return {
        "task": task, "arm": arm, "completed": done, "metrics": {"tool_calls": calls, "response_chars": chars},
        "tokens": {"available": True, "input_tokens": tokens[0], "output_tokens": tokens[1]} if tokens else {"available": False},
    }


class ByArm(unittest.TestCase):
    def test_groups_by_task_and_arm_with_medians(self):
        rs = [result("upload", "all", True, 4, 400), result("upload", "all", False, 10, 800), result("upload", "all", True, 6, 1200),
              result("upload", "interactive", True, 3, 100), result("download", "all", True, 2, 40)]
        rows = {(r["task"], r["arm"]): r for r in rn.by_arm(rs)}
        a = rows[("upload", "all")]
        self.assertEqual((a["runs"], a["completed"], a["success_rate"], a["median_turns"], a["median_total_tokens"]), (3, 2, 0.667, 6, 200))
        self.assertTrue(a["tokens_source"].startswith("estimate"))
        self.assertEqual(rows[("upload", "interactive")]["median_turns"], 3)
        self.assertEqual(len(rows), 3)

    def test_even_count_gives_the_mean_of_the_middle_two(self):
        rows = rn.by_arm([result("t", "all", True, 2), result("t", "all", True, 5)])
        self.assertEqual(rows[0]["median_turns"], 3.5)

    def test_reported_tokens_win_over_the_estimate(self):
        rows = rn.by_arm([result("t", "all", True, 2, 4000, (100, 20)), result("t", "all", True, 2, 4000, (300, 40))])
        self.assertEqual((rows[0]["median_total_tokens"], rows[0]["tokens_source"]), (230, "reported"))

    def test_mixed_sources_are_labelled(self):
        rows = rn.by_arm([result("t", "all", True, 2, 40, (1, 1)), result("t", "all", True, 2, 40)])
        self.assertTrue(rows[0]["tokens_source"].startswith("mixed"))

    def test_a_run_without_an_arm_is_none(self):
        self.assertEqual(rn.by_arm([result("t", None, True, 1)])[0]["arm"], "none")

    def test_mcp_env_names_the_arm(self):
        self.assertEqual(rn.mcp_env("all"), {"BARE_MCP_SNAPSHOT_MODE": "all"})


class Cli(unittest.TestCase):
    def setUp(self):
        self.d = tempfile.mkdtemp()
        self.addCleanup(lambda: __import__("shutil").rmtree(self.d, ignore_errors=True))

    def test_an_unknown_arm_is_refused(self):
        r = subprocess.run([*RUN, "fixture", "--task", "upload", "--out", self.d + "/x", "--arm", "compact"], capture_output=True, text=True, timeout=60)
        self.assertEqual(r.returncode, 2)
        self.assertIn("--arm", r.stderr)

    def test_fixture_score_and_report_keep_the_arm(self):
        out = os.path.join(self.d, "ext")
        p = subprocess.Popen([*RUN, "fixture", "--task", "upload", "--out", out, "--arm", "all", "--deadline", "60"], stdout=subprocess.PIPE, text=True)
        try:
            info = json.loads(p.stdout.readline())
        finally:
            p.send_signal(signal.SIGINT)
            p.wait(timeout=30)
        self.assertEqual((info["arm"], info["mcp_env"]), ("all", {"BARE_MCP_SNAPSHOT_MODE": "all"}))
        self.assertEqual(json.load(open(os.path.join(out, "upload", "manifest.json")))["arm"], "all")
        state = os.path.join(out, "upload", "state.json")
        self.assertEqual(json.load(open(state))["arm"], "all")
        tr = os.path.join(self.d, "t.jsonl")
        with open(tr, "w") as f:
            for e in (
                {"v": 1, "type": "request", "t": 0.0, "id": 1, "method": "tools/call", "tool": "read", "session": "main", "bytes": 60},
                {"v": 1, "type": "response", "t": 0.2, "id": 1, "bytes": 300, "is_error": False, "text": "page", "text_chars": 400},
            ):
                f.write(json.dumps(e) + "\n")
        sc = os.path.join(self.d, "score")
        subprocess.run([*RUN, "score", "--task", "upload", "--transcript", tr, "--state", state, "--out", sc], capture_output=True, text=True, timeout=60)
        self.assertEqual(json.load(open(os.path.join(sc, "upload", "result.json")))["arm"], "all")
        r = subprocess.run([*RUN, "report", sc], capture_output=True, text=True, timeout=60)
        rep = json.loads(r.stdout)
        self.assertEqual([(x["task"], x["arm"], x["runs"], x["median_turns"], x["median_total_tokens"]) for x in rep["by_arm"]], [("upload", "all", 1, 1, 100)])
        self.assertIn("estimate", r.stderr)


if __name__ == "__main__":
    unittest.main()
