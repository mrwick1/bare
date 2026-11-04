#!/bin/sh
# First HTTPS check for bare. It fails until S1 topic 4 is done. It is not part of `cargo test`.
# Contacts https://staging.app.8vox.ai with two requests. Run from the repo root.
set -eu
want=$(python3 -I -c 'import json;print(json.load(open("tools/fixtures/staging/manifest-2026-11-04.json"))["pages"]["/login"]["sha256"])')
cargo build --release -q -p bare
got=$(./target/release/bare --source https://staging.app.8vox.ai/login | head -c -1 | sha256sum | cut -d' ' -f1)
[ "$got" = "$want" ] && echo "ok /login sha256 $got" || { echo "FAIL /login: got $got want $want"; exit 1; }
