# Design: stop parsing lazy JS functions again and again

Status: proposal. The owner reviews this document before any code is built.
Written on 2026-11-14 against main `28a8fbc2` (task R2-D). All numbers come from archMachine (16 cores, shared). Instruction counts are `instructions:u`.
Nothing here is built. A number marked "estimate" is computed from measured numbers. A claim marked "hypothesis" or "to verify" is not checked.

## 0. Summary

The question: why do `wiktionary-portal` (1,561 M instructions, +28% since PR #492) and `wikipedia-portal` (1,234 M, +23%) cost more, and what removes the cost?

Findings, in short:

1. **The hypothesis is right, but the first parse is not a preparse.** The parser parses a long function body in full, checks it, and drops the nodes (`function_inner` in `parse.rs`). It has no cheap skip mode. A function that runs is parsed a second time. Every long function inside it is parsed again too, and dropped again. A function that sits `d` lazy functions deep, and runs, is parsed `d + 2` times. V8 had the same fault before v6.3 and fixed it with data from the preparser (section 2).
2. **Lazy functions explain the whole rise on these two journeys.** With lazy functions off (same binary), `wiktionary-portal` costs 1,239 M and `wikipedia-portal` 996 M. Before #492 they cost 1,213 M and 1,000 M. The second parses cost 331 M and 238 M. The load step of those functions costs 20 M and 16 M. A lazy walk saves 39 M and 26 M.
3. **On all 47 journeys the cost is 3,490 M (3.8%).** Second parses are 3,116 M. The load step is 358 M. The walk saves 809 M. The rest, 822 M, is a second cause: each call that crosses from one script slot to another swaps eleven fields of the interpreter (about 190 instructions). Lazy functions live in slots of their own, so calls cross slots 1.4 M more often in `react-docs` alone. This is the cause of the rows that the second parses do not explain (`react-docs` +330 M, `nodejs-org-home` +150 M, `github-docs-home` +106 M).
4. **Not the cause:** the parse cache (0 hits on both portals), the lazy regex compile (it is cheaper than before), the garbage collector (no cost is left once the slot swaps are counted).
5. **The benefit of lazy functions is real.** Peak RSS is 61.9 MiB (3.4%) lower over the 47 journeys. `substack-home` is 92 M and 21 MiB lower. A fix must keep that.

Recommendation (section 5): **A. Keep skip data for the lazy functions inside a lazy function, so a second parse skips them.** Then **S. Make a slot swap one pointer swap.**

| Criterion | Now | After A | After A and S |
| --- | --- | --- | --- |
| 2: 47 journeys, instructions (estimate from the corrected 94,796 M of 105,189 M) | 90.1% | 88.2% | 87.5% |
| `wiktionary-portal` | 1,561 M | about 1,317 M | about 1,309 M |
| `wikipedia-portal` | 1,234 M | about 1,072 M | about 1,060 M |
| Peak RSS, 47 journeys | -3.4% against eager | the same (table: +0.3 MB at most per process) | the same |

Neither reaches the old cost of the two portals (1,213 M and 1,000 M): 6% to 8% remain. Criterion 2 (80%) is not reachable with this change: all lazy cost removed gives 86.8%. Criterion 4 (`python-org-docs-link` 1.3 times) is not reached either: eager code gives 1.25 times.

## 1. Measurements

### 1.1 Method

- Binary: main `28a8fbc2`, `bare-drive`, release, `CARGO_PROFILE_RELEASE_DEBUG=line-tables-only`, with temporary counters. The patch and the counter file are in `/data/bare-tmp/r2/R2-D/` (`counters.patch`, `r2d.rs`). Nothing of it is in the repository.
- Counters read `perf_event_open` (user instructions of the thread) around: each fresh parse in the parse cache, each second parse (`Program::lazy_function`), the rest of `lazy_load` (scope names and walk), `walk_program`, the regex compile and the regex check at parse time. One more counter counts `Interp::select` calls that change the current script. A counter binary gives the same totals as the build without counters to 0.4% (`wiktionary-portal` 1,560.3 M against 1,566.8 M with per-root bookkeeping; `wikipedia-portal` 1,233.3 M against 1,233.8 M).
- A temporary switch `R2D_MODE` changes the parser in the same binary: `nofn` (no function is lazy; data literals and lazy rule bodies stay on), `nonest` (a function is lazy only in the first tree, not inside a second-parse tree), default (main). `R2D_MINBODY` sets the 128-byte limit for a lazy body.
- Inputs: the 47 journeys that pass in the corpus `virtual` (`BARE_CLOCK=virtual`, `BARE_SEED=1`, replay). Each run counts replay misses (`no recorded reply`). Wiktionary and the other main rows had 0 misses. The threshold sweep (section 1.5) had 1 to 4 misses on four `wikipedia-*` rows: those rows are not used.
- Runs: 3 runs for each journey and mode. Spread (max - min over median) is 0.1% or less on both portals and 0.5% or less on 43 of 47 journeys (`hn-new-past` 2.6%, `lobsters-comments-back` 1.4%, `blog-rust-lang-home` 0.9%, `hn-front` 0.8%). Counter files and the `select` counts are 1 run each (fixed work under the virtual clock).
- A first sweep used `R2D_EAGER`, which turned lazy parsing off together with data literals. It made `python-docs-search` look 857 M cheaper without lazy functions. That was wrong: data literals only exist in the lazy parse mode. The numbers here come from `nofn`, which keeps them.
- The earlier binary `0727ffef` (before #492) is the "before" column. Main has had more merges since. The `nofn` column is the clean same-binary baseline.

### 1.2 The two journeys

Instructions, M, median of 3 runs.

| Journey | before #492 (`0727ffef`) | main, lazy | main, `nofn` | main, `nonest` |
| --- | ---: | ---: | ---: | ---: |
| `wiktionary-portal` | 1,213.4 | 1,561.2 | 1,238.7 | 1,396.7 |
| `wikipedia-portal` | 1,000.3 | 1,234.2 | 995.5 | 1,095.9 |

Where the lazy cost goes (counters, 1 run):

| Item | `wiktionary-portal` | `wikipedia-portal` |
| --- | ---: | ---: |
| Scripts that make lazy functions (16 KiB or more) | 5 of 12 scripts, 1.68 MB | 6 of 15 scripts, 1.10 MB |
| First parse, all scripts (no hit in the parse cache) | 202.5 M | 115.0 M |
| Second parses | 655 functions, 3.04 MB, 331.4 M | 703 functions, 2.11 MB, 237.8 M |
| of which the function sits in the first tree (depth 0) | 117 functions, 155.0 M | 120 functions, 97.2 M |
| of which the function sits inside a second-parse tree | 538 functions, 176.3 M | 583 functions, 140.5 M |
| Load step (scope names, walk of the new tree) | 19.9 M | 16.0 M |
| Walk saved at load (lazy against `nofn`) | -39.1 M | -25.6 M |
| Slot swaps (lazy against `nofn`; 190 instructions each, section 1.4) | +42,856 swaps (8 M) | not counted (10 M of the sum below) |
| Sum | +320.6 M | +238.8 M |
| Measured, lazy minus `nofn` | +322.5 M | +238.7 M |

One bundle holds most of it on `wiktionary-portal`: a `load.php` file of 1,386,355 bytes (jQuery and `mw.loader.impl` modules). Its first parse costs 173.7 M. It has 63 lazy functions at the top. 61 of them run (97%). They hold a second parse of 130.9 M. Its 575 second parses by depth: 61, 161, 205, 116, 25, 4, 1, 2. They cost 288.2 M.

`wikipedia-portal` has the same shape: a 783,037-byte bundle (87.8 M first parse; 568 second parses, 194.0 M) and one of 132,325 bytes (65 second parses, 23.5 M).

The `nofn` column does not equal the "before" column on `wiktionary-portal` (+25.5 M, 2.1%). Hypothesis: the 23 merges (87 commits) between `0727ffef` and main. Not examined.

### 1.3 The 47 journeys

Lazy against `nofn`, instructions M, sum of medians: 95,488.2 M against 91,998.1 M (+3,490.1 M, +3.8%). `nonest`: 93,803.5 M (+1,805.4 M). Peak RSS, sum of medians, MiB: lazy 1,779.7, `nofn` 1,841.6, `nonest` 1,800.3.

Split of the 3,490 M (counters, sum over journeys):

| Item | M | Note |
| --- | ---: | --- |
| Second parses | +3,115.5 | 10,069 parses, 26.37 MB |
| Load step | +357.6 | 10,069 loads in realms (one per realm and function) |
| First parse, lazy against `nofn` | +2.3 | the nodes are made and dropped: no extra cost |
| Walk saved | -809.0 | the first walk skips the dropped bodies |
| Regex compile, check | +0.1 | no change |
| Rest | +822.5 | slot swaps, see 1.4 |
| Sum | +3,489.0 | measured +3,490.1 (sum of medians; the journeys with no lazy function are not in the split) |

Parse bytes and instructions (counters, 24 of the 47 journeys have a script of 16 KiB or more):

| | Bytes | Instructions | per byte |
| --- | ---: | ---: | ---: |
| First parse, all scripts | 38.23 MB | 4,045 M | 106 |
| of which scripts of 16 KiB or more | 36.74 MB | 3,833 M | 104 |
| Second parses | 26.37 MB | 3,116 M | 118 |

So the parser reads 64.6 MB for 38.2 MB of source (1.69 times).

Share of the lazy functions that later run (counters):

| Depth in lazy functions | Lazy functions in the trees | Run | Run by count | Run by bytes | Second-parse instructions |
| ---: | ---: | ---: | ---: | ---: | ---: |
| 0 (first tree) | 9,471 (18.44 MB) | 3,470 (13.91 MB) | 37% | 75% | 1,522.9 M |
| 1 | 4,089 (10.12 MB) | 2,540 (8.60 MB) | 62% | 85% | 1,039.7 M |
| 2 | 7,930 (5.51 MB) | 2,404 (2.84 MB) | 30% | 52% | 390.4 M |
| 3 | 2,452 | 1,207 | 49% | 57% | 126.9 M |
| 4 to 7 | 750 | 448 | 60% | | 35.7 M |

The functions that run are the big ones. On `wiktionary-portal` 85% of the depth-0 functions run and on `wikipedia-portal` 83%. A bundler wrapper (`function($,jQuery,require,module){…}` of 100 KB or more) always runs.

Per journey, first parse (M), second parse (M, MB), skippable inner bytes (MB, section 3), load (M), depth-0 functions and the share that runs, parse-cache hits (journeys with 5 M or more in second parses; the others are in `/data/bare-tmp/r2/R2-D/s2/`):

| Journey | first | second M | second MB | inner MB | load | depth 0 (run) | hits |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| `wikipedia-main-random` | 384.6 | 722.8 | 6.51 | 4.67 | 61.8 | 261 (95%) | 24 |
| `wikipedia-search` | 279.1 | 510.9 | 4.70 | 3.16 | 36.7 | 361 (58%) | 8 |
| `wikipedia-article` | 211.8 | 441.2 | 3.91 | 2.69 | 49.0 | 200 (94%) | 19 |
| `wiktionary-portal` | 202.7 | 331.4 | 3.04 | 2.24 | 19.9 | 137 (85%) | 0 |
| `wikipedia-toc-languages` | 121.6 | 253.3 | 2.26 | 1.53 | 17.7 | 128 (93%) | 0 |
| `wikipedia-portal` | 115.1 | 237.8 | 2.11 | 1.44 | 16.0 | 145 (83%) | 0 |
| `react-docs` | 341.1 | 105.1 | 0.55 | 0.10 | 33.5 | 1,628 (33%) | 14 |
| `go-dev-home` | 250.9 | 96.4 | 0.43 | 0.15 | 35.4 | 1,576 (35%) | 11 |
| `python-org-docs-link` | 70.9 | 71.0 | 0.44 | 0.30 | 11.5 | 150 (69%) | 5 |
| `nodejs-org-home` | 298.5 | 54.2 | 0.26 | 0.07 | 19.5 | 1,095 (28%) | 18 |
| `github-docs-home` | 303.1 | 51.8 | 0.30 | 0.06 | 17.4 | 865 (36%) | 11 |
| `rust-book-chapters` | 27.9 | 31.7 | 0.32 | 0.16 | 6.5 | 140 (51%) | 20 |
| `rust-book-search` | 91.2 | 31.4 | 0.33 | 0.16 | 2.8 | 140 (69%) | 0 |
| `mdn-reference` | 238.7 | 26.3 | 0.17 | 0.04 | 5.3 | 90 (90%) | 21 |
| `devto-home` | 155.5 | 24.5 | 0.12 | 0.03 | 5.8 | 968 (23%) | 0 |

`rust-book-search` re-parses 166 bodies (326 KB) in this run, 0.3% of its 16.3 G: the same as the #501 note.

### 1.4 The second cause: slot swaps

The second parses and the load step do not explain every row. `react-docs` is +330.5 M against `nofn`, but its second parses and loads are 138.6 M and its walk saves 80.2 M. A residual of 272 M is left.

A function of a lazy tree is a script slot of its own (`lazy_load` in `eval_scripts.rs`). A call into another slot runs `Interp::select`. It swaps `prog` and ten per-script fields (`expr_name`, `pat_name`, `stmt_label`, `func_name`, `site_strs`, `prop_str`, `scopes`, `slots`, `fflags`, `cflags`), then swaps two `Code` records in `scripts`. The return swaps back. I counted the calls that change the slot:

| Journey | residual M | slot swaps, lazy | slot swaps, `nofn` | extra swaps | residual per extra swap |
| --- | ---: | ---: | ---: | ---: | ---: |
| `react-docs` | 272 | 1,556,400 | 154,410 | 1,401,990 | 194 |
| `nodejs-org-home` | 129 | 835,884 | 167,648 | 668,236 | 193 |
| `github-docs-home` | 83 | 491,842 | 52,918 | 438,924 | 190 |
| `go-dev-home` | 60 | 311,956 | 1,766 | 310,190 | 193 |
| `mdn-reference` | 51 | 426,640 | 160,710 | 265,930 | 191 |
| `wikipedia-article` | 73 | 270,170 | 16,362 | 253,808 | 288 (25 M left) |
| `wiktionary-portal` | 10 | 45,622 | 2,766 | 42,856 | 229 |
| `devto-home` | 10 | 52,760 | 214 | 52,546 | 183 |

One slot swap costs about 190 instructions. This explains the residual on six of the eight rows within 5% of 190. `wiktionary-portal` is a small residual (10 M). `wikipedia-article` has 25 M left. The 822 M over the 47 journeys is about 4.3 M extra swaps (estimate from the 190 figure; counted on 8 journeys only).

`nonest` does not remove this: `react-docs` still has 1,354,090 swaps. The swaps come from the slot of each lazy function, not from the nesting.

The garbage collector, the parse cache and the regex compile are not the cause. The regex compile of a lazy literal is a saving: 185 programs on `wiktionary-portal`, 42.2 M, and before #492 the first walk compiled every literal of every walked body (`0727ffef`, `ext_eval_regexp`).

### 1.5 The limits

`LAZY_MIN_BODY` (128 bytes) and `LAZY_MIN_SOURCE` (16 KiB) are limits that protect a slow second parse. I raised the first one (1 run per row, 18 journeys; the four `wikipedia-*` rows with misses are in the sum, so read the sums as rough):

| `LAZY_MIN_BODY` | instructions, sum M | peak RSS, sum MiB |
| ---: | ---: | ---: |
| 128 (main) | 86,768 | 1,081.6 |
| 512 | 86,315 | 1,098.4 |
| 2,048 | 86,070 | 1,117.3 |
| 8,192 | 85,359 | 1,139.2 |

`wiktionary-portal` goes from 1,561 M to 1,491 M (-4.5%) at 8,192 and RSS from 42.6 to 46.2 MiB. A limit buys little time and costs memory at once, because the functions that run are big (section 1.3). The limit is a sign of the design fault of section 3. After design A it is needed less.

## 2. How other engines do it

- **V8.** Its preparser skips a function and still checks syntax. It stores a dense array of flags for each variable that an inner function may use, only for functions that have inner functions. "Before V8 v6.3 / Chrome 63, compiling a lazy function meant re-preparsing its nested functions. A function was therefore preparsed as many times as its nesting depth" (our fault, the same shape). Since v6.3 the metadata lets the parser skip inner functions: each function is preparsed at most once and fully parsed once. After bytecode is flushed the function is parsed again, and its inner functions are skipped again. V8 also compiles a function at once when `(` stands before `function` (PIFE), or after `!` or `,` in the Uglify pattern. It warns that a rule such as "any function argument is a PIFE" compiles too much and costs memory. [V8: Blazingly fast parsing, part 2](https://v8.dev/blog/preparser).
- **V8 code cache.** The cache holds the bytecode of the script after it ran, so the functions compiled lazily during that run are in it. A cold start does not gain: only a later load does. [V8: Code caching for JavaScript developers](https://v8.dev/blog/code-caching-for-devs).
- **SpiderMonkey.** The default parse is a "syntax" (lazy) parse. It finds all early errors and makes no AST and no bytecode for the inner functions. The first call recompiles only that function ("delazification"). The syntax parser "runs quickly, avoids allocating memory when possible, and only checks for syntax errors". The syntax parse of a large script can run on another thread. [Firefox source docs, SpiderMonkey](https://firefox-source-docs.mozilla.org/js/index.html), [Bugzilla 678037](https://bugzilla.mozilla.org/show_bug.cgi?id=678037), [Mozilla JavaScript blog](https://blog.mozilla.org/javascript/). I did not read about relazification in a source I could open: not covered here.
- **JavaScriptCore.** `SourceProviderCache` maps the start offset of a function to the data that lets the parser skip it in a later parse. `UnlinkedFunctionExecutable` parses a function again at its first call. The cache has a known flaw: an entry made in one context (for example an arrow function parsed as the parameters of another) can be reused in a context where it is wrong. Bun's WebKit fork fixed this by storing generator and async flags in the entry. [oven-sh/WebKit #702](https://github.com/oven-sh/WebKit/pull/702), [#703](https://github.com/oven-sh/WebKit/pull/703), [#699](https://github.com/oven-sh/WebKit/pull/699).

All three keep per-function data from the first parse, so the second parse of an outer function does not parse its inner functions again. We keep nothing, because our first parse is the full parser and its result (inner stubs) is thrown away when the outer function is dropped.

## 3. The current design

- First parse of a source of 16 KiB or more: the full parser (`parse_counted`, lazy flag). For a plain function with a body of 128 bytes or more, `function_inner` parses the body, checks it, and `arena_truncate`s its nodes. It keeps a `LazyFn` (`params_at`, `name`, `start`, `kind`, `strict`, `parsed: OnceCell`) in `LazyInfo.lazies`. `Body::Lazy(idx)` points at it. **Any stub that a function inside it made is truncated away with it** (`arena_mark` has `lazies`). That is the data that V8 and JSC keep.
- First call of the function in a realm: `lazy_load`. `Program::lazy_function` parses it again (`parse_lazy_function`, the same lazy flag on, `eager_at = start`). The tree is shared by all realms through `LazyFn.parsed`. In this parse the inner long functions are parsed in full, checked and dropped, as in the first parse. Then the realm adds a script slot, walks the new tree in the scope of the stub (`lazy_walk`) and sets `Callee::Script` of the function object.
- Complexity. Let `S` be the source bytes and `d(f)` the number of lazy functions around `f`. A function that runs costs `d(f) + 2` parses of its bytes (the first parse, once for each lazy function around it, and its own). Total parse work is `O(S × (1 + average d))`. Memory: the trees of the functions that ran, plus one `LazyFn` (about 40 bytes, estimate) for each lazy function of a tree. A cold run pays this; the parse cache only saves a later document.
- Measured: `d` reaches 7 (wiktionary bundle: 61, 161, 205, 116, 25, 4, 1, 2 second parses at depths 0 to 7). Parses cost 1.69 times the source bytes over the 47 journeys.

Three facts shape the design:

1. The functions that run are the big ones (75% of the bytes at depth 0). A rule based on size or position (the `LAZY_MIN_BODY` sweep, the IIFE rule) cannot find them: a bundler passes them as an argument or an array item, not as `(function(){…})()`.
2. 66% of the second-parse bytes are the bodies of lazy functions inside the function (17.34 of 26.37 MB, "inner MB" in 1.3). They are parsed only to be dropped.
3. The cost does not depend on the page script alone. A cold run has no earlier document: 0 parse-cache hits on both portals.

## 4. Options

### Option A: keep skip data for inner lazy functions (V8 skippable functions, JSC `SourceProviderCache`)

Data structures. When `function_inner` drops a body, it moves the stubs that the body made (`lazies[mark.lazies..]`) into the `LazyFn` of the dropped function: `LazyFn.inner: Rc<[LazyFn]>`, sorted by `start`. Each inner `LazyFn` also keeps the first token after its `}` (`end: u32`) and the `strict` state of the function after its body (the body may have a `"use strict"` directive; the `Func` node needs it). Its own `inner` list comes along. This gives a tree of tables, one table for each lazy function that has lazy functions inside.

Algorithm. In `parse_lazy_function` the parser has the table of the function. At `function_inner`, if `lazy_ok` and the table has an entry for `start`: build the stub from the entry, jump the lexer to `end` (`Lexer::starting_at` exists), set `self.tok` and `prev_end`, and return `Body::Lazy(i)`. The tree of the second parse shares the table (`LazyInfo.lazies` becomes an `Rc<[LazyFn]>` for a second-parse tree), so `Body::Lazy(i)` indexes it, and the cache of inner trees (`parsed`) stays in the entry. The first parse is the same as now. A function that was not lazy in the first parse (an IIFE, `eager_at`) has no entry and parses in full, as now.

Why it is safe: the parse context of a function is the same in both parses (the second parse starts with the `strict` and the `Ctx` that the first parse had; `eager_at` decides alike). The first parse found every early error. The facts that stop laziness (`super`, `new.target`) make no entry. Not safe to assume: the lexer goal after `}` (a function declaration or a function expression), the `used` node count, and the `await_pos` and `last_octal` state. These need tests. To verify: that a skipped function body cannot change the scope declarations of the outer function (`var` in the body does not leave a function; the walk of a lazy stub already works without the body).

Complexity. A function that runs costs 2 parses of its own bytes, not counting the inner lazy bodies: total parse work `O(S + sum over run functions of own bytes)`. Memory: one `LazyFn` of about 56 bytes (40 plus `inner`) for each lazy-eligible function of the first parse: 30,611 entries over all 47 journeys, 2,133 on `wiktionary-portal` (120 KB of its 43 MiB, 0.3%), 1,549 on `wikipedia-portal`. Nothing of the tree memory changes.

Expected gain. The second-parse bytes fall from 26.37 to 9.03 MB: the inner bodies are not parsed (17.34 MB). Estimate: second-parse instructions fall by the same share, 66%. Per journey, instructions saved = second-parse instructions × inner bytes / second-parse bytes:

| Journey | second M | inner / second | saved M | new total M |
| --- | ---: | ---: | ---: | ---: |
| `wiktionary-portal` | 331.4 | 0.74 | 244 | 1,317 |
| `wikipedia-portal` | 237.8 | 0.68 | 162 | 1,072 |
| `wikipedia-main-random` | 722.8 | 0.72 | 518 | 3,624 |
| `wikipedia-search` | 510.9 | 0.67 | 343 | 6,997 |
| `wikipedia-article` | 441.2 | 0.69 | 304 | 7,475 |
| `wikipedia-toc-languages` | 253.3 | 0.67 | 171 | 3,643 |
| `react-docs` | 105.1 | 0.18 | 19 | 16,930 |
| all 47 journeys | 3,115.5 | 0.66 | 1,989 | 93,499 |

The 47-journey total uses each journey's own ratio (sum of the products). The estimate assumes that the inner bytes cost the same per byte as the rest of the second parse. The measured cost per byte is 110 at depth 0, 121 at depth 1, 137 at depth 2 and rises with depth. I give a range of 1.6 G to 2.4 G (+-20%). The model is tested: for `nonest` it predicts that the second parses at depth 1 or more disappear, 1,593 M plus their loads; the measured gain of `nonest` is 1,685 M. Hypothesis: the model holds within 10% for A. To verify with a prototype before the design is accepted (a day of work, kept out of main).

Cost of A: about 150 lines in `parse.rs`, `ast.rs` and `eval_scripts.rs`. Tests: `exec_diff`, test262, new parse tests for every early-error path inside a skipped body, a function that turns strict inside its body, a getter or setter, a body in a class (not lazy), a function in a block, labels, and a regex after a skipped function.

### Option B: parse eagerly

B1: no lazy functions (the `nofn` mode, measured): -3,490 M over the 47 journeys (95,488 to 91,998 M), and `wiktionary-portal` 1,239 M, `wikipedia-portal` 996 M. Peak RSS +61.9 MiB (1,779.7 to 1,841.6 MiB, +3.5%); `substack-home` +21.5 MiB and +92 M instructions; `go-dev-home` +6.3 MiB; `nodejs-org-home` +3.8 MiB. It removes the reason for PR #492 (tree memory 2 to 4 times lower on big script pages). It needs no code.

B2: raise `LAZY_MIN_BODY` (section 1.5). 8,192 bytes recovers about 1,400 M on the 18 journeys and costs +57.6 MiB (+5.3%) there. That is B1's memory loss for 40% of B1's gain. Not recommended.

B3: eager for a heuristic of "runs at load" (IIFE, `!function`, `,function`, as V8). #492 already does it for the call forms. The functions that cost here are not in those forms (section 3, fact 1). A rule that every function argument is eager loses the memory gain; V8 warns about the same. Not recommended.

### Option C: record which functions run, per source, in the process

Keep per source hash the set of functions that ran, and parse those eagerly in a later load of the same text. Gain: 0 on the two portals (0 parse-cache hits, so no later load) and no gain in a cold run. Journeys with hits (233 over the sweep, up to 24 on `wikipedia-main-random`) already share the second-parse tree through `LazyFn.parsed` and parse nothing again. V8's code cache has the same limit: it helps a later load. Not recommended. A disk cache of parse results is a separate item (step 5 of `docs/perf-goal.md`, "only if the measured misses justify it"); the cold benchmark would not show it.

### Option D: lazy only in the first tree (`nonest`, measured)

In a second parse the inner functions are parsed in full and kept. One condition in `function_inner`. Measured: 93,804 M over the 47 journeys (-1,685 M against main), `wiktionary-portal` 1,397 M (-164 M), `wikipedia-portal` 1,096 M (-138 M). Peak RSS 1,800.3 MiB (+20.6 MiB, +1.2% against main; between main and B1). It removes about half of the lazy cost and about two thirds of the memory gain stays. It does not remove the slot swaps. It is a fallback if A is blocked. A subsumes it: with A an inner function costs only its own bytes.

### Option S: a slot swap is one pointer swap (second cause)

Data structures. Put the ten caches of `Code` in one `Box<Caches>`. `Interp` holds `cc: Box<Caches>` and `prog`. `Code` is `{ prog: Rc<Program>, cc: Box<Caches> }` (16 bytes). `select` swaps two pointers and the `scripts` record (16 bytes). Algorithm: unchanged. The use sites change from `self.expr_name` to `self.cc.expr_name`: about 245 mechanical edits (`expr_name` 45, `pat_name` 37, `stmt_label` 23, `func_name` 26, `site_strs` 16, `prop_str` 13, `scopes` 53, `slots` 24, `fflags` 21, `cflags` 10), and one more pointer read on each. Hypothesis: a swap then costs 15 to 25 instructions.

Expected gain. About 4.3 M extra swaps over the 47 journeys, 822 M at 190 each. At 20 each: 4.3 M × 170 = 730 M saved (estimate). It also lowers the 29 to 32 M that the swaps of eager code cost on `react-docs` and `nodejs-org-home`. `wiktionary-portal`: -8 M; `react-docs`: about -240 M of 16,949 M (1.4%).

Option S would also fit a later change: `Interp` could keep the slot of the lazy tree in the slot of its parent. It is not needed.

### Option F: a preparse mode (no nodes) for the first parse

A parser mode that checks syntax and scopes but builds no nodes, as the V8 preparser and the SpiderMonkey syntax parser. It cuts the first parse of the bodies that never run: 4.5 MB of depth-0 bytes (18.44 - 13.91 MB) and a share of the rest. Upper bound at 106 instructions per byte: 480 M over the 47 journeys if the mode were free, so the real gain is under that (hypothesis: a preparse costs half of a parse, then about 240 M). It is a second parser to keep in step with the first one. Not now. After A the first parse is the largest lazy item that is left; measure it again then.

## 5. Recommendation

1. **Build A.** It removes the multiplier `d` (the cause on the two journeys), it keeps the memory gain of #492 (the table is 0.3%) and it needs no heuristic. Expected: -1,989 M on the 47 journeys (-2.1%; criterion 2 from 90.1% to about 88.2%), `wiktionary-portal` 1,561 to about 1,317 M, `wikipedia-portal` 1,234 to about 1,072 M. Build a prototype first and compare it with the table in section 4. If the measured gain is under 1.2 G, take D.
2. **Then build S** (one measure first: count swaps with the counters that are in `/data/bare-tmp/r2/R2-D/counters.patch`). Expected: -730 M more (criterion 2 about 87.5%). It also helps pages without lazy functions.
3. **Do not do B1, B2, B3 or C.** B1 is the cheapest in time and costs 61.9 MiB.
4. Limits that A makes unneeded or weaker: the `LAZY_MIN_BODY` and `LAZY_MIN_SOURCE` limits protect the second parse; after A the second parse is cheap and a lower `LAZY_MIN_SOURCE` can be tried (it keeps small scripts eager today because "a lazy function that runs is parsed twice and walked in a script of its own"). The `lazy_work` count in `lazy_load` counts second parses against `max_script_nodes`: after A it counts less work and can be lowered.

What remains after A and S, over the old cost: 6% to 8% on the two portals (the second parse of the own bytes of 655 functions: about 87 M on `wiktionary-portal`, 20 M of load, the stub tables). Criteria 2 (80%) and 4 (1.3 times) are not reached by this item alone: with every lazy cost gone (B1) criterion 2 is 86.8% and `python-org-docs-link` is 1.25 times.

## 6. Not measured

- A prototype of A or S. Their gain is arithmetic from the counters and the swap count (section 4).
- The swap count on 39 of the 47 journeys (the others have almost no lazy functions: 0 to 5 M of second parses).
- Peak RSS of A. Hypothesis: the same as main to 0.3 MB; the second parse allocates less (no inner nodes to drop), so its transient peak is lower.
- Live sites, other platforms, the byte lane of the VM (`exec_diff`), the module goal.
- The 23 merges between `0727ffef` and main that give `wiktionary-portal` +25 M at `nofn`.
- Relazification (SpiderMonkey) and the JSC cache details, beyond the sources linked in section 2.
