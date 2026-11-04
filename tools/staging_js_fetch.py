#!/usr/bin/env python3
"""Fetch public login scripts from the saved fixture: staging_js_fetch.py OUT_DIR.

No credentials or redirects. Caps: 24 GETs, 8 MiB per file, 24 MiB total, 120 s.
The elapsed limit is checked between blocking operations; one DNS/socket wait can exceed it.
"""
import hashlib
import io
import json
import os
import re
import sys
import time
import urllib.request

HOST = "https://staging.app.8vox.ai"
FIX = os.path.join(os.path.dirname(os.path.abspath(__file__)), "fixtures", "staging")
MAX_GETS, MAX_FILE, MAX_TOTAL, MAX_SECS = 24, 8 << 20, 24 << 20, 120


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        raise ValueError("redirect refused")


def read_body(response, room, deadline):
    if response.headers.get("Content-Encoding", "identity").strip().lower() != "identity":
        raise ValueError("encoded response refused")
    cap = min(MAX_FILE, room)
    length = response.headers.get("Content-Length")
    if length is not None and int(length) > cap:
        raise ValueError("byte cap exceeded")
    body = io.BytesIO()
    while True:
        if time.monotonic() >= deadline:
            raise ValueError("time cap exceeded")
        piece = response.read1(min(65536, cap - body.tell() + 1))
        if not piece:
            return body.getvalue()
        if body.tell() + len(piece) > cap:
            raise ValueError("byte cap exceeded")
        body.write(piece)


def main(out):
    with open(os.path.join(FIX, "login.html"), encoding="utf-8") as f:
        html = f.read()
    with open(os.path.join(FIX, "manifest-2026-11-04.json")) as f:
        manifest = json.load(f)["resources"]
    paths = re.findall(r'<script src="(/_next/[^"]+)"', html)
    paths = [p for p in dict.fromkeys(paths) if p in manifest and f'src="{p}" noModule' not in html]
    if len(paths) > MAX_GETS:
        raise ValueError("GET cap exceeded")
    os.makedirs(out, exist_ok=True)
    opener = urllib.request.build_opener(NoRedirect())
    start, total, gets = time.monotonic(), 0, 0
    deadline = start + MAX_SECS
    for path in paths:
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            raise ValueError("time cap exceeded")
        req = urllib.request.Request(HOST + path, headers={"Accept-Encoding": "identity", "User-Agent": "bare-research"})
        gets += 1
        with opener.open(req, timeout=min(15, remaining)) as response:
            if response.status != 200:
                raise ValueError("non-200 response")
            body = read_body(response, MAX_TOTAL - total, deadline)
        total += len(body)
        digest = hashlib.sha256(body).hexdigest()
        with open(os.path.join(out, path.rsplit("/", 1)[1]), "wb") as f:
            f.write(body)
        print(f"200 {len(body):>7} {digest} manifest={'match' if digest == manifest[path]['sha256'] else 'DIFF'} {path}")
    print("gets", gets, "bytes", total, "secs", round(time.monotonic() - start, 1))


if __name__ == "__main__":
    if len(sys.argv) != 2:
        sys.exit("usage: staging_js_fetch.py OUT_DIR")
    try:
        main(sys.argv[1])
    except (OSError, ValueError) as error:
        sys.exit(str(error))
