# Design: where the Wikipedia journeys spend their instructions (task R2-B)

Status: proposal. The owner reviews this document before any code is built. No product code changed. Written on 2026-11-14 against main `28a8fbc2` (the code of `wikipedia-article` and `wikipedia-search` is the same on later main commits: only docs merged since).

Journeys: `wikipedia-article` (open a long article, click a link, open a second article, go back by address) and `wikipedia-search` (open the article, type a query, Enter). Together they are 15.1 G of the 95.4 G instructions of the 47 journeys (15.8%). With `react-docs`, `rust-book-search` and `nodejs-org-home` (task R2-A) they are 57%.

Rules for numbers: "measured" means I ran it (command and file named). "Estimate" means I computed it from measured numbers (the arithmetic is shown). "Hypothesis" means I believe it and did not check it.

## 0. Summary

1. **The hypothesis of round 1 was wrong for these two journeys.** The final benchmark said the heavy journeys "spend their instructions in page JS execution". Measured: page JS (execution, parse, regex) is 33% of `wikipedia-article` and 35% of `wikipedia-search`. Style, layout and one named-property list are 51% and 48%.
2. **One finding is about the benchmark, not the product.** The replay driver runs `Session::settle_pictures` before every step and at the end (`crates/drive/src/run.rs:485`). It ticks the page until all pictures are decoded. That loop is 74% of `wikipedia-article` and 56% of `wikipedia-search`, and 52.8% of all 47 journeys (50.3 G of 95.4 G). With the loop off, all 47 journeys still pass. Both the baseline and main pay the loop, so the ratio of criterion 2 hardly moves (0.907 with it, 0.893 without it). The loop is not a way to reach 80%. It decides which work counts. The owner must know this before reading any journey share (section 4.1).
3. **Four kinds of work that the design does not need** (sections 4.2 to 4.5), each measured:
   - Window named properties: a list of every `id` and `name` of the page is built again, from the whole DOM, for each miss on a global name. 54 and 40 builds of about 15 k nodes. 11.5% and 10.7% of the journeys. Only one name is needed each time.
   - Style: the whole document (about 10 k elements) is styled again 8 times for one load of the long article. 4 of them follow a sheet that a script appended, 2 follow a restyle that gave up, 2 are frame requests. Plus 3 "motion" passes. Style is 31% and 25%.
   - Layout: 17 and 19 layouts of about 9.4 k boxes. The 10 "kept" layouts of each journey reused 0 fragments. Layout is 8.8% and 11.9%.
   - One hover change restyled 90,232 elements (9 times the document): one call, 865 M instructions, 10.8% of `wikipedia-article`. Nested roots are styled again and again.
4. **Recommendation** (section 6): build, in this order, W1 (answer a window name with one walk and no list), H (skip a restyle root that an earlier root covers), then S-A (one retained cascade and layout, updated by damage marks, for every caller). Expected: `wikipedia-article` -38%, `wikipedia-search` -33%, the two together -5.4 G, which moves criterion 2 from 90.7% to about 85.5%. If the other four Wikipedia journeys gain 25 to 35% (hypothesis: same page shape), the total is 82 to 83%. The Wikipedia family alone cannot reach 80%.

## 1. Method and inputs

- Machine: archMachine, 16 cores, shared (load 1 to 3). Instruction counts hold under load; wall time is not used.
- Binary: `cargo build --release -p bare-drive` of main `28a8fbc2` with `CARGO_PROFILE_RELEASE_DEBUG=line-tables-only` (symbols, no strip). Profiles use a second build with `-C force-frame-pointers=yes` (it costs 2.6% more instructions: 7,982 M against 7,776 M, so shares apply, absolute numbers come from the first build).
- Corpus `/data/bare-tmp/corpus/virtual`, virtual clock: `tools/bench/journeys.sh run --clock virtual --runs 3 --pss --only wikipedia-`. Run 0 of `wikipedia-search` had 2 replay misses (a `POST .../ins-502b/v2/events`): the request depends on timing. Runs 1 to 3 had none. A run that prints these misses with `BARE_FORCE_STATS` set can show the same 2 misses. I count only runs 1 to 3.
- Split by subsystem: `perf record -e instructions:u -c 500000 --call-graph fp --user-callchains`, a fixed period (16 k samples per run, so no throttle), 3 runs per journey. A sample goes to the subsystem of the innermost frame in a `bare_*` crate. Frames of `core`, `alloc`, `std` and `libc` (`malloc`, `memcpy`) are skipped, so their cost goes to the caller. A frame of `bare_html::dom` (`Document::attr`) is skipped too, so a matcher that reads attributes is style. A style sample under `Dom::query`, `matches_selector` or `closest` is "selector API". The sampling error of a share is below 0.4 points. Run-to-run spread of every share is 0.5 points or less (3 runs). The first run of `wikipedia-search` had 3% fewer samples (the timing miss).
- Counts: temporary counters in `/data/bare-tmp/r2/R2-B/counters-final.patch` (with `cnt.rs.txt`), switched by `BARE_FORCE_STATS=1` (`crates/bare/src/force.rs`, which already exists). Not in the repository. Call counts of host functions come from a run with ASLR off (`setarch -R`) and `addr2line`.
- The no-settle runs use `nosettle.patch`: `BARE_NO_SETTLE=1` skips `settle_pictures`. The baseline tree (`bench-base-vclock`, copied to `/data/bare-tmp/r2/R2-B/base-src`) has the same hook. Both builds: `-p bare-drive`, 2 runs per journey, all 47 journeys.
- Memory: `valgrind --tool=massif` of `wikipedia-article` (one run; it is slow) and peak RSS from the journey runner.

## 2. Measured split (settled journeys, as the benchmark runs them)

Shares of instructions, mean of 3 profile runs (min to max). "M" is the share of the clean build: 7,776 M for the article, 7,338 M for the search.

| Subsystem | `wikipedia-article` | M | `wikipedia-search` | M |
|---|---:|---:|---:|---:|
| Style: cascade, restyle, rule index (`bare_css`, `browser_computed`) | 30.7% (30.7-30.9) | 2,390 | 25.2% (25.0-25.5) | 1,850 |
| JS execution (interpreter, heap, builtins) | 22.5% (22.5-22.6) | 1,750 | 22.3% (22.0-22.4) | 1,640 |
| Window named properties (`browser_frames::build`) | 11.5% (11.5-11.5) | 894 | 10.7% (10.7-10.8) | 785 |
| Layout (`bare_layout`, geometry glue) | 8.8% (8.8-8.8) | 684 | 11.9% (11.7-11.9) | 873 |
| JS parse (lexer, parser) | 8.4% (8.3-8.4) | 653 | 10.6% (10.4-10.8) | 778 |
| DOM bindings (host functions, motion walk) | 3.7% | 288 | 4.0% | 294 |
| Selector API (`querySelector*`, `matches`) | 2.7% | 210 | 2.8% | 205 |
| HTML parse | 2.4% | 187 | 2.7% | 198 |
| CSS parse | 2.3% | 179 | 2.2% | 161 |
| JS regexp | 1.8% | 140 | 1.8% | 132 |
| Loader (`feed_styles`, `flatten`) | 1.2% | 93 | 1.3% | 95 |
| Network replay and decode (`Decoded::read_all`) | 1.1% | 86 | 1.1% | 81 |
| Image decode | 0.9% | 70 | 1.7% | 125 |
| Other (DOM lookups, drive, glue) | 1.7% | 132 | 1.7% | 125 |
| Paint | 0.0% | 2 | 0.0% | 3 |
| GC (`Heap::collect`, inside JS execution) | 0.3% | 27 | not split | |

Reading the table:

- Paint is free (the picture-on-demand step of round 1 holds). GC is small. Network replay is 1.1%.
- JS parse is mostly the second parse of lazy functions that run: `parse_lazy_function` is 5.6% of the article (inclusive). Task R2-D owns that. I do not design it here.
- Page JS is 6 to 8 big scripts (1.9 MB of source in the article, 2.4 MB in the search), 294 k and 267 k script function calls, and 40.7 k and 42.0 k host calls. The host calls are cheap accessors: `node_type` 13.0 k, `owner_document` 5.8 k, `add_event_listener` 2.6 k, `document_element` 2.1 k, `namespace_uri` 1.9 k, `parent_node` 1.7 k, `matches_selector` 1.5 k, `set_attribute` 1.2 k, `getElementsByClassName` 1.0 k, `getElementsByTagName` 0.9 k. The top natives are `Function.prototype.call` 23.4 k, `Array.prototype.push` 12.3 k, `Object.prototype.toString` 6.6 k, `apply` 5.9 k, `String.prototype.match` 5.6 k, `RegExp.prototype.test` 5.4 k. R2-A owns the JS side. Nothing in the DOM binding layer is large.

### 2.1 Memory

Peak RSS from the runner (median of 2 runs): 60.3 MiB (article) and 62.8 MiB (search). An empty page is 15.9 MiB. `massif` of the article (one run): the heap peaks at 50.6 MB near the end. Shares of the peak: JS object table (`Vec<bare_js::heap::Object>`) 13.9 MB (27.6%), JS syntax trees (`Expr`, `Stmt`, `Program`) about 5.3 MB (10.5%), DOM attributes (`set_attr_atom`) 3.5 MB (7.0%), layout fragments 4.0 MB (8%), decoded pictures 2.0 MB (4.0%), computed styles 1.4 MB (2.8%), the named-property map 0.7 MB. 681 small sites make 8.5 MB (17%). The peak RSS of the article without the settle loop is 33.0 MiB (60.3 MiB with it), so 27 MiB comes from the JS that the loop runs.

## 3. Counts of the dominant operations

Counters of one run each (they are deterministic on the virtual clock). `FS` lines come from `force.rs`.

| Operation | `wikipedia-article` | `wikipedia-search` |
|---|---:|---:|
| Page loads in the journey | 3 (article 9,970 elements, second article 1,300, article again) | 2 (article, then Alan Turing, about 10 k) |
| Style passes that styled the document ("compute") | 16 passes, 132,869 elements, 1,319 M | 15 passes, 148,267 elements, 1,560 M |
| Rule-set builds ("rules": plan, sheet text, index) | 10, 246 M | 8, 204 M |
| Restyle with marks (`restyle_frame`) | 1 call, 90,232 elements, 865 M | 0 |
| Style passes answered from the cache ("hit") | 17 | 27 |
| Layout passes | 17 (7 full, 10 "kept"), 639 M | 18 (8 full, 10 "kept"), 815 M |
| Fragments that "kept" layouts reused | 0 of 10 passes | 0 of 10 passes |
| Window name list builds, nodes walked | 54, 839 k (16 names asked) | 40, 773 k (18 names asked) |
| `querySelector` + `querySelectorAll` calls, nodes walked | 392, 7.0 M | 342, 6.6 M |
| `getElementById` calls (each a tree walk) | 61 | 58 |
| CSS text added to a rule set (`add_in_layer`) | 185 calls, 3.38 MB | 188 calls, 3.22 MB |
| JS source parsed (lazy lane) | 6 scripts, 1.92 MB (+ 31 small) | 8 scripts, 2.44 MB (+ 25 small) |
| Candidates a full cascade tests for each element | 36 (2,534 rules; 358 k candidates for 9.3 k elements) | 41 (2,335 rules) |
| Cost of one styled element in a full cascade | about 10,000 instructions (100 M for 9.97 k) | the same |
| Cost of one laid-out box | about 4,800 instructions (45 M for 9.47 k) | the same |

The names that a page asks for with the window named-property list are feature probes of globals that do not exist: `RLCONF`, `NORLQ`, `documentMode`, `vitest`, `Redux`, `pg`, `VisualEditorSupportCheck`, `onbeforeCollapse` (10 asks) and so on. 16 and 18 distinct names, 54 and 40 asks.

The `WHY` counters name the cause of each full style pass of the article (first load, 8 passes): 4 follow a new rule set (a script or `mw.loader` appended sheets, so `Built` was reset and the retained cascade is lost), 2 are a restyle that returned `None` (marks that touch a sheet or an unbounded root, `restyle_marks`), and 2 are frame requests with `keep = true`, which never use a restyle (`browser_computed.rs`, `cascade_inner`: `let read = want.is_none() && !keep`).

## 4. What a journey never needs

### 4.1 The settle loop (benchmark policy; measured)

`Session::settle_pictures` runs only when a replay tape is set. It ticks the page until `images_pending()` is false, or 5 s of virtual time pass, before each step and once at the end. Each call ran 25 to 45 ticks and about 50 ms of virtual time. Everything the page does in that time counts: timers, idle callbacks, animation frames, motion passes, the scripts of `load.php` (their replies fall due while the loop waits).

A/B on the same binary and corpus, 2 runs, all 47 journeys pass in both modes, 0 misses (`wikipedia-search`: up to 2 timing misses):

| Journey | With the loop (M) | Without (M) | Without / with |
|---|---:|---:|---:|
| `wikipedia-article` | 7,785 | 1,995 | 25.6% |
| `wikipedia-search` | 7,337 | 3,242 | 44.2% |
| `react-docs` | 16,948 | 127 | 0.7% |
| `mdn-reference` | 7,179 | 338 | 4.7% |
| `rust-book-search`, `nodejs-org-home`, `github-docs-home` | 16,319, 5,917, 4,900 | the same | 100% |
| all 47 journeys | 95,376 | 45,037 | 47.2% |
| top 5 journeys of round 1 | 54,305 | 27,600 | 50.8% |

Baseline `f6415364` with the same hook (same corpus, 2 runs): 105,194 M with the loop, 50,448 M without. So criterion 2 is 90.7% with the loop (this binary, 2 runs; the estimate of PR #522 was 90.1%) and 89.3% without it. The loop is not a lever for the ratio: both sides pay it.

What it means:

- A real test run ends when the last step passes. The steps of both journeys pass without the loop. A page that the user keeps open runs the JS of its scripts anyway, so the loop is not "pure waste": it is a rule that says "every journey runs the page JS to rest". It makes the numbers repeatable, and it makes page-JS work 3 to 100 times heavier in the weight of the benchmark (`react-docs` is 99.3% loop).
- Owner decision (not a design): keep the rule, or bound it (for example: wait only for pictures that were started, and run no animation frames or idle callbacks while waiting). A change must be made on both sides of the comparison, and the baseline must be measured again. Until then, work that a design removes from the settled phase counts in full. All savings in section 6 are on the settled benchmark.
- For R2-A: in the no-settle runs `react-docs` falls from 16.9 G to 0.13 G. Its cost is the page JS that the loop runs, not the steps.

### 4.2 Window named properties: a list for one name

`window.RLCONF` (a global that does not exist) misses on the window object. The host (`browser_frames::fill`, `crates/bare/src/browser_frames.rs:343`) checks `State::stale(doc, true)`. The check is true after any document change (`self.full != doc.revision()`) or after any change of a named element (`name_revision`). Then `build()` walks the whole DOM and makes a `HashMap<Vec<u16>, (NodeId, bool)>` of every `id` and `name`, with one `Vec<u16>` allocation for each key. Measured: 54 builds, each of 15.5 k nodes and 5.8 k names; 17 M instructions each (1,100 for each node). All 54 were stale by `name_revision` already (counter `frames_stale_name` 54, `frames_stale_full_only` 0), because Wikipedia scripts add elements that have an `id` between two probes. So gating the list by `name_revision` alone changes nothing.

Only one key is used from each list. The ask is made for a name that nobody defines, so the answer is "no". Complexity today: O(nodes) time and O(ids) memory for each miss after a change. The budget `it.charge(doc.len())` in `fill` is a limit that protects this slow algorithm.

### 4.3 Style: full passes for small changes

The cost of a full cascade is 100 M for 9.97 k elements (10 k instructions for each element; 36 candidates for each element). The article makes 8 of them for one load of the long article (782 M), plus 4 rule builds (103 M). The same page is loaded again at the end: 480 M and 105 M. The rule builds of the second load cost the same as those of the first (16.6, 26.9, 29.4, 29.9 M against 19.2, 26.8, 29.4, 29.9 M). The rule-set cache (`rule_cache`) saves nothing on the reload. Hypothesis: the cost is the plan (sheet text, hashing, `feed_styles`) and not the parse, or the sets of the first load were evicted. Not examined.

Between two passes the document changed little: the passes style 9,853, 9,902, 9,969, 9,970, 9,970 elements. A restyle of the changed elements would cost a small part of 100 M. The code for it exists (`restyle_marks`, `RuleSet::restyle`, `layout_kept`), but it serves one narrow path (a window frame after a restyle) and is lost when the rules change.

Motion passes are 811 M (10.4%) in the article and 916 M (12.5%) in the search: 14 passes in each, a full cascade, then a walk of the whole tree (`motion_seen`, `transition_items`). The page is "armed" (`browser_motion.rs:284`): Wikipedia has transitions, so each document change queues a pass. Most of a motion pass is not lost: the next reader (`offsetTop`) reuses it when the document did not change in between. Hypothesis: the 3 motion passes of the first load run at revisions that no reader uses. Not checked.

### 4.4 Layout: "kept" layouts that keep nothing

`layout_at` is called with `keep = Some(..)` for window frames (`browser_ui.rs:1183`, `:1251`) and the counter `layout reused` is 0 for all 10 "kept" layouts of each journey. A kept layout costs 41.7 to 46 M, the same as a full one (45 M). Cause (hypothesis, from the call sites): `dirty` is `None` at `browser_ui.rs:1183`, so every node is dirty. The headless driver rarely takes the path at `:1251` because the cascade before it was a new full pass, which has no damage list.

Layout is 8.8% and 11.9%. A full layout is about 4,800 instructions for each box.

### 4.5 One hover: 9 restyles of the document

The click step calls `rehover` (`control.rs:416`). That call made one `restyle_frame` of 90,232 elements and 865 M instructions: 9.05 times the document (9,970 elements). `RuleSet::restyle_with` sorts the roots parents first and keeps a comment: "a root inside a subtree that is styled again by an earlier root costs a little more, and is still right" (`cascade.rs:1275`). It does not skip a covered root. A hover change gives the hovered element and its ancestors as roots (`Scopes::user_roots`, `Scope::Subtree`). About 9 nested subtree roots each style their whole subtree. Hypothesis: that is the cause; I did not print the roots. The cost of one subtree root `html` is one full pass: about 96 M. This is the only restyle of the journey, and `wikipedia-search` does not make one. The corpus count of round 1 ("Who forces style and layout") gave `control.rs:416` 222 M over 47 journeys, so it is a Wikipedia effect.

### 4.6 Smaller items

- `querySelector` and `querySelectorAll` walk the whole tree for every call: 392 calls and 7.0 M nodes (302 M inclusive, 3.8%, 43 instructions for each node). `getElementById` is a linear walk too (61 calls). No index by id, class or tag exists in the DOM (`browser_dom.rs:244`, `:1672`).
- HTML parse is 510 instructions for each byte (194 M for 380 KB in three loads). Not designed here.
- Decoded replies: 88 M in `Decoded::read_all`, gzip or brotli of the tape. A replay artifact; a real client decodes too.

## 5. How other engines do it

Read for this task:

- Chromium style invalidation (`style-invalidation.md`, in the Blink source): a DOM change schedules invalidation sets for the changed feature (id, class, tag, attribute). The nodes get dirty bits `NeedsStyleInvalidation`, `ChildNeedsStyleInvalidation`, `NeedsStyleRecalc`, `ChildNeedsStyleRecalc`. When style is read, `StyleInvalidator::Invalidate` walks the tree and applies the pending sets. Style is recalculated only for the nodes that were invalidated. The sets "err on the side of correctness". The document does not describe what happens when a style sheet is added; I did not find it.
- LayoutNG (`developer.chrome.com/docs/chromium/layoutng`): "the immutable fragment tree is designed to reuse large portions of the previous tree for incremental layouts". Each fragment is stored with the parent constraints that made it. One place compares old and new constraints to decide if a child needs layout again. There are caches of the measure and layout passes, and paragraph-level caching for inline layout.
- Stylo, WebKit and Ladybird for the rule index, ancestor filter and sharing: `docs/design-style.md` section 2 has the notes and sources. Stylo's invalidation map (`Dependency` records) restyles only the elements a changed feature can reach.
- Window named properties: the HTML standard defines them as live. Chromium and Firefox keep per-document maps of named items that change when an element with an `id` or `name` is added or removed. From memory, not read in source: to verify.

What I take: retained style and layout with damage marks (Blink, Stylo), keyed reuse of layout (LayoutNG), and lookup by name without a list of all names. bare has the parts (marks, `restyle`, `Kept`), but does not use them for the callers that cost most.

## 6. Designs

The largest share is style plus layout (40% and 37% of the journeys). The cheapest large item is the window name list (11%). I give two designs for each of three kinds of work, then the recommendation.

### 6.1 Window named properties (11.5% and 10.7%)

**W1. One walk with early exit, no list.** On a miss for the name `n`, walk the tree once and compare `id` (all elements) and `name` (`a`, `embed`, `form`, `iframe`, `img`, `object`) with `n` in tree order. Return the first match, as the standard says. Do not allocate. Keep `build` only for the enumeration of keys (`Object.keys(window)`) and for an iframe index query. Data: none new. Complexity: O(nodes) for each miss, no allocation, no list. Measured cost of a walk with a match test: `Dom::query` costs 43 instructions for each node; I assume 50. Estimate: article 54 asks x 15.5 k nodes x 50 = 42 M (now 894 M): saves 850 M = 10.9%. Search 40 x 19.3 k x 50 = 39 M (now 785 M): saves 745 M = 10.2%. Changes about 40 lines in `browser_frames.rs`. The step charge in `fill` becomes the nodes walked, not `doc.len()` for a list.

**W2. A document index of `id` and `name`.** The `Document` keeps `HashMap<Atom, SmallVec<NodeId>>` that follows each insert, remove and attribute write (the same places where `name_revision` moves). A lookup is O(1). It also answers `getElementById` (61 walks now) and `#id` selectors. Memory: 8 to 16 bytes per named element (5.8 k in the article: about 70 KB). Cost: an update on each insert and removal of a subtree (O(subtree) for the ids inside it), and the tree order of the first match must be resolved when two nodes share a name. Estimate: the same 850 and 745 M as W1, plus about 15 M for `getElementById` (61 x 7 k nodes x 35). The extra gain of 0.2% does not pay for the larger change. **Choose W1.** W2 is the design if a later profile shows `getElementById` or `#id` selectors as a large share.

### 6.2 One hover restyle (9.9% of the article)

**H1. Skip a covered root.** In `restyle_with`, keep the set of subtree roots already styled and skip a root that is inside one (the sort by depth already puts parents first). About 10 lines in `crates/css/src/cascade.rs`. Estimate: 90,232 elements become 9,970: 865 M becomes 96 M: saves 770 M = 9.9% of the article, 0 on the search. Check first that the nested roots are the cause (print the roots).

**H2. Fall back to the full cascade when the roots cover most of the document.** If the sum of the subtree sizes of the roots is more than the number of elements, run `compute_kept` (100 M). Same estimate as H1 for this page; it is safe when the subtree sizes are not known cheaply. H1 is the right fix (the algorithm repeats work), and H2 can stay as a guard. **Choose H1.**

### 6.3 Style and layout (30.7% + 8.8% article, 25.2% + 11.9% search)

**S-A. One retained cascade and layout, updated by damage.** Data: the `Styles` of the last pass stay the single cache for every caller: frame, motion, `offset*`, `getBoundingClientRect`, locators, hit test. `Built` keeps them when the rule set changes. The document logs damage marks for the reader (`DAMAGE_STYLE`, which exists). Rules:

1. A pass finds the marks since the last pass. No marks and the same rules: the cache answers (this exists as "hit").
2. Marks without a change of rules: `restyle` of the roots (exists). It must serve `keep = true` callers too, not only `read`.
3. A sheet appended at the end (a new `<link>` or `<style>` that the loader adds in order; `plan.parts.starts_with(old key)`, which exists for the rule build): match only the new rules against the elements. An element that gets no new match keeps its style. An element that does is cascaded again with the old matched list plus the new entries. Cost: elements x candidates of the new rules, not of all rules. The new rules are 20 to 25% of the final set at each growth step (rule counts 1,783, 2,407, 2,490, 2,534: steps of 624, 83, 44). Hypothesis: the matched lists are kept in `Kept`; I did not check what `Kept` stores for each element. If it does not store them, step 3 needs them (the index of rule order is in `entries`).
4. Motion: the walk of `motion_seen` covers only the elements in `changed`, not the whole tree.
5. Layout: a style change gives the dirty nodes (the changed elements and their ancestors, as `browser_ui.rs:1251` does) to `layout_kept`. The `Kept` is used by all callers. A layout with no dirty node is a hit.

Complexity: today O(elements x candidates) for each pass. After: O(changed elements x candidates) for each pass, plus one full pass for each load. Memory: no new structure; the `Kept` of layout (fragments of the last layout) stays alive between passes: about 9.5 k fragments x 56 B = 0.5 MB (hypothesis: the size of `Fragment`; massif shows 4.0 MB of fragments at the peak with several copies alive).

Estimate for the article (from the `FS` counters). Today: compute 1,319 M + layout 639 M = 1,958 M. After, with the cost model: the first full cascade of each load (80 M, 80 M; second article 17 M), 3 sheet-growth steps for each long load at 25 M, change restyles about 10 M for each pass, first layout of each load (45 M, 45 M, 6 M) and 5 M for each later layout. Load 1: 80 + 75 + 20 = 175 M; article 2: 17 + 8 = 25 M; load 2 (5 passes): 80 + 50 = 130 M; layouts: 96 M + 14 x 5 M = 166 M. Total 496 M. Saves 1,462 M = 18.8%. Sensitivity: if only 50% of the style and layout instructions go away: 979 M = 12.6%. If 80%: 1,566 M = 20.1%. For the search: today 1,560 M + 815 M = 2,375 M: 50% = 1,188 M (16.2%), 70% = 1,663 M (22.7%), 80% = 1,900 M (25.9%). I use 70% in section 7. These are estimates. The risk is step 3 and the quality of the dirty marks; a restyle that returns `None` falls back to a full pass, so the result stays correct.

**S-B. Style on demand, no pass for the page itself.** Remove the passes that nobody reads: do not run `motion_frame` as a separate cascade; run `motion_seen` only when a transition or animation could start (a changed element has a `transition-property` or an animation). Layout and styles are made at the first read after a change (as today for readers). Data: none new. Estimate: hypothesis: the motion passes of the long loads run at revisions that no reader uses. They cost 811 M in the article and 916 M in the search, of which part returns later when the reader's pass runs at the next revision. Count from the `FS` lines: of 8 full passes in load 1, 3 are motion passes. Saves about 3/8 of the compute and 2 of 4 rule builds of the long loads: (297 + 56) M x 2 + 43 M = about 750 M = 9.6% (article), search about 10 to 12%. The gate "a changed element starts a transition" needs the list of changed elements, which is S-A step 4. Without it the page is armed all the time (Wikipedia has transitions) and S-B has no gate. So S-B depends on S-A, and its gain is inside the S-A estimate (the 70% of S-A already includes the motion passes). **Choose S-A.** Build S-B's gate as step 4 of S-A.

**S-C. A cheaper full cascade.** 10 k instructions for each element is high (36 candidates for each element). Sharing a style between elements that look the same (Stylo's sharing cache, `docs/design-style.md` phase 3) could halve it. Round 1 deferred it: the estimate gave wrong matched lists for 0.14% of elements. Estimate: it halves the cost of the first full pass of each load (about 80 M each) and of the rare full pass later: about 100 to 150 M for the article (1.3 to 1.9%). Small after S-A, because S-A removes most full passes. Not now.

### 6.4 Not designed here

JS execution and JS parse are 31% and 33% of the journeys and belong to R2-A and R2-D. For R2-A: the five most-called natives and host accessors above; a parse cache keyed by source text would help the second load of a page (the article is loaded twice, and the scripts have the same bytes): upper bound is the parse of the repeated scripts, about 1/3 of 653 M = 220 M (2.8%) (hypothesis: the three loads parse the same scripts the same amount; the 6 `lazy_used` parses of 1.92 MB did not show which URL each was).

## 7. Expected gain on the criterion

Settled benchmark, estimates. Today (this build, 2 runs): 95,376 M of 105,194 M = 90.7%.

| Step | `wikipedia-article` (7,776 M) | `wikipedia-search` (7,338 M) | The two | Share of 95,376 M |
|---|---:|---:|---:|---:|
| W1 window names | -850 M (10.9%) | -745 M (10.2%) | -1,595 M | 1.7% |
| H1 hover restyle | -770 M (9.9%) | 0 | -770 M | 0.8% |
| S-A style and layout (70%) | -1,370 M (17.6%) | -1,663 M (22.7%) | -3,033 M | 3.2% |
| All three | -2,990 M (38.5%) | -2,408 M (32.8%) | -5,398 M | 5.7% |

The savings do not overlap by construction (W1 is the name list, H1 is the one restyle call, S-A is the cascade and layout passes; S-A's 70% was taken on the `FS` style and layout counters, which exclude the restyle call and the name list). The arithmetic for the article S-A row uses 70% of 1,958 M = 1,371 M.

Criterion 2 after the three steps on these two journeys: (95,376 - 5,398) / 105,194 = 85.5%. Gain: 5.2 points of the 10.7 that are missing (90.7 to 80).

The Wikipedia family has four more journeys: `wikipedia-main-random` 4,139 M, `wikipedia-toc-languages` 3,723 M, `wikipedia-portal` 1,233 M, `wiktionary-portal` 1,560 M (together 10,655 M). Hypothesis: they have the same page shape (the same skin, scripts and sheets), so W1 and S-A apply. Their cost without the settle loop is 1,386, 1,402, about 250 and 90 M, so most of it is the settled phase. A gain of 25 to 35% gives 2.7 to 3.7 G more: criterion 2 = (95,376 - 5,398 - 2,700..3,700) / 105,194 = 82.1% to 83.0%. Not measured: I did not profile these four.

Criterion 4 and others: not affected. Peak RSS: S-A keeps one `Kept` layout alive (about 0.5 MB), W1 removes the 0.7 MB name map and its churn. Expected RSS change: -1 MiB to +0.5 MiB; not measured.

## 8. Order of work, risks and checks

1. **H1** (10 lines): print the roots first to confirm the cause. Check: a test with a hover rule on an ancestor and a deep tree: elements styled equal the subtree size, not a multiple. Journey check: `FS style restyle` line for the article shows about 9,970 elements.
2. **W1** (40 lines): tests for the order of the first match (`id` and `name` rules of `browser_frames.rs`) and for a name added and removed between two asks. Journey check: `frames_build` count 0, name-list share under 1%.
3. **S-A** in four slices, each measured on the two journeys: (a) `keep = true` callers use `restyle`; (b) `Built` is kept when a sheet is appended, steps 2 and 3 of section 6.3; (c) the dirty list reaches `layout_kept` (the counter `layout reused` is the check: it must be above 0); (d) the motion walk reads `changed`. The existing `a_kept_layout_equals_a_full_layout_after_random_changes` test (`crates/layout/src/keep.rs`) and a new test of the same kind for the cascade (a restyle equals a full pass after random changes, as for layout) guard the correctness. A restyle that cannot answer returns `None` and the full pass runs, so a missing case costs time, not a wrong style.
4. Limits that these steps make unneeded: the `it.charge(doc.len())` of the name list; nothing else is removed. `computed-work` stays.
5. Re-measure with `tools/bench/journeys.sh compare --clock virtual --runs 3 --pss` and the counters of `force.rs`.

## 9. What I did not measure

- The four other Wikipedia journeys (hypothesis only). Journeys of other sites for S-A.
- The cause of the 90,232-element restyle (the roots were not printed). The cause of "rule builds cost the same on reload" (hypothesis only). What `Kept` stores for each element (S-A step 3 depends on it).
- Wall time (the machine is shared). The live sites. The memory cost of S-A.
- The profile has no inline frames from the standard library; a sample inside `memcpy` or `malloc` is charged to the next bare frame above (frame pointers skip the direct caller of a leaf libc function). Memory by `massif` is one run of one journey.
- The no-settle runs change the timing of the page. They are a count, not a replacement benchmark. I did not check that the page does the same visible work without the loop.
- The cost model of S-A (25 M for a sheet-growth step, 10 M for a change restyle, 5 M for an incremental layout) is an estimate. The sensitivity (50% and 80%) is in section 6.3.

## 10. Noticed, not changed

- `getElementById`, `querySelector*`, `getElementsByClassName` and `getElementsByTagName` walk the tree on each call. 3.8% inclusive for the selectors. An index by id, class and tag would help; the gain is small next to the three steps above.
- `wikipedia-search` has a timing-dependent replay miss (`POST .../ins-502b/v2/events?hasty=true`) in about 1 run in 4: the page sends an event when the timer fires before the step ends. It changes the instruction count by about 3%. The benchmark would gain from a recorded reply for it.
- The settle loop prints no counters. `SETTLE` lines in the counter patch show 25 to 45 ticks and about 50 ms virtual for each call, and `images_pending()` is still true after some loops that ended on their own (to verify).
