# 2026-11-21 — Edit constructed stylesheet rules in place

## Before

A CSSOM sheet owns a source string and a live indexed list. Each entry holds either a byte range in the string or an already created rule object. A grouping rule owns its own source string and list. Reading an index can create a rule object. These objects keep their identity across list edits.

On a sheet edit, Bare clones the list, inserts or removes the entry, and builds a new string and list. It copies each raw chunk. It serializes each entry that has a rule object. An insertion also parses the input and parses candidate chunks to find the first rule. For a sheet with N entries and B source bytes, publication takes O(N + B) time and temporary storage. Growing the sheet one rule at a time makes total publication work quadratic.

A constructed sheet edit increments the stylesheet generation. The style plan collects adopted sheet text in tree and sheet order. On a style or layout read that needs a new rule set, the CSS crate parses the sheet, builds selector indexes and keeps source ranges for declaration blocks that it can parse on demand. CSSOM edits do not call this parser directly. The cascade can reuse a rule set when the text, scope and viewport match.

## Other engines

- Blink parses one rule in `CSSStyleSheet::insertRule`, mutates `StyleSheetContents` and shifts the wrapper slots. `WillMutateRules` clears the cached `RuleSet`. `EnsureRuleSet` rebuilds from the stored rules when style needs it. The rule-set diff can record changed rules for invalidation. [CSSStyleSheet](https://raw.githubusercontent.com/chromium/chromium/main/third_party/blink/renderer/core/css/css_style_sheet.cc) · [StyleSheetContents](https://raw.githubusercontent.com/chromium/chromium/main/third_party/blink/renderer/core/css/style_sheet_contents.cc)
- Firefox delegates CSSOM edits to `ServoCSSRuleList`. Stylo parses one insertion in the sheet context and changes the locked `CssRules` vector. It reports the affected rule to the stylist. The stylist records local invalidations where it can and flushes dirty cascade data before styling. [StyleSheet.cpp](https://raw.githubusercontent.com/mozilla-firefox/firefox/main/layout/style/StyleSheet.cpp) · [Servo glue](https://raw.githubusercontent.com/mozilla-firefox/firefox/main/servo/ports/geckolib/glue.rs) · [Stylist](https://raw.githubusercontent.com/servo/stylo/main/style/stylist.rs) · [Invalidation](https://raw.githubusercontent.com/servo/stylo/main/style/invalidation/stylesheets.rs)

## Design

- Edit the span list in place and mark the constructed source dirty. Inserts append bytes to the source buffer. Deletes release the bytes at the tail or leave a gap. Compact the buffer when dead space exceeds the threshold. Normal edits cost O(inserted bytes + shifted entries), plus amortized compaction. The list keeps its compact span entries. The sheet adds a live byte count, a materialized entry count and a dirty flag. A materialized rule keeps its last source span. The style plan assembles the sheet text in O(N + B) when it needs it. This preserves the original order and separators, including with parser recovery.
- Keep a parsed rule per entry and pass the parsed list to the cascade. This also saves parsing when a page reads style after each edit. It needs owned parser data or a stable backing store for the borrowed selector and declaration ranges. It also needs a rule-set input that preserves the single source, scope, layered order and recovery behavior. It adds storage for parsed data on sheets that a script edits but never adopts.
- Cache parsed rules by exact input text. This saves repeated validation on the fixed-rule ramp. It adds lookup and retained storage, and full publication still grows with sheet size. It gives little benefit when every rule has different text. A bounded cache controls its memory cost.

Use in-place edits and defer publication until the style plan. Find the first usable chunk without allocating a list of chunks. Reuse the validation parse when the chunk covers the whole input. Keep synchronous publication for element-backed sheets because scripts read and observe their text nodes. Use the existing serializer when a surviving materialized rule can run script through its declaration getter. Reading an index alone keeps the cascade text stable. The style engine continues to consume the same text input.

## Counter test

The baseline test inserted and deleted 64 and 128 distinct rules on an adopted sheet. The stylesheet had one rule at the start. Inserts parsed 128 and 256 rules and wrote 2,144 and 8,384 entries. Deletes parsed zero rules and wrote 2,080 and 8,256 entries. The cascade build count stayed unchanged during edits. Reading computed style after the insert batch built the new cascade once. The repeated parse in the hot path was CSSOM validation of the inserted text.

With in-place edits, the same batches parse 64 and 128 rules. The edits publish zero entries. The computed style reads write 65 and 129 entries after insertion, then one entry after deletion. The test checks the change in cascade color and the cascade build count.

## Ramp measurements

The final release comparison uses the build inputs from `6187ab08` on both sides. An earlier comparison at `52bf1db5` gave the same trend. The changed build replaces only `browser_cssom.rs`. The builds include the js2 session support from PR #854. The fixtures are the 11 `ctor_*` and `style_*` pages from the journey sweep. A loopback server serves the pages. The driver checks the completion marker and the fixture's result.

The runs use `archMachine` with an AMD Ryzen 9 5900HX, Linux `7.2.15-zen1-1-zen`, Rust 1.96.0, glibc 2.44 and perf 7.3.1. The old engine uses the default session mode. js2 uses `BARE_JS_EXEC=new`. Each cell gives the min–max spread of three separate release processes per build and engine. The percentages compare the minimum counts. Counts use `perf stat -e instructions:u` and `glibc.malloc.tcache_count=65535`.

| Fixture | Old engine, M | js2, M |
| --- | ---: | ---: |
| `ctor_100k` | 1,623.4–1,623.4 → 1,025.0–1,025.1 (-36.9%) | 1,611.0–1,613.9 → 1,003.8–1,003.8 (-37.7%) |
| `ctor_10k` | 169.4–169.5 → 107.9–107.9 (-36.3%) | 166.0–166.1 → 104.8–104.8 (-36.9%) |
| `ctor_1k` | 22.3–22.3 → 16.2–16.2 (-27.3%) | 20.6–20.7 → 14.5–14.6 (-29.6%) |
| `ctor_1m` | 16,075.8–16,221.1 → 10,196.0–10,196.0 (-36.6%) | 15,954.6–16,053.8 → 9,996.6–10,026.6 (-37.3%) |
| `ctor_read_100k` | 2,648.4–2,648.5 → 2,050.7–2,050.7 (-22.6%) | 2,789.4–2,789.5 → 2,184.5–2,190.4 (-21.7%) |
| `ctor_read_10k` | 266.5–266.6 → 206.8–206.8 (-22.4%) | 282.3–283.2 → 222.8–222.9 (-21.1%) |
| `ctor_read_1k` | 32.3–32.3 → 26.1–26.2 (-19.2%) | 32.7–32.8 → 26.4–26.5 (-19.3%) |
| `style_100k` | 2,502.0–2,502.0 → 2,253.5–2,254.9 (-9.9%) | 2,395.6–2,399.4 → 2,143.2–2,144.4 (-10.5%) |
| `style_10k` | 254.1–254.2 → 229.2–229.4 (-9.8%) | 241.9–242.4 → 216.9–217.0 (-10.4%) |
| `style_1k` | 30.7–30.7 → 28.2–28.2 (-8.1%) | 28.3–28.3 → 25.8–25.9 (-8.8%) |
| `style_250k` | 6,205.0–6,240.1 → 5,609.1–5,618.8 (-9.6%) | 5,961.0–5,973.0 → 5,320.5–5,344.8 (-10.7%) |

Plain assignment to `document.adoptedStyleSheets` creates an own property on the frozen js2 build. The original js2 `ctor_*` fixtures therefore measure constructed sheet edits without applying the sheet to the cascade. The same assignment applies the sheet on the old engine. A control calls the prototype setter directly. Both builds and both engines then give the same rule identity, live indexes, detachment and red → blue → red cascade results. The direct-setter `ctor_1m` on js2 also checks that a matching element is styled. Its three-run spread fell from 15.920–16.043 G to 10.030–10.030 G instructions (-37.0% by the minima).

## Cost matrix

`tools/js/cost_matrix.sh -j 4 -g '^examples/(cssom|style)_load ' -J -a` uses the minimum of three instruction counts for each row and allocator. The CSSOM and inline style examples use the old engine. All 16 rows passed. Mean change was −0.276% with glibc and −0.245% with jemalloc. The largest increase was +0.414% with jemalloc.

| Row | glibc change | jemalloc change |
| --- | ---: | ---: |
| `cssom_load adopted 1` | -0.242% | -0.219% |
| `cssom_load adopted 10` | -0.255% | -0.264% |
| `cssom_load computed 1` | +0.379% | +0.392% |
| `cssom_load computed 10` | +0.392% | +0.414% |
| `cssom_load rules 1` | -1.957% | -1.787% |
| `cssom_load rules 10` | -2.104% | -1.971% |
| `cssom_load sheets 1` | -0.307% | -0.267% |
| `cssom_load sheets 10` | -0.344% | -0.300% |
| `style_load attrs 1` | +0.039% | +0.007% |
| `style_load attrs 10` | +0.018% | -0.005% |
| `style_load churn 1` | -0.022% | +0.023% |
| `style_load churn 10` | -0.010% | +0.047% |
| `style_load setget 1` | -0.007% | +0.020% |
| `style_load setget 10` | +0.015% | +0.002% |
| `style_load shorthand 1` | +0.004% | +0.016% |
| `style_load shorthand 10` | -0.008% | -0.025% |

## Verification

- On Linux, `cargo test --locked -p bare --lib cssom` passed 31 tests. The tests cover live rules, rule identity, style reads, source compaction and a style read from a declaration getter.
- `cargo check --locked -p bare` passed with the upstream RegExp build fix from PR #862.
- Raw runs and fixture hashes for both comparisons are under `/data/bare-tmp/r21/cssom-evidence` on `archMachine`.
- Local coverage excludes Windows, the full suite, live journeys, wall time and peak RSS.
