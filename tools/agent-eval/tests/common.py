"""Test helpers: the import path and a tiny HTTP client that acts like a browser for the fixture (cookies, no redirects hidden)."""
import http.cookiejar
import json
import os
import sys
import urllib.parse
import urllib.request

sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), ".."))

import fixture as fx  # noqa: E402
import tasks as tk  # noqa: E402


class Browser:
    def __init__(self, base):
        self.base = base
        self.jar = http.cookiejar.CookieJar()
        self.opener = urllib.request.build_opener(urllib.request.HTTPCookieProcessor(self.jar))

    def get(self, path):
        with self.opener.open(self.base + path) as r:
            return r.read().decode("utf-8", "replace")

    def post(self, path, **form):
        data = urllib.parse.urlencode(form).encode()
        with self.opener.open(urllib.request.Request(self.base + path, data=data)) as r:
            return r.read().decode("utf-8", "replace")

    def post_json(self, path, obj):
        req = urllib.request.Request(self.base + path, data=json.dumps(obj).encode(), headers={"Content-Type": "application/json"})
        with self.opener.open(req) as r:
            return json.loads(r.read())

    def upload(self, path, name, data):
        b = "XBOUNDARYX"
        body = (f'--{b}\r\nContent-Disposition: form-data; name="file"; filename="{name}"\r\nContent-Type: application/octet-stream\r\n\r\n').encode() + data + f"\r\n--{b}--\r\n".encode()
        req = urllib.request.Request(self.base + path, data=body, headers={"Content-Type": f"multipart/form-data; boundary={b}"})
        with self.opener.open(req) as r:
            return r.read().decode()

    def login(self, user, world, nxt="/account"):
        u = world["users"][user]
        return self.post("/login", email=u["email"], password=u["password"], next=nxt)


def ctx_for(f, task_id, answer=None, downloads_dir=None):
    return tk.Ctx(world=f.world, base=f.base, state=f.state(), answer=answer, downloads_dir=downloads_dir, upload_path="/x/" + f.world["upload_name"])


def failed(checks):
    return sorted(c["name"] for c in checks if not c["ok"])
