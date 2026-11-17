"""The command line: help, a smoke run against a fake server that only says ok, scoring an external transcript, and the no-overwrite rule."""
import json
import os
import signal
import subprocess
import sys
import tempfile
import unittest
import urllib.request

from common import fx  # noqa: F401  (sets the import path)

HERE = os.path.dirname(os.path.abspath(__file__))
RUN = [sys.executable, os.path.join(HERE, "..", "run.py")]
FAKE = os.path.join(HERE, "fake_mcp.py")


def run(*args, **kw):
    return subprocess.run([*RUN, *args], capture_output=True, text=True, timeout=120, **kw)


def write_jsonl(path, events):
    with open(path, "w") as f:
        for e in events:
            f.write(json.dumps(e) + "\n")


def good_transcript(answer):
    ev = [
        {"v": 1, "type": "request", "t": 0.0, "id": 1, "method": "tools/call", "tool": "read", "session": "main", "bytes": 60},
        {"v": 1, "type": "response", "t": 0.2, "id": 1, "bytes": 300, "is_error": False, "text": "page"},
    ]
    if answer is not None:
        ev.append({"v": 1, "type": "final_answer", "t": 0.3, "text": answer})
    return ev


class Cli(unittest.TestCase):
    def setUp(self):
        self.d = tempfile.mkdtemp()
        self.addCleanup(lambda: __import__("shutil").rmtree(self.d, ignore_errors=True))

    def p(self, *x):
        return os.path.join(self.d, *x)

    def test_help_exits_0_and_names_the_commands(self):
        r = run("--help")
        self.assertEqual(r.returncode, 0)
        for word in ("smoke", "fixture", "score", "SCRIPTED"):
            self.assertIn(word, r.stdout)

    def test_a_server_that_only_says_ok_completes_no_task(self):
        os.chmod(FAKE, 0o755)
        r = run("smoke", "--bare-mcp", FAKE, "--out", self.p("o"), "--deadline", "30")
        self.assertEqual(r.returncode, 1, r.stdout + r.stderr)
        self.assertIn("completed 0/8", r.stdout)
        s = json.load(open(self.p("o", "summary.json")))
        self.assertEqual((s["completed"], s["tasks"], s["runner_kinds"]), (0, 8, ["scripted"]))
        self.assertGreater(s["failed_assertions"], 8)
        self.assertFalse(s["tokens"]["available"])
        one = json.load(open(self.p("o", "upload", "result.json")))
        self.assertFalse(one["completed"])
        self.assertEqual(one["tokens"]["available"], False)
        man = json.load(open(self.p("o", "upload", "manifest.json")))
        self.assertEqual((man["runner"]["kind"], man["task"]["version"], man["viewport"], man["fixture"]["seed"]), ("scripted", 2, {"width": 1280, "height": 800}, 1))
        self.assertEqual(len(man["browser"]["binary_sha256"]), 64)

    def test_smoke_refuses_a_used_output_folder_and_a_bad_binary(self):
        os.makedirs(self.p("o"))
        open(self.p("o", "keep"), "w").close()
        self.assertEqual(run("smoke", "--bare-mcp", FAKE, "--out", self.p("o")).returncode, 2)
        self.assertEqual(run("smoke", "--bare-mcp", "/nonexistent", "--out", self.p("n")).returncode, 2)
        self.assertEqual(run("smoke", "--bare-mcp", FAKE, "--out", self.p("n"), "--task", "nope").returncode, 2)

    def state_for(self, **over):
        w = fx.make_world(3)
        t = next(i for i in w["items"] if i["id"] == w["search_target"])
        st = {"seed": 3, "requests": 1, "item_views": [t["id"]], **over}
        write_jsonl(self.p("tr.jsonl"), good_transcript(f"{t['sku']} {t['price']}"))
        with open(self.p("state.json"), "w") as f:
            json.dump(st, f)

    def test_score_external_transcript_needs_no_model_and_marks_missing_tokens(self):
        self.state_for()
        r = run("score", "--task", "search_extract", "--transcript", self.p("tr.jsonl"), "--state", self.p("state.json"), "--out", self.p("s"))
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        res = json.load(open(self.p("s", "search_extract", "result.json")))
        self.assertTrue(res["completed"])
        self.assertEqual(res["runner_kind"], "external")
        self.assertFalse(res["agent_recorded"])
        self.assertFalse(res["tokens"]["available"])
        self.assertIsNone(res["resources"]["cpu_ms"])
        self.assertTrue(res["resources"]["reason"])
        self.assertEqual(res["metrics"]["response_bytes"], 300)

    def test_supplied_tokens_are_recorded_as_supplied(self):
        self.state_for()
        with open(self.p("tok.json"), "w") as f:
            json.dump({"input_tokens": 10, "output_tokens": 4}, f)
        r = run("score", "--task", "search_extract", "--transcript", self.p("tr.jsonl"), "--state", self.p("state.json"), "--out", self.p("s"), "--tokens-json", self.p("tok.json"))
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        res = json.load(open(self.p("s", "search_extract", "result.json")))
        self.assertEqual(res["tokens"], {"available": True, "input_tokens": 10, "output_tokens": 4, "source": "supplied by the caller"})

    def test_an_invalid_transcript_fails_the_task_and_says_why(self):
        self.state_for()
        events = good_transcript("x")[:1]  # the response is missing
        write_jsonl(self.p("tr.jsonl"), events)
        r = run("score", "--task", "search_extract", "--transcript", self.p("tr.jsonl"), "--state", self.p("state.json"), "--out", self.p("s"))
        self.assertEqual(r.returncode, 1)
        self.assertIn("transcript_valid", r.stdout)
        self.assertIn("has no response", r.stdout)

    def test_a_wrong_answer_is_a_scored_failure_with_exit_1(self):
        self.state_for()
        write_jsonl(self.p("tr.jsonl"), good_transcript("SKU-000000 $0.00"))
        r = run("score", "--task", "search_extract", "--transcript", self.p("tr.jsonl"), "--state", self.p("state.json"), "--out", self.p("s"))
        self.assertEqual(r.returncode, 1)
        self.assertIn("answer_has_sku", r.stdout)

    def test_score_does_not_overwrite_a_result(self):
        self.state_for()
        args = ("score", "--task", "search_extract", "--transcript", self.p("tr.jsonl"), "--state", self.p("state.json"), "--out", self.p("s"))
        self.assertEqual(run(*args).returncode, 0)
        r = run(*args)
        self.assertEqual(r.returncode, 2)
        self.assertIn("refusing to overwrite", r.stderr)

    def test_a_negative_token_file_and_a_missing_state_are_usage_errors(self):
        self.state_for()
        with open(self.p("tok.json"), "w") as f:
            json.dump({"input_tokens": -1, "output_tokens": 1}, f)
        base = ("score", "--task", "search_extract", "--transcript", self.p("tr.jsonl"), "--out", self.p("s"))
        self.assertEqual(run(*base, "--state", self.p("state.json"), "--tokens-json", self.p("tok.json")).returncode, 2)
        self.assertEqual(run(*base, "--state", self.p("none.json")).returncode, 2)

    def test_fixture_command_serves_then_writes_the_record_on_sigterm(self):
        p = subprocess.Popen([*RUN, "fixture", "--task", "overlay_delayed", "--out", self.p("f"), "--seed", "4", "--deadline", "60"], stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        try:
            info = json.loads(p.stdout.readline())
            self.assertIn("exactly once", info["prompt"])
            req = urllib.request.Request(info["base_url"] + "/api/submit", data=b'{"note":"n"}', headers={"Content-Type": "application/json"})
            urllib.request.urlopen(req).read()
            p.send_signal(signal.SIGTERM)
            p.wait(10)
        finally:
            if p.poll() is None:
                p.kill()
                p.wait()
            p.stdout.close()
            p.stderr.close()
        state = json.load(open(self.p("f", "overlay_delayed", "state.json")))
        self.assertEqual((state["seed"], len(state["submits"])), (4, 1))
        self.assertNotIn("admin", json.dumps(state))
        man = open(self.p("f", "overlay_delayed", "manifest.json")).read()
        self.assertNotIn("X-Eval-Admin", man)


if __name__ == "__main__":
    unittest.main()
