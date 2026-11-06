#!/bin/sh
# Compare the DOM-side APIs of bare (`AbortController`, `AbortSignal`, `structuredClone`, `btoa`, `atob`) with headless Chrome on
# tools/fixtures/js/dom-cases.txt. The `.expected` file is a Chrome recording (`--update` records it again: it needs
# google-chrome-stable). Only our own tiny programs run in Chrome. Never run a fetched bundle with this tool.
#   tools/js/dom_check.sh [--update]
set -eu
here=$(dirname "$0")
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
cargo build --release -q -p bare --example dom_check
cases=$here/../fixtures/js/dom-cases.txt
if [ "${1:-}" = "--update" ]; then node "$here/../ui/chrome_cases.mjs" "$cases" > "$here/../fixtures/js/dom-cases.expected"; fi
target/release/examples/dom_check "$cases" > "$tmp/m"
if diff "$here/../fixtures/js/dom-cases.expected" "$tmp/m" > "$tmp/d"; then
  echo "dom cases: $(wc -l < "$tmp/m") match"
else
  echo "dom cases: DIFFERENCES (< chrome, > bare)"; cat "$tmp/d"; exit 1
fi
