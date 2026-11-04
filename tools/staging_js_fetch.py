#!/usr/bin/env python3
"""Fetch the login-route JS chunks named by the saved login page. Stdlib only.

  python3 -I tools/staging_js_fetch.py OUT_DIR

Public GETs only, no credentials. Caps: 24 GETs, 8 MiB per file, 24 MiB total, 120 s.
Prints URL, status, decoded bytes, SHA256 and whether the hash matches the manifest.
"""
import gzip, hashlib, json, os, re, sys, time, urllib.request

HOST = "https://staging.app.8vox.ai"
FIX = os.path.join(os.path.dirname(os.path.abspath(__file__)), "fixtures", "staging")
MAX_GETS, MAX_FILE, MAX_TOTAL, MAX_SECS = 24, 8 << 20, 24 << 20, 120

html = open(os.path.join(FIX, "login.html"), encoding="utf-8").read()
manifest = json.load(open(os.path.join(FIX, "manifest-2026-11-04.json")))["resources"]
paths = re.findall(r'<script src="(/_next/[^"]+)"(?! noModule)', html)
paths = [p for p in dict.fromkeys(paths) if p in manifest]
paths = [p for p in paths if f'src="{p}" noModule' not in html]
out = sys.argv[1]
os.makedirs(out, exist_ok=True)
t0, total = time.time(), 0
for n, p in enumerate(paths):
    if n >= MAX_GETS or total > MAX_TOTAL or time.time() - t0 > MAX_SECS:
        print("cap reached, stopping"); break
    req = urllib.request.Request(HOST + p, headers={"Accept-Encoding": "gzip", "User-Agent": "bare-research"})
    with urllib.request.urlopen(req, timeout=15) as r:
        body = r.read(MAX_FILE + 1)
        if r.headers.get("Content-Encoding") == "gzip":
            body = gzip.decompress(body)
        status = r.status
    assert len(body) <= MAX_FILE
    total += len(body)
    h = hashlib.sha256(body).hexdigest()
    open(os.path.join(out, p.rsplit("/", 1)[1]), "wb").write(body)
    print(f"{status} {len(body):>7} {h} manifest={'match' if h == manifest[p]['sha256'] else 'DIFF'} {p}")
print("gets", n + 1, "bytes", total, "secs", round(time.time() - t0, 1))
