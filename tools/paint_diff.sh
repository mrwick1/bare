#!/bin/sh
# Compare `bare --paint` with headless Chrome for one fixture (docs/resources.md, "Paint and pixel capture").
# Usage: tools/paint_diff.sh FIXTURE.html OUT_DIR [WIDTH [HEIGHT]]
# Writes OUT_DIR/NAME.chrome.png, NAME.bare.png and NAME.diff.png, and prints the pixel difference.
# Chrome gets `* { font-family: 'Noto Sans Mono' !important }` after the doctype: the font that layout measures.
set -e
f=$1; out=$2; w=${3:-400}; h=${4:-800}
name=$(basename "$f" .html)
mkdir -p "$out"
sed '1a<style>* { font-family: "Noto Sans Mono" !important }</style>' "$f" > "$out/$name.chrome.html"
google-chrome-stable --headless=new --disable-gpu --hide-scrollbars --force-device-scale-factor=1 \
  --window-size="$w,$h" --screenshot="$out/$name.chrome.png" "$out/$name.chrome.html" >/dev/null 2>&1
target/release/bare --paint="$out/$name.bare.png" --width="$w" "$f"
target/release/examples/diff diff "$out/$name.chrome.png" "$out/$name.bare.png" "$out/$name.diff.png"
