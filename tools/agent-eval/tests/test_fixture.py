import json
import unittest
import urllib.error
import urllib.request

from common import Browser, fx


class FixtureTests(unittest.TestCase):
    def setUp(self):
        self.f = fx.Fixture(7)
        self.addCleanup(self.f.stop)
        self.b = Browser(self.f.base)

    def test_same_seed_same_world_and_other_seed_differs(self):
        a, b, c = fx.make_world(7), fx.make_world(7), fx.make_world(8)
        self.assertEqual(a, b)
        self.assertNotEqual(a["users"]["alice"]["password"], c["users"]["alice"]["password"])
        self.assertTrue(a["users"]["alice"]["email"].endswith("@example.test"))

    def test_reset_drops_state_and_applies_the_new_seed(self):
        self.b.login("alice", self.f.world)
        self.assertEqual(len(self.f.state()["logins_ok"]), 1)
        self.f.reset(9)
        self.assertEqual(self.f.state()["logins_ok"], [])
        self.assertEqual(self.f.world["seed"], 9)
        self.assertEqual(self.f.sessions, {})

    def test_admin_endpoints_need_the_token(self):
        with self.assertRaises(urllib.error.HTTPError) as e:
            urllib.request.urlopen(self.f.base + "/__admin/state")
        self.assertEqual(e.exception.code, 403)
        req = urllib.request.Request(self.f.base + "/__admin/state", headers={"X-Eval-Admin": self.f.admin_token})
        self.assertEqual(json.load(urllib.request.urlopen(req))["seed"], 7)
        req = urllib.request.Request(self.f.base + "/__admin/reset?seed=3", method="POST", headers={"X-Eval-Admin": self.f.admin_token})
        urllib.request.urlopen(req)
        self.assertEqual(self.f.world["seed"], 3)

    def test_state_holds_session_hashes_and_not_session_ids(self):
        self.b.login("alice", self.f.world)
        sid = next(iter(self.f.sessions))
        self.assertNotIn(sid, json.dumps(self.f.state()))
        self.assertIn(sid, self.f.secrets())
        self.assertIn(self.f.world["users"]["alice"]["password"], self.f.secrets())

    def test_wrong_password_is_counted_and_gives_no_session(self):
        with self.assertRaises(urllib.error.HTTPError) as e:
            self.b.post("/login", email=self.f.world["users"]["alice"]["email"], password="nope", next="/account")
        self.assertEqual(e.exception.code, 401)
        self.assertEqual(self.f.state()["logins_failed"], 1)
        self.assertEqual(self.f.sessions, {})

    def test_submit_counter_counts_every_post(self):
        for _ in range(3):
            self.b.post_json("/api/submit", {"note": "x"})
        self.assertEqual(len(self.f.state()["submits"]), 3)

    def test_result_is_delayed(self):
        t = self.b.post_json("/api/submit", {"note": "x"})["ticket"]
        self.assertFalse(json.loads(self.b.get("/api/result?ticket=" + t))["ready"])

    def test_upload_records_exact_bytes(self):
        self.b.upload("/upload", "a.bin", b"\x00\x01\r\n--x\xff")
        up = self.f.state()["uploads"]
        self.assertEqual((up[0]["name"], up[0]["bytes"]), ("a.bin", 8))

    def test_flow_needs_the_chain(self):
        with self.assertRaises(urllib.error.HTTPError):
            self.b.get("/flow/3")
        self.assertEqual(self.f.state()["flow_bad_chain"], 1)


if __name__ == "__main__":
    unittest.main()
