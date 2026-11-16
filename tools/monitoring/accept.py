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
            env = dict(os.environ, BARE_OBSERVE=mode, BARE_OBSERVE_FILE=str(capture))
            command = ["perf", "stat", "-x", ";", "-e", "instructions:u", "-o", str(perf),
                       str(args.binary.resolve()),
                       "--quiet", "--repeat", "20", str(steps)]
            with (args.artifacts / (label + ".stderr")).open("wb") as err, (args.artifacts / (label + ".stdout")).open("wb") as out:
                child = subprocess.Popen(command, env=env, stdout=out, stderr=err)
                _, status, usage = os.wait4(child.pid, 0)
                child.returncode = os.waitstatus_to_exitcode(status)
            if child.returncode:
                raise RuntimeError(f"{label}: exit {child.returncode}; see artifacts")
            records = [json.loads(line) for line in capture.read_text().splitlines()] if capture.exists() else []
            if mode != "off":
                assert records and records[-1]["event"] == "collector_status"
            instructions = next(int(line.split(";")[0]) for line in perf.read_text().splitlines() if ";instructions:u;" in line)
            results.append(dict(mode=mode, run=run, instructions=instructions,
                                peak_rss_kib=usage.ru_maxrss, records=len(records),
                                drops=records[-1]["fields"]["dropped"] if records else 0))
finally:
    server.shutdown()
    thread.join()
    server.server_close()
metadata = dict(commit=subprocess.check_output(["git", "rev-parse", "HEAD"], text=True).strip(),
                machine=os.uname().nodename, body=body.decode(), repeat=20, results=results)
(args.artifacts / "results.json").write_text(json.dumps(metadata, indent=2) + "\n")
print(json.dumps(metadata, indent=2))
