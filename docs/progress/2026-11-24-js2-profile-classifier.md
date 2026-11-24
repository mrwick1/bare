# 2026-11-24 — Profile classifier rules for js2

## Problem

`tools/bench/classify.py` had no rule for `bare_js2::`. A js2 sample went to the first browser frame above it. On substack-home, 61% of the js2 samples showed as "Network + TLS + HTTP" (see [the item 10 profile](2026-11-24-js2-react-substack-profile.md)). `tools/bench/jscat.py` counted js2 samples as JS, but did not see the js2 collector.

## Change

- `classify.py`: the "JS engine" rule matches `bare_js` and `bare_js2`. The collector rule also matches `bare_js2::gc::collect`. The parse rule also matches the js2 modules `parse`, `lex`, `ast`, `parse_ahead` and `compile*`.
- `jscat.py`: the same collector rule. The "Parse and compile" kind also matches `bare_js2::` parse modules and `bare_js2::compile`.

## Check

- A synthetic `perf script` file with 3 js2 samples under `bare::browser_fetch` frames (run, `gc::collect`, `parse::Parser`).
  - Before: classify.py gave 100% "Network + TLS + HTTP"; jscat.py gave no collector row.
  - After: classify.py gave 33.3% each to JS run, parsing and collector; jscat.py gave 33.3% collector.
- The OLD substack-home profile (`/data/bare-tmp/i10/prof-substack-home-auto/run1.txt`) gives the same output on main and on this branch.

## Not tested

- A real js2 profile. No js2 `perf script` file is kept. The next js2 profile (crates-io-home, mdn-reference) uses these rules.
