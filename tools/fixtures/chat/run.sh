#!/bin/sh
# Start the chat server of the fixture (server.mjs). The packages are not in the repository: they install in a work directory.
#   tools/fixtures/chat/run.sh [PORT]        (default 8790; PING_MS sets the ping interval, default 5000)
#   bare-drive tools/fixtures/chat/chat.steps        (Socket.IO: polling, then the upgrade to WebSocket)
#   bare-drive tools/fixtures/chat/plain.steps       (the native WebSocket on /ws)
set -eu
here=$(dirname "$(realpath "$0")")
work=${TMPDIR:-/data/bare-tmp/tmp}/bare-chat
mkdir -p "$work"
cp "$here/package.json" "$work/"
cd "$work"
[ -d node_modules ] || npm install --no-audit --no-fund >/dev/null
rm -rf public && cp -r "$here/public" "$here/server.mjs" .
exec node server.mjs "${1:-8790}"
