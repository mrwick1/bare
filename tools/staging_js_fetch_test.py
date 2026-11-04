"""Offline fetch-cap regressions: python3 -I tools/staging_js_fetch_test.py."""
import importlib.util
import io
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location("fetch", Path(__file__).with_name("staging_js_fetch.py"))
fetch = importlib.util.module_from_spec(spec)
spec.loader.exec_module(fetch)


class Reply(io.BytesIO):
    status = 200

    def __init__(self, body, **headers):
        super().__init__(body)
        self.headers = headers


class FetchTests(unittest.TestCase):
    def test_exact_and_excess_file_or_total_bytes(self):
        for cap in [0, 1, 100]:
            for delta in [0, 1]:
                for file_cap, room in [(cap, 200), (200, cap)]:
                    with patch.object(fetch, "MAX_FILE", file_cap):
                        response = Reply(b"x" * (cap + delta))
                        if delta:
                            with self.assertRaisesRegex(ValueError, "byte cap"):
                                fetch.read_body(response, room, float("inf"))
                        else:
                            self.assertEqual(fetch.read_body(response, room, float("inf")), b"x" * cap)

    def test_encoding_length_and_time_rejected_before_read(self):
        for headers, deadline in [({"Content-Encoding": "gzip"}, float("inf")), ({"Content-Length": "101"}, float("inf")), ({}, 0)]:
            response = Reply(b"x", **headers)
            with self.assertRaises(ValueError):
                fetch.read_body(response, 100, deadline)
            self.assertEqual(response.tell(), 0)

    def test_multiple_files_never_cross_total_cap(self):
        class Opener:
            def open(self, *args, **kwargs):
                return Reply(b"xx")
        with tempfile.TemporaryDirectory() as out, patch.object(fetch, "MAX_TOTAL", 24), patch.object(fetch.urllib.request, "build_opener", return_value=Opener()), patch("sys.stdout", new=io.StringIO()):
            with self.assertRaisesRegex(ValueError, "byte cap"):
                fetch.main(out)
            self.assertEqual(sum(p.stat().st_size for p in Path(out).iterdir()), 24)

    def test_redirect_does_not_create_another_request(self):
        with self.assertRaisesRegex(ValueError, "redirect refused"):
            fetch.NoRedirect().redirect_request(None, None, 302, "", {}, "https://other.example/")


if __name__ == "__main__":
    unittest.main()
