#!/usr/bin/env python3
"""Measure a committed profiling binary against a bounded loopback journey."""
import argparse
import http.server
import json
import os
from pathlib import Path
import subprocess
import threading

parser = argparse.ArgumentParser()
parser.add_argument("binary", type=Path)
parser.add_argument("artifacts", type=Path)
args = parser.parse_args()
args.artifacts.mkdir(parents=True, exist_ok=False)
body = b"<body><div id='a'>fixture</div><script>var x=0;for(var i=0;i<100000;i++)x+=i;setTimeout(function(){x++;},0);</script></body>"

class Handler(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        self.send_response(200)
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Content-Type", "text/html")
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *_):
        pass

server = http.server.HTTPServer(("127.0.0.1", 0), Handler)
thread = threading.Thread(target=server.serve_forever)
thread.start()
steps = args.artifacts / "journey.steps"
steps.write_text(f'open http://127.0.0.1:{server.server_port}/\nexpect css="#a" visible\n')
results = []
try:
    for run in range(3):
        for mode in ["off", "metrics", "trace"]:
            label = f"{mode}-{run}"
            capture = args.artifacts / (label + ".jsonl")
            perf = args.artifacts / (label + ".perf")
            rss = args.artifacts / (label + ".rss")
            env = dict(os.environ, BARE_OBSERVE=mode, BARE_OBSERVE_FILE=str(capture))
            command = ["perf", "stat", "-x", ";", "-e", "instructions:u", "-o", str(perf),
                       "/usr/bin/time", "-f", "%M", "-o", str(rss), str(args.binary.resolve()),
                       "--quiet", "--repeat", "20", str(steps)]
            out = subprocess.run(command, env=env, capture_output=True, timeout=120)
            (args.artifacts / (label + ".stderr")).write_bytes(out.stderr)
            (args.artifacts / (label + ".stdout")).write_bytes(out.stdout)
            if out.returncode:
                raise RuntimeError(f"{label}: exit {out.returncode}; see artifacts")
            records = [json.loads(line) for line in capture.read_text().splitlines()] if capture.exists() else []
            if mode != "off":
                assert records and records[-1]["event"] == "collector_status"
            instructions = next(int(line.split(";")[0]) for line in perf.read_text().splitlines() if ";instructions:u;" in line)
            results.append(dict(mode=mode, run=run, instructions=instructions,
                                peak_rss_kib=int(rss.read_text()), records=len(records),
                                drops=records[-1]["fields"]["dropped"] if records else 0))
finally:
    server.shutdown()
    thread.join()
    server.server_close()
metadata = dict(commit=subprocess.check_output(["git", "rev-parse", "HEAD"], text=True).strip(),
                machine=os.uname().nodename, body=body.decode(), repeat=20, results=results)
(args.artifacts / "results.json").write_text(json.dumps(metadata, indent=2) + "\n")
print(json.dumps(metadata, indent=2))
