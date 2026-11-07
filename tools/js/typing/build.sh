#!/bin/sh
# Build the page that `crates/bare/examples/type_load.rs` types into: a React 19 production bundle of a controlled form
# (N inputs, 2 textareas with counters, a progress bar and a total that re-render for each key, and Radix select, checkbox,
# switch, radio group, tabs and accordion). The bundle is 335 KB, so it is not in the repository: make it here.
#   tools/js/typing/build.sh OUTDIR [N] [flags]    N: number of inputs (60). flags: nolabel nochoice noradix
#   cargo run --release -p bare --example type_load -- OUTDIR '#a1' 20     (the textarea; '#f0' is the first input)
#   node tools/ui/chrome_type.mjs OUTDIR '#a1' 100                         (the same page in headless Chrome)
# The Radix versions are the ones of a Next.js 14 page. Newer `react-collection` has `class extends Map`, which bare does not run
# yet. esbuild targets es2020 because bare does not run class static blocks.
set -eu
out=$(realpath -m "$1"); n=${2:-60}; shift 2 2>/dev/null || shift $#
here=$(dirname "$(realpath "$0")")
work=${TMPDIR:-/data/bare-tmp/tmp}/bare-typing-build
mkdir -p "$work" "$out"
cp "$here/package.json" "$here/app.jsx" "$work/"
cd "$work"
[ -d node_modules ] || npm install --legacy-peer-deps --no-audit --no-fund >/dev/null
flags=" $* "
flag() { case "$flags" in *" $1 "*) echo true;; *) echo false;; esac; }
./node_modules/.bin/esbuild app.jsx --bundle --minify --format=iife --target=es2020 \
  --define:process.env.NODE_ENV='"production"' --define:__N="$n" \
  --define:__NOLABEL="$(flag nolabel)" --define:__NOCHOICE="$(flag nochoice)" --define:__NORADIX="$(flag noradix)" \
  --outfile="$out/app.js"
cp "$here/style.css" "$out/style.css"
printf '%s' '<!doctype html><html><head><meta charset="utf-8"><title>wizard</title><link rel="stylesheet" href="style.css"></head><body><div id="root"></div><script src="app.js"></script></body></html>' > "$out/index.html"
