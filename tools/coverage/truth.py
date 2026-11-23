#!/usr/bin/env python3
"""Write Chrome's document.body.innerText of each coverage fixture to <name>.truth.txt.

usage: truth.py FLASHBROWSE_BIN
Chrome is driven through the flashbrowse MCP server (`flashbrowse mcp`). Run it when a fixture changes.
"""
import functools, http.server, json, pathlib, subprocess, sys, threading

DIR = pathlib.Path(__file__).resolve().parent.parent / "fixtures" / "coverage"

class Quiet(http.server.SimpleHTTPRequestHandler):
    def log_message(self, *a):
        pass


srv = http.server.ThreadingHTTPServer(("127.0.0.1", 0), functools.partial(Quiet, directory=str(DIR)))
threading.Thread(target=srv.serve_forever, daemon=True).start()

p = subprocess.Popen([sys.argv[1], "mcp"], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True, bufsize=1)
n = 0


def req(method, params):
    global n
    n += 1
    p.stdin.write(json.dumps({"jsonrpc": "2.0", "id": n, "method": method, "params": params}) + "\n")
    while True:
        m = json.loads(p.stdout.readline())
        if m.get("id") == n:
            return m


def call(tool, **args):
    r = req("tools/call", {"name": tool, "arguments": args}).get("result") or {}
    return "\n".join(c.get("text", "") for c in r.get("content", []) if c.get("type") == "text")


req("initialize", {"protocolVersion": "2024-11-05", "capabilities": {}, "clientInfo": {"name": "truth", "version": "1"}})
p.stdin.write(json.dumps({"jsonrpc": "2.0", "method": "notifications/initialized"}) + "\n")
for f in sorted(DIR.glob("*.html")):
    call("browser_open", url=f"http://127.0.0.1:{srv.server_port}/{f.name}")
    text = call("browser_evaluate", expression="document.body.innerText")
    if text.startswith('"'):
        text = json.loads(text)
    if not text.strip():
        sys.exit(f"empty truth for {f.name}")
    f.with_suffix(".truth.txt").write_text(text + "\n", encoding="utf-8")
    print(f.name, len(text))
p.stdin.close()
