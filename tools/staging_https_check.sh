#!/bin/sh
# First HTTPS check for bare. It fails until S1 topic 4 is done. Not part of cargo test.
# Contacts staging once. Run from the repo root. Refresh the manifest after a deploy.
set -eu
cargo build --release -q -p bare
body=$(mktemp)
trap 'rm -f "$body"' EXIT HUP INT TERM
./target/release/bare --source https://staging.app.8vox.ai/login > "$body"
python3 -I - "$body" <<'PYTHON'
import hashlib, json, sys
from pathlib import Path
want = json.loads(Path("tools/fixtures/staging/manifest-2026-11-04.json").read_text())["pages"]["/login"]["sha256"]
body = Path(sys.argv[1]).read_bytes()
if not body.endswith(b"\n"):
    sys.exit("FAIL: source output has no final newline")
got = hashlib.sha256(body[:-1]).hexdigest()
if got != want:
    sys.exit(f"FAIL /login: got {got} want {want}; check for a staging deploy")
print(f"ok /login sha256 {got}")
PYTHON
