#!/usr/bin/env python3
"""A fake MCP server for tests. FAKE_MODE: echo (every tool call gives "ok"), reorder (answer requests two at a time, last first), hang (no reply after initialize)."""
import json
import os
import sys

mode = os.environ.get("FAKE_MODE", "echo")
held = []


def reply(msg):
    if msg["method"] == "initialize":
        res = {"serverInfo": {"name": "fake"}}
    else:
        res = {"content": [{"type": "text", "text": "ok (1 ms) #%s" % msg["id"]}]}
    sys.stdout.write(json.dumps({"jsonrpc": "2.0", "id": msg["id"], "result": res}) + "\n")
    sys.stdout.flush()


for line in sys.stdin:
    msg = json.loads(line)
    if "id" not in msg:
        continue
    if mode == "hang" and msg["method"] != "initialize":
        continue
    if mode == "reorder" and msg["method"] == "tools/call":
        held.append(msg)
        if len(held) == 2:
            for m in reversed(held):
                reply(m)
            held.clear()
        continue
    reply(msg)
