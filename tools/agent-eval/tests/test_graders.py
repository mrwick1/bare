"""Graders judge fixture state and artifacts, not receipts: each task has a genuine completion and at least one failure that looks like success."""
import os
import tempfile
import unittest

from common import Browser, ctx_for, failed, fx, tk


class Base(unittest.TestCase):
    def setUp(self):
        self.f = fx.Fixture(5)
        self.addCleanup(self.f.stop)
        self.w = self.f.world
        self.b = Browser(self.f.base)

    def grade(self, task, **kw):
        c = ctx_for(self.f, task, **kw)
        return tk.TASKS[task].grade(c), tk.TASKS[task].duplicates(c)


class Login(Base):
    def test_one_login_then_restore_passes(self):
        self.b.login("alice", self.w)
        self.b.get("/account")
        checks, dups = self.grade("login_restore")
        self.assertEqual(failed(checks), [])
        self.assertEqual(dups, {"login": 0})

    def test_second_login_is_a_repeat(self):
        self.b.login("alice", self.w)
        self.b.login("alice", self.w)
        checks, dups = self.grade("login_restore")
        self.assertIn("signed_in_once", failed(checks))
        self.assertEqual(dups["login"], 1)

    def test_nothing_done_fails(self):
        self.assertEqual(failed(self.grade("login_restore")[0]), ["account_loaded_twice_with_one_session", "signed_in_once"])


class Search(Base):
    def target(self):
        return next(i for i in self.w["items"] if i["id"] == self.w["search_target"])

    def test_exact_data_after_opening_the_item(self):
        t = self.target()
        self.b.get("/item/" + t["id"])
        self.assertEqual(failed(self.grade("search_extract", answer=f"{t['sku']} {t['price']}")[0]), [])

    def test_a_decoy_answer_fails(self):
        t = self.target()
        d = next(i for i in self.w["items"] if i["id"] != t["id"])
        self.b.get("/item/" + t["id"])
        self.assertIn("answer_has_sku", failed(self.grade("search_extract", answer=f"{d['sku']} {d['price']}")[0]))

    def test_a_right_answer_without_opening_the_page_fails(self):
        t = self.target()
        self.assertEqual(failed(self.grade("search_extract", answer=f"{t['sku']} {t['price']}")[0]), ["item_page_was_opened"])

    def test_list_of_every_sku_fails(self):
        for i in self.w["items"]:
            self.b.get("/item/" + i["id"])
        t = self.target()
        self.assertIn("answer_has_no_other_sku", failed(self.grade("search_extract", answer=" ".join(i["sku"] + i["price"] for i in self.w["items"]))[0]))


class Duplicates(Base):
    def test_only_the_target_once(self):
        self.b.post("/approve", id=self.w["approval_target"])
        self.assertEqual(failed(self.grade("duplicate_button")[0]), [])

    def test_wrong_button_fails_even_with_a_status_page(self):
        wrong = next(r["id"] for r in self.w["approvals"] if r["id"] != self.w["approval_target"])
        page = self.b.post("/approve", id=wrong)
        self.assertIn("Approved", page)  # the receipt looks like success
        self.assertIn("target_approved", failed(self.grade("duplicate_button")[0]))

    def test_target_twice_fails(self):
        for _ in range(2):
            self.b.post("/approve", id=self.w["approval_target"])
        checks, dups = self.grade("duplicate_button")
        self.assertEqual(failed(checks), ["approved_once"])
        self.assertEqual(dups["approve"], 1)


class Overlay(Base):
    def run_flow(self, submits):
        self.b.post_json("/api/dismiss", {})
        for _ in range(submits):
            t = self.b.post_json("/api/submit", {"note": self.w["overlay"]["note"]})["ticket"]
        import time

        time.sleep(fx.RESULT_DELAY + 0.1)
        return self.b.get("/api/result?ticket=" + t)

    def test_one_submit_passes(self):
        self.assertIn('"ready": true', self.run_flow(1))
        self.assertEqual(failed(self.grade("overlay_delayed")[0]), [])

    def test_two_submits_fail_though_the_result_page_succeeds(self):
        self.assertIn('"ready": true', self.run_flow(2))
        checks, dups = self.grade("overlay_delayed")
        self.assertEqual(failed(checks), ["note_is_exact", "report_sent_once"])
        self.assertEqual(dups["submit"], 1)

    def test_a_submit_without_waiting_for_the_result_fails(self):
        self.b.post_json("/api/dismiss", {})
        self.b.post_json("/api/submit", {"note": self.w["overlay"]["note"]})
        self.assertEqual(failed(self.grade("overlay_delayed")[0]), ["result_was_delivered_to_the_page"])


class Flow(Base):
    def walk(self):
        fl = self.w["flow"]
        self.b.get("/flow/1")
        self.b.get("/flow/2?k=" + fl["n1"])
        self.b.get("/flow/3?k=" + fl["n2"])

    def test_walk_and_code(self):
        self.walk()
        self.assertEqual(failed(self.grade("nav_stale_ref", answer=self.w["flow"]["code"])[0]), [])

    def test_code_without_the_walk_fails(self):
        self.assertIn("steps_visited_in_order", failed(self.grade("nav_stale_ref", answer=self.w["flow"]["code"])[0]))

    def test_walk_without_the_code_fails(self):
        self.walk()
        self.assertEqual(failed(self.grade("nav_stale_ref", answer="done")[0]), ["answer_has_the_code"])


class TwoSessions(Base):
    def test_each_user_posts_once(self):
        s = self.w["seed"]
        other = Browser(self.f.base)
        self.b.login("alice", self.w, "/board")
        self.b.post("/notes", note=f"Alice note {s}")
        other.login("bob", self.w, "/board")
        other.post("/notes", note=f"Bob note {s}")
        self.assertEqual(failed(self.grade("two_sessions")[0]), [])

    def test_a_shared_cookie_jar_posts_as_the_wrong_user(self):
        s = self.w["seed"]
        self.b.login("alice", self.w, "/board")
        self.b.login("bob", self.w, "/board")  # the second login replaces the cookie of the first
        self.b.post("/notes", note=f"Alice note {s}")
        self.b.post("/notes", note=f"Bob note {s}")
        self.assertIn("notes_posted_by_the_right_users_once", failed(self.grade("two_sessions")[0]))


class Files(Base):
    def test_upload_exact_bytes_once(self):
        self.b.upload("/upload", self.w["upload_name"], self.w["upload"])
        self.assertEqual(failed(self.grade("upload")[0]), [])

    def test_upload_of_changed_bytes_fails_though_the_page_says_received(self):
        page = self.b.upload("/upload", self.w["upload_name"], self.w["upload"][:-1] + b"\x00")
        self.assertIn("Received", page)
        self.assertIn("bytes_are_exact", failed(self.grade("upload")[0]))

    def test_upload_twice_fails(self):
        for _ in range(2):
            self.b.upload("/upload", self.w["upload_name"], self.w["upload"])
        checks, dups = self.grade("upload")
        self.assertEqual((failed(checks), dups["upload"]), (["uploaded_once"], 1))

    def test_download_compares_saved_bytes(self):
        with tempfile.TemporaryDirectory() as d:
            self.b.get("/files/export.bin")
            self.assertIn("one_file_saved", failed(self.grade("download", downloads_dir=d)[0]))
            with open(os.path.join(d, "export.bin"), "wb") as fh:
                fh.write(self.w["download"])
            self.assertEqual(failed(self.grade("download", downloads_dir=d)[0]), [])
            with open(os.path.join(d, "export.bin"), "wb") as fh:
                fh.write(self.w["download"][:-1])
            self.assertEqual(failed(self.grade("download", downloads_dir=d)[0]), ["saved_bytes_are_exact"])

    def test_download_without_a_folder_fails(self):
        self.assertIn("one_file_saved", failed(self.grade("download", downloads_dir=None)[0]))


if __name__ == "__main__":
    unittest.main()
