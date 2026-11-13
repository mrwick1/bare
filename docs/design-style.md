# Design: the style pipeline

Status: proposal. The owner reviews this document before any code is built.
Written on 2026-11-13 against main `3e72e9e6`. All measurements use the frozen commit `f6415364` on archMachine.
Web access worked. Every research claim has a source in section 2. The claims I could not check are listed in "What I did not verify" at the end.

## 0. Summary

The style pipeline turns CSS text and a DOM into computed styles. In JavaScript terms: it is `getComputedStyle` for every element, plus all the code that finds and loads the style sheets.

It costs 17.7% of all instructions on the 48 public journeys (32.2% on the ramps). The sheet loader adds 10.7%. CSS parse adds 8.9%. Together that is 37% of journey instructions.

My finding: the cost is not in one hot function. It comes from **four kinds of work that the design does not need**:

| # | Kind of work | Where it shows | Why the design does it |
|---|---|---|---|
| 1 | Tests of rules that cannot match (the rule is in the right bucket, but its ancestor is not there). | 96% of `compm_22000`, 94% of `rest_13` | The index looks at the **last** compound only. |
| 2 | Matching again for elements that look the same. | 28.6% of elements have a new signature (estimate, section 3.2). | Sharing happens **after** matching. |
| 3 | Whole-DOM walks to find sheets and links. | 23.9 M node visits for 217 k styled elements (43 journeys). | Nobody remembers where the `<link>`, `<style>` and `<base>` elements are. |
| 4 | Work for rule bodies and CSSOM objects that nobody reads. | 87% of rules never match (estimate). 10.2 KB per CSSOM rule object. | Parse and wrap everything at once. |

The recommended design removes each kind (section 4). Estimated gains, with the method in the sections that follow. All are estimates until built and measured on the frozen benchmark:

| Number | Now | After (estimate) |
|---|---:|---:|
| `compm_22000` instructions | 11.7 G | 0.6 to 1.0 G |
| `rest_13` instructions | 11.3 G | 0.5 to 1.0 G |
| Loader share of journey instructions | 10.7% | 1 to 2% |
| Matching share of journey instructions | 8.8% | 2 to 5% |
| CSS parse share of journey instructions | 8.9% | 3 to 4% |
| `sheety_16383` peak RSS (with the `cssRules` read) | 947 MiB | 270 to 300 MiB |
| Total journey instructions, all phases | 100% | about 80 to 85% |

The last row is the sum of the rows above it, weighted by the shares in section 1. It is an estimate with a wide error range.

## 1. How to read this document

Each idea has a short explanation and, where it helps, a JavaScript comparison. Each sub-problem has the same layout: what we do now, at least two other designs, a table that compares them, and a verdict.

Rules for the numbers:

- "Measured" means I ran it. The command or the source file is named.
- "Estimate" means I computed it from measured numbers. The method is written next to it.
- "Hypothesis" means I believe it and did not check it.
- Journeys ran against live sites on 2026-11-13. Sites change between runs. A journey that failed or returned no data in one run is not counted (5 of 48 had no data in the last run: `craigslist-home`, `devto-home`, `kernel-org-home`, `paulgraham-home`, and `github-docs-home` in the newest run).
- Instruction counts come from `instructions:u`. Wall time is not used, because the machine is shared.

### 1.1 Measured facts used here

Sources: `/data/bare-tmp/prof/cpu/report.md`, `/data/bare-tmp/prof/mem/report.md`, `/data/bare-tmp/bm/b/report.md`, `/data/bare-tmp/bm/a/report.md`.

| Fact | Value |
|---|---|
| Style/cascade, journeys (attributed) | 17.7% of instructions |
| Style/cascade, ramps | 32.2% |
| Loader (`tell_links`, `document_base`, `flatten`) on journeys | 10.7% (up to 30.5% of one journey) |
| CSS parse, journeys and ramps | 8.9% and 8.1% |
| `compound_matches`, `compute_kept`, `matches_from`, `subject_matches` (leaf) | 4.26%, 2.44%, 1.57%, 0.49% = 8.76% |
| `sources_capped`, `restyle` (leaf) | 1.15%, 1.14% |
| `compm_22000` (22,000 divs with a span, 6 classes each, mixed descendant rules) | 11.7 G instructions, 1.16 s, 96.2% style |
| `rest_13` (6,000 elements, 36,000 rules, 13 class changes) | 11.3 G instructions, 93.6% style. The step budget charges about 35 M steps per restyle. The real cost is about 65 ms. |
| `ComputedStyle` | 856 B (106 properties x 8 B + 8 B), `cascade.rs:2064` |
| CSS rule `.a{color:red}` at peak | about 478 B (selector about 230, `Entry` about 105, parse tree 72 + about 94) |
| `:is()` argument | 289 B |
| CSSOM rule object | 10.2 KB, cap `MAX_OBJECTS` = 65,536, so 671 MiB on a 16 MiB sheet (947 MiB RSS) |
| DOM node | 36 B struct, 56 B RSS |
| Median journey peak RSS | 18 MiB, of which 77% is the program image |

## 2. Research: how other engines do it

I read the sources named below. Where I could not open a source, the claim says "to verify".

### 2.1 Servo and Stylo (Firefox)

Source: [selector_map.rs](https://github.com/servo/stylo/blob/main/style/selector_map.rs), [bloom.rs](https://github.com/servo/stylo/blob/main/style/bloom.rs), [sharing/mod.rs](https://github.com/servo/stylo/blob/main/style/sharing/mod.rs), [invalidation_map.rs](https://github.com/servo/stylo/blob/main/style/invalidation/element/invalidation_map.rs), [rule_tree/mod.rs](https://github.com/servo/stylo/blob/main/style/rule_tree/mod.rs), [Mozilla Hacks, Quantum CSS](https://hacks.mozilla.org/2017/08/inside-a-super-fast-css-engine-quantum-css-aka-stylo/).

- **Rule index.** A `SelectorMap` has buckets by id, by class, by local name, and a rest list. A rule goes in the bucket of its **rightmost** compound. This is what bare does.
- **Ancestor filter.** `StyleBloom` is a counting Bloom filter. It keeps the element stack. When the traversal moves to a new element, it pops what it left and pushes the new path. This is what bare does (`AncestorFilter`).
- **Style sharing cache.** Before it matches an element, Stylo looks in a small cache of recent siblings and cousins. A candidate can share when tag, classes, id, attributes that selectors read, and parent style are the same. The cache stores bits ("revalidation selectors") for the selectors that depend on things the key does not cover. If those bits agree, Stylo copies the style and skips matching. The cache is small and per thread. (Details of the sizes: from the source file; I did not copy exact constants into this document, so treat them as "to verify".)
- **Rule tree.** The matched declaration blocks form a path in a shared tree. Two elements with the same matched rules point to the same node. The computed style is cached on the node. This makes the cascade "key is a pointer".
- **Invalidation.** An `InvalidationMap` stores `Dependency` records. Each one holds the selector, the offset of the compound that has the changed part, and the next dependency. A class change on an element looks up only the selectors that contain that class, then tests the **rest of the selector** on the element and on the relations (descendant, child, sibling). Most changes restyle one element or a small part of a subtree.
- **Parallelism.** The traversal is parallel with work stealing. Mozilla reports large speedups on multi-core machines. bare runs on one thread for the style pass, so this does not apply now.

### 2.2 Blink (Chrome)

Source: [rule_set.h](https://chromium.googlesource.com/chromium/src/+/refs/heads/main/third_party/blink/renderer/core/css/rule_set.h), [style-invalidation.md](https://chromium.googlesource.com/chromium/src/+/refs/heads/main/third_party/blink/renderer/core/css/style-invalidation.md) (I read it through a mirror), [chromium memory-dev thread](https://groups.google.com/a/chromium.org/g/memory-dev/c/aymCU6DDw0E).

- **RuleSet.** `RuleData` is compact and stores a few hashes of the ancestor identifiers inline. This is the same idea as bare's `ancestors: [u32; 4]`. Rules go in maps by id, class, tag, attribute, and so on. After a build, the maps are compacted into flat arrays.
- **Invalidation sets.** For each feature (class, id, attribute), Blink builds an **invalidation set**: which classes, ids, tags and attributes can be affected below or after it. A class change reads the set and marks only elements that have a matching feature. A set can be "whole subtree" when Blink cannot be precise. Blink is conservative on purpose.
- **Matched properties cache.** Blink caches the result of the cascade by the list of matched blocks and the parent style.
- **ComputedStyle.** It is split into groups. Groups are reference counted and copied on write. Elements share groups, not whole styles. A Blink engineer says in the memory-dev thread that sharing whole styles between elements was removed because it was error prone without enough gain. (I did not find a second source for this, so it is a quote of one person.)
- **Lazy declaration parsing.** Blink has `CSSLazyPropertyParser`. It keeps the text of a declaration block and parses it when a rule matches the first time. Source: search result and my memory of the class; I did not open the file. To verify.

### 2.3 WebKit

- `SelectorFilter` is a counting Bloom filter for ancestors. Each rule has up to 4 hashes. (From a search result; to verify against the source.)
- WebKit has a style sharing resolver and a selector JIT. I could not open `StyleSharingResolver.cpp` (the page returned an error). **To verify**: the sibling and cousin limits.

### 2.4 Ladybird

Source: [PR 13655, new style engine](https://github.com/LadybirdBrowser/ladybird/pull/13655), [PR 13724](https://github.com/LadybirdBrowser/ladybird/pull/13724).

- A rule index with a **subject key** and **ancestor keys**. The key of a rule comes from the subject compound. Ancestor keys are used to reject a rule early.
- Selector components are 8 bytes in an arena. Computed values are grouped. Groups are shared by pointer.
- A sharing cache, and parallel passes.
- Reported result: RSS down by 31.8% and the style benchmark up by 26.9%.
- A lesson from the follow-up: precise differential bookkeeping (what exactly changed) cost more than it saved. This supports not building the most precise invalidation first.

### 2.5 Papers

- Meyerovich and Bodik, "Fast and Parallel Webpage Layout", WWW 2010: selector matching is a main cost of layout and can be parallel. I could not read the PDF (the fetch returned binary data). I only use the abstract-level claim. To verify the numbers.

### 2.6 What I take from the research

| Idea | Used in | Do we copy it? |
|---|---|---|
| Rightmost-compound buckets | Stylo, Blink, Ladybird, bare | Already done. |
| Counting Bloom filter for ancestors | all | Already done. |
| Ancestor key inside a big bucket | Ladybird | Yes (section 3.1, option M2). |
| Style sharing before matching | Stylo, WebKit | Yes, but with an interned signature (M3). |
| Rule tree | Stylo | No (3.2, option S3). |
| Dependency-based invalidation | Stylo | Later, only if the numbers ask for it (3.3). |
| Invalidation sets | Blink | No (3.3). |
| Lazy declaration parsing | Blink | Yes (3.4). |
| Copy-on-write style groups | Blink, Ladybird | No for now (3.2, option S4). |
| Parallel traversal | Stylo, Ladybird | Out of scope. |

## 3. The sub-problems

### 3.0 The current design

All file names are in the repo at `3e72e9e6`. The code at the frozen commit `f6415364` is the same for `cascade.rs` and `matching.rs` (I compared them).

**Parse.** `parse_stylesheet` (`crates/css/src/parser.rs:95`) tokenizes the whole text and builds a tree of `Rule` (72 B, `parser.rs:43`) with a `Vec<Declaration>` (48 B each, `parser.rs:82`). The tokenizer reads one `char` at a time. Selectors and values are parsed in the same pass.

**Rule storage.** `RuleSet::add_rules` (`cascade.rs:691`) makes one `Entry` per selector (`cascade.rs:119`). It puts the entry number in an `Index` (`cascade.rs:146`): five hash maps from key to `Vec<u32>` (`by_id`, `by_class`, `by_tag`, `by_attr`, and `other`). The key comes from the **last** compound of the selector. An `Entry` also keeps `ancestors: [u32; 4]`, the hashes of up to 4 ancestor identifiers for the Bloom check.

**Matching.** For each element, `compute_kept` (`cascade.rs:810`) calls `candidates` (`cascade.rs:1230`). That gathers the lists for the element's id, classes, attributes, tag, and `other`. For each candidate:
1. Test the 4 ancestor hashes against the counting Bloom filter `AncestorFilter` (`cascade.rs:2538`, 4096 `u8` counters).
2. If it passes, run `matches` right to left (`matches_from`, `matching.rs:76`; `compound_matches`, `matching.rs:126`).

Cost: O(elements x candidates in the buckets of the element). Memory: 4 KiB for the filter.

**Cascade and sharing.** `style_of` (`cascade.rs:1238`) uses a memo from `(parent style id, matched rule list, hint)` to a style id (`Memo`, `cascade.rs:1564`). New styles are also deduplicated by hash. So two elements with the same matched list and the same parent style get the same style id. A `ComputedStyle` is 856 B (`cascade.rs:2064`). Inheritance copies from the parent style. **Sharing happens after matching.** Matching runs for every element.

**Invalidation.** `Scopes` (`cascade.rs:367`) records, for each class, id and attribute that a rule reads, the widest scope that a change can reach: `Own`, `Subtree` or `Siblings`. `RuleSet::roots` (`cascade.rs:502`) turns a set of change marks into restyle roots. `restyle_with` (`cascade.rs:929`) recomputes every element under each root. The restyle step charge is 64 steps per element plus the candidates tested (`STEPS_PER_RESTYLED_ELEMENT`, `browser_computed.rs:224`).

**Reuse.** `Built::last` keeps the last cascade for geometry reads (see "Cascade reuse for geometry reads" in `decisions.md`).

**CSSOM.** `browser_cssom.rs` builds, for each rule, a JS object. `wrap_declaration` (`browser_cssom.rs:982`) copies about 130 accessors onto each declaration object. This costs 10.2 KB per rule. The cap `MAX_OBJECTS` = 65,536 (`browser_cssom.rs:15`) stops a page from using more than 671 MiB.

**Sheet loading and linking.**
- `plan` (`browser_computed.rs:343`) walks the whole DOM with `sources_capped` (`cascade.rs:2658`) and copies all sheet text, on every revision change.
- `document_base` (`resources.rs:350`) walks the whole DOM for `<base>`.
- `tell_links` (`browser_loader/sheets.rs:405`) walks the whole DOM for each sheet that arrives, to find the `<link>` elements with the same `href` and fire `load`.
- `feed_styles` (`sheets.rs:161`) and `flatten` (`sheets.rs:316`) copy and rewrite the sheet text (`absolute_urls`, `sheets.rs:456`).

Cost: O(sheets x nodes) per page, and O(passes x nodes) for the base and source scans.

**Table: complexity now.**

| Part | Time | Memory |
|---|---|---|
| Parse | O(bytes), about 182 instructions per byte for tokenize + parse + add (measured, section 3.4) | 286 B per rule for the tree, 478 B per rule at peak |
| Index build | O(rules) | `Entry` about 105 B + selector about 230 B per rule |
| Matching | O(elements x candidates per bucket) | 4 KiB filter |
| Cascade | O(distinct matched lists) with a memo | 856 B per unique style |
| Restyle | O(elements under the root) x candidates | none |
| Sheet scans | O(sheets x nodes) | text copies |
| CSSOM | O(rules read), 10.2 KB each | capped at 65,536 objects |

### 3.1 Sub-problem A: finding the rules that match an element

#### Where the time goes (measured)

I built an instrumented copy of `f6415364` under `/data/bare-tmp/design/style/` (counters only, no logic change except estimates). I ran the 48 journeys with `BARE_STYLE_STATS=1` (script `runj.sh`, aggregate `agg2.py`). 43 journeys returned data.

| Measure (43 journeys, 216,899 elements in full walks) | Value |
|---|---:|
| Candidates tested | 7.94 M, 36.6 per element |
| Candidates that pass the Bloom filter | 2.26 M, 28.5% of candidates |
| Candidates that match | 0.51 M, 22.5% of those that pass, 6.4% of all |
| Bucket of the candidates (walks and restyles together, 12.95 M) | `tag` 54%, `other` 38%, `class` 5%, `attr` 3.5%, `id` 0.02% |
| Per-journey candidates per element | median about 10. Extremes: `nextjs-docs` 236, `substack-home` 226, `python-org-*` 155, `w3c-home-links` 156, `mdn-reference` 70 |

Reading: **93.6% of the tested candidates do not match.** Most of them are rejected by the ancestor filter. The work of rejecting is the cost.

Ramps (same counters, `rawr/`):

| Page | Elements per pass | Candidates per element | Passes | Candidates | Matched | Instructions per candidate (estimate) |
|---|---:|---:|---:|---:|---:|---:|
| `compm_22000` | 44,004 | 3,669 | 3 | about 484 M | 0.2 M | 11.7 G / 484 M = 24 |
| `rest_13` | 6,004 | 5,999 | 13 | about 468 M | 0.2 M | 11.3 G / 468 M = 24 |

Why `compm` is quadratic: N/3 rules look like `.cX span`. They sit in `by_tag[span]`. Each of the N spans tests all N/3 candidates. The Bloom filter rejects nearly all of them (0% pass in the table above), but each reject still costs about 24 instructions. This is O(N^2). The body class change makes `body.a .cX` read `body`'s class, which gives the scope `Subtree`, so the page is styled about 3 times per run.

Blink and Stylo also bucket `.cX span` by the tag `span` (section 2). They would also see a big bucket on this page (hypothesis: I did not run them). Fixing it needs a better index, not a copy of their index.

#### Options

**M1. Keep the index. Make the test cheaper** (the "10% on one row" kind of change).
- Data: same. Algorithm: pack `ancestors` hashes into a 32-bit Bloom mask per entry, test with one `&`. Blink does something close.
- Complexity: still O(elements x candidates). The constant drops from 24 to maybe 6 instructions.
- Memory: +0 (the entry already has 16 B for the hashes).
- Gain on `compm_22000`: 4x at best (estimate). Still quadratic: N=44,000 gives 4x more time than now.
- Verdict: **no**. It makes the work cheaper. The quadratic term stays and the limits stay.

**M2. Ancestor-keyed second level for big buckets.**
- Idea in JS terms: instead of `rules.filter(r => r.matches(el))` over a long list, use `rulesByAncestorClass[ancestorClass]` for each ancestor class that really exists above the element. You do not look at rules whose ancestor is not there.
- Data: when a bucket (for example `by_tag[span]`) has more than 16 entries, split it. Each entry gets an `anc_key: u32`. This is the hash of the rarest identifier in its compounds to the left of the subject (class, id or tag; a plain `descendant` or `child` combinator). Entries with no usable ancestor key stay in a `rest` list. The split has a flat layout: `keys: Vec<(u32 key, u32 start, u32 len)>` sorted by key, and one `Vec<u32>` of entry numbers (4 B per entry).
- Algorithm: the traversal keeps a stack of the identifier hashes of the current path (it already computes these for the Bloom filter in `element_hashes`, `cascade.rs:2592`). For a split bucket, for each hash on the path, binary search (or a small hash table) in `keys`. Test the entries of the range with the full matcher. The Bloom check is still done. Then test the `rest` list.
- Complexity: O(path hashes x lookup + entries whose ancestor key is on the path + rest) per element per big bucket. For `compm`: path hashes are about 8 (the div has 6 classes and a tag, the body and html add a few) so about 20 lookups, plus one or two candidates. About 30 candidates instead of 3,669.
- Memory: +4 B for `anc_key` in `Entry` (padding probably hides it: to verify with `size_of`), +4 B per entry in the flat list, +12 B per distinct key. At most +20 B per rule on a 478 B rule (+4%). Only for big buckets.
- Work removed: the 99.99% of candidates that the Bloom filter rejects. The Bloom filter is not needed for the entries in a split bucket (but it is still needed in the `rest` list and in small buckets).
- Risks: (1) correctness: an entry must be reachable through exactly one key, and every ancestor on the path must be tested. A fuzz test that compares M2 with the current matcher on random trees and rule sets is enough to guard this. (2) The `:is()`, `:not()`, `:has()` and shadow DOM cases: an entry with these in an ancestor compound goes to `rest` (no key). (3) `rest` can become the new big list (hypothesis: on real pages, `*` and attribute rules are in `other`; section 3.1 shows that `other` holds 38% of the candidates, so `other` needs the same split with the key from the leftmost useful ancestor).
- Expected gain on our numbers (estimate):
  - `compm_22000`: 484 M candidates to about 44,004 x 3 x 30 = 4 M. At 24 instructions each that is 0.1 G instead of 11.6 G. Add the cost of the path lookups (about 100 instructions per element and pass, 13 M instructions in total) and the rest of the page (0.4 G for parse and layout, from the ramp split). **About 0.6 G in total, 19x lower.**
  - `rest_13`: 468 M to about 6,004 x 13 x 30 = 2.3 M. **About 0.5 to 1 G in total.**
  - Journeys: candidates per element median about 10, so most elements have small buckets and gain little. The gain is on the long tail (`nextjs-docs` 236, `substack` 226, `python-org` 155). Estimate: the matching share of 8.8% drops by about 40% from M2 alone (that is 3.5 points of journey instructions). This is a guess: I did not measure how many of the 7.94 M candidates sit in buckets above 16 entries.

**M3. Interned element signature (share the matched list across elements).**
- Idea in JS terms: a `Map` from "what this element looks like to CSS" to "the list of matched rules". Two `<li class="item">` under the same parent look the same to CSS, so the second one does a map lookup instead of matching.
- Data: an `u32` signature per node. `sig(el) = intern((sig(parent), own_key))`. `own_key` is the interned tuple of: tag atom; the **sorted** set of class atoms (interned to one `u32`); the id (only if some rule reads that id); for each attribute name that some rule reads, a flag that it exists; and for each attribute selector, the result (true or false) of testing it on this element; for each structural selector (`:first-child`, `:nth-child(an+b)`, and so on) the result of testing it. The signature table maps `(sig(parent), own_key)` to a `Sig { matched: Range<u32> }` in an arena.
- Algorithm: in the walk, compute `sig(el)`. If new, run the matcher once and store the matched list in the arena. Otherwise reuse the list. The memo in `style_of` then gets the matched-list number directly.
- Complexity: O(elements x cost of own_key) + O(distinct signatures x candidates). `own_key` is a few hundred instructions at most (class split and sort; I estimate 100 to 200 instructions per element, **hypothesis**, not measured).
- Memory: `Vec<u32>` of signatures per node: 4 B per node (+7% of the 56 B per node), kept during the walk. The table: about 24 B per distinct signature. The arena: about 17 B per distinct signature (2.35 matched rules per element, 4 B each, plus an 8 B range). Total about 16 B per element at the measured 28.6% (estimate). It can be freed after the walk.
- Work removed: all matching for elements with a known signature.
- Measured by the estimator (counter 12, 13, 28, 29; the code is in the instrumented copy, `cascade.rs`, `stat::Share`):

| Measure (43 journeys, per full walk) | Value |
|---|---:|
| Elements that hit a known signature | 71.4% |
| Of those, the stored matched list equals the real one | 99.86% |
| Candidates tested only for first-seen signatures | 47.2% of all candidates |
| Filter-passed candidates for first-seen signatures | 40.6% |
| Per journey: new-signature share | `hn-front` 5%, `wikipedia-article` 20%, `python-docs-tutorial` 42%, `mdn-reference` 49%, `substack-home` 55%, `nextjs-docs` 80%, `python-org-search` 94% |

  The 0.14% of hits that are **not** valid shows that the signature misses something. Where: I did not find the cause for every case. **Hypothesis**: selectors that read text, `:lang()`, `:checked`, `:disabled` or form state, that the estimator did not hash. A real build must list every input of the matcher (see risks).

  **A bug in my first estimate.** My first run hashed the raw sibling index for any page with `:nth-child` or `:first-child`. That made every sibling different and gave 5.7% hits. The estimate was wrong, not the idea. The fix hashes only the result of each structural selector that rules actually use. This gave 71.4%. I record it here so the owner knows the first number was an error.

- Risks: (1) Every input of the matcher must be in the signature or in a revalidation bit. Missing one gives wrong styles, which is the worst kind of bug. Stylo has the same risk and handles it with revalidation selectors. (2) Pages with unique class names (CSS-in-JS, hashed class names: `nextjs-docs` 80% new) get little sharing. The cost on these pages is the extra `own_key` work (100 to 200 instructions per element; hypothesis). (3) Many `:has()`, `:is()` with complex arguments, or rules that read the parent's inline style break signatures. Those rules should force the element to run the matcher.
- Expected gain on our numbers (estimate): candidate work x 0.47 for the walk. With signature work added: per element now about 36.6 x 25 = 900 instructions (average). After: 0.286 x 900 + 150 = 410, so **about 54% lower** (this uses my 150 guess).
- On `compm`: **no gain** (every div has a unique class set, so 100% new signatures). M3 does not fix the quadratic page. M2 does. This is why I recommend both.

**M4. Rule tree as the key (Stylo).**
- Data: a tree of matched blocks. Matching still runs for every element.
- Verdict: **no**. It helps the cascade (which already hits the memo 90.9% of the time, measured: 301,931 hits and 30,291 misses). It does not remove matching.

**M5. Compile selectors to a bytecode or to machine code (WebKit JIT).**
- Complexity: the constant in the matcher drops. The number of tests does not.
- Verdict: **no**. It makes the work cheaper (10 to 30%, **hypothesis**). It adds code and an executable-memory risk.

#### Compare

| | Candidates (compm_22000) | Journey matching work | Memory | Risk | Verdict |
|---|---:|---:|---|---|---|
| M0 now | 484 M | 100% | 0 | none | |
| M1 cheaper test | 484 M x 0.25 | 100% x 0.7 (estimate) | 0 | low | no |
| M2 ancestor keys | about 4 M | about 60% (estimate) | +4% per rule | medium | **yes** |
| M3 signature | 484 M (no change) | about 46% (estimate) | +16 B per element during a walk | high | **yes, after M2, if the numbers hold** |
| M2 + M3 | about 4 M | about 25 to 35% (estimate, not additive) | both | | **recommended** |
| M4 rule tree | no change | no change | tree | medium | no |
| M5 JIT | no change | about 75% | code | high | no |

The M2 and M3 gains overlap: both remove candidates that the Bloom filter would reject. I will measure M3 after M2 is built. If M2 already removed most of the work, M3 may not pay for its risk.

### 3.2 Sub-problem B: computed-style sharing and memory

The cascade is already shared. The memo hit rate is 90.9% (301,931 hits and 30,291 misses over 43 journeys). A miss costs one cascade (about 856 B). Per journey that is about 700 unique styles (30,291 / 43, estimate), or 0.6 MB. So computed styles are not a main cost in memory on the journeys. They matter for stress cases only (`css-unique-styles`: 53.5 MiB at 50,000 unique styles).

| Option | Data | Gain | Verdict |
|---|---|---|---|
| S1 now: memo `(parent style, matched list, hint)` to style id, deduplicated by hash | `Memo`, `cascade.rs:1564` | already 90.9% hit | keep |
| S2 signature to matched list (M3 above) | `u32` per node | removes matching, not cascade | yes (M3) |
| S3 rule tree (Stylo) | tree of blocks, style cached in nodes | cascade cost, not matching | no, the memo does the same job |
| S4 copy-on-write style groups (Blink, Ladybird) | `ComputedStyle` split into about 10 groups of properties, each shared by pointer | 856 B becomes about 100 B + shared groups for unique styles (estimate) | **later**: the unique style count on journeys is small (700 per journey), so the gain is under 1% of RSS on the median journey. It matters on `css-unique-styles`. Build only if a journey with many unique styles asks for it. |

Verdict: no new design in this area for now. This is a finding, not a skipped step.

### 3.3 Sub-problem C: invalidation (DOM, class and attribute changes)

**What happens now.** `Reach` (`cascade.rs:265`) says, for each class, id and attribute that a rule reads, the widest scope of effect. The scope is `Own`, `Subtree` or `Siblings`. A class change on `body` with a rule `body.a .c1` gives `Subtree`. `restyle` then recomputes all elements under `body`: the whole page.

**Measured.** In the 43 journeys, restyles recomputed 115,323 elements. Full walks computed 216,899. So restyles are 35% of all element-style computations, but concentrated: `wikipedia-article` 85,497, `wikipedia-main-random` 15,246, `python-docs-tutorial` 5,225, `python-org-docs-link` 4,425, `nextjs-docs` 2,319. 44 restyles in total.

The restyle step charge is 64 steps per element plus candidates. For `rest_13` it charges about 35 M steps for a pass that costs about 65 ms. Per the benchmark report, I did not find which term dominates. **Hypothesis**: the candidate term, because the buckets are long. With M2 the candidate term becomes small and the charge matches the cost.

| Option | Data and algorithm | Gain | Risk | Verdict |
|---|---|---|---|---|
| I1 now: three scopes | `Reach`: class/id/attr to widest scope | the base | none | |
| I2 dependency map (Stylo) | `Dependency { selector, offset, next, kind }` for each selector part. A class change on `body` finds `body.a .c1`, then tests which descendants can match the part to the right. Restyle only the elements that match. | `rest_13`: a class change on `body` restyles only the elements that a rule `body.a .cX` can reach, not the page. About 1/3 of the elements of that page (estimate: 1 of 3 rule kinds reads `body.a`). Without M2 that is 3x. With M2 the pass is already cheap, so the gain is on top: about 3x of a small number. | High. Needs a second matcher entry point that tests "the rest of the selector" and the sibling cases. | **later**, only if journeys with big restyles still show up after M2 and M3 |
| I3 invalidation sets (Blink) | Per feature, a set of classes, tags and ids that can be affected. | Similar to I2. | Medium. A conservative design: when unsure it falls back to the whole subtree. | no, same gain as I2 with another data structure |
| I4 no precision, cheap full pass | Keep the scopes. Make a full pass cheap with M2 and M3. | `rest_13` pass: from 0.87 G to about 0.04 G (estimate: 6,004 x 30 candidates x 24 + signature work). | None new. | **yes**. This is "make the work unneeded" for rejected candidates, and it is what Ladybird did after its precise bookkeeping cost more than it saved (PR 13724). |
| I5 charge by real cost | Count real work (candidates tested that passed the filter and elements) in the step charge. Remove the 64 steps per element. | The charge matches the time. A page may restyle more times before it hits the budget. | Low. | **yes, after M2**: lower the budget to the real cost. |

Verdict: do I4 and I5 with M2 and M3. Do I2 only if the numbers ask for it.

### 3.4 Sub-problem D: CSS parsing

**Measured** (the example `crates/css/examples/lazy.rs` in the instrumented copy; instruction counts with `perf stat -e instructions:u`, one machine, 1 run each, the counts repeat within 0.2%):

| Input | Bytes | Tokenize only | Parse tree (`parse_stylesheet`) | Parse + build entries (`add`) | Skip-scan (finds rule boundaries only) |
|---|---:|---:|---:|---:|---:|
| `bootstrap.min.css` | 232,803 | 17.5 M | 22.4 M | 42.5 M | 2.1 M |
| `bootstrap.css` | 281046 | not recorded | 27.5 M | 47.7 M | 2.56 M |
| `fontawesome.css` | 140798 | not recorded | 15.6 M | 27.0 M | 1.38 M |

For `bootstrap.min.css`: `add` costs 182 instructions per byte. The skip-scan costs 9 instructions per byte, so it is **10.6 times cheaper than `parse_stylesheet`** and 20 times cheaper than `add`. The bench numbers from `/data/bare-tmp/bm/a/`: tokenize 6.89 MB takes about 60 ms (8.7 ns per byte), parse about 115 ms, parse with values about 175 ms, adding 10,000 rules about 16 ms.

**How many rule bodies are used.** The new counter (`stat` 30 and 31) counts the entries that matched at least one element in a full walk, compared with all entries in that walk. Over 12 journeys: **12.8% of the entries ever match** (14,119 of 110,423; range 4.9% on `github-docs-home` to 25.8% on `debian-org-home`). The entries include all walks of the journey, so a rule counts once per walk. This is a proxy. Rules in `@media` that do not apply are not in `entries`, so the real share of used rules in the text is lower or equal. Rules under a different state (`:hover`) are used later and not counted. **So the number is a lower bound for the share of bodies that are used on first style.**

| Option | Data and algorithm | Complexity and memory | Gain (estimate) | Risk | Verdict |
|---|---|---|---|---|---|
| P1 now | Eager tree, `Vec<Declaration>`, all values parsed | 182 instr/B, 286 B per rule for the tree | | | |
| P2 lazy bodies with a skip scanner | One pass finds `selector { ... }` boundaries with a byte table (memchr-like). Selectors are parsed at once (needed for the index). The body stays as a `(start, end)` span (8 B). It is parsed on the first match, or when CSSOM reads it. | Time: 2.1 M + selectors + 13% of the body cost. Memory: a rule becomes selector + 8 B instead of selector + 72 B + 94 B. | `bootstrap.min.css`: 42.5 M to about 12 M (3.5x). I get 12 M from: 2.1 M (scan) + about 8 M (selectors; I did not measure this part, **hypothesis**: about 40% of the 20 M gap between `add` and the parse tree) + 13% x 22 M for bodies ... = 2.1 + 8 + 2.9 = 13 M. Journeys: CSS parse 8.9% to about 3 to 4%. Memory: rule tree 286 B to about 100 B per rule (estimate). | Medium. Error handling must match the eager parse exactly (a bad declaration inside a block must not hide the rule). Rules with `@import`, `@layer`, `@media`, `@supports` and nesting must still be parsed to find the structure. CSSOM reads need the body (they trigger the parse). | **yes** |
| P3 streaming: no tree. The parser pushes entries directly. | Remove the `Rule` tree. | The 72 B `Rule` tree node is not built. | Saves 20 to 30% of memory of a rule (estimate). | Medium. The `@layer` and nesting code now reads the tree. | **maybe**, with P2 |
| P4 faster tokenizer | Byte tables, `memchr` for comments and strings, skip of `char` decoding in ASCII runs. | The same data. Time: tokenize 8.7 ns/B to maybe 3 ns/B (**hypothesis**; I did not build a prototype). | `Tokens::name` is 2.25% and `next_spanned` 1.27% of journey instructions. Maybe a half of these 3.5 points. | Low. | **yes**, small change, same phase as P2 |
| P5 parse in a background thread | | | | Out of scope. | no |

Comparing with Blink: Blink has lazy parsing of declaration blocks (section 2.2, not opened). The skip scanner is the same idea. We reuse the idea, not the code.

### 3.5 Sub-problem E: CSSOM objects

**Now.** Each CSSOM rule object gets about 130 own accessor properties by `wrap_declaration` (`browser_cssom.rs:982`). One object costs 10.2 KB (671.3 MiB / 65,536, from the memory report). The cap `MAX_OBJECTS` stops growth. A page that reads `document.styleSheets[0].cssRules` on a sheet over 4 MiB gets `cssom-objects` refused (benchmark report).

In JS terms: each rule has its own copy of 130 getters instead of one shared prototype.

**Measured:** `sheety_16383` with the `cssRules` read uses 947 MiB. The same page without that read used **256.3 MiB** (5 of 6 runs in the memory report). So the CSSOM wrappers cost 690 MiB, and the rest of the sheet (parse, style, copies) costs about 256 MiB.

| Option | Data and algorithm | Memory per rule object | Gain | Risk | Verdict |
|---|---|---:|---|---|---|
| C1 now | own accessors on each object | 10.2 KB | | | |
| C2 accessors on a shared prototype | The declaration object has one internal slot (rule number, index into the sheet). Getters and setters live on one shared prototype object (one per sheet kind). | about 100 to 300 B (an object of 84 B + a slot, from the JS object cost in the memory report; estimate) | 65,536 objects x 10.2 KB = 671 MiB to 65,536 x 0.3 KB = 20 MiB. `sheety_16383` 947 MiB to about 280 MiB (256 + 20). | Medium. JS code can observe own properties (`Object.keys(rule.style)`, `hasOwnProperty`). The first draft said that Chrome has them on the prototype. That was wrong. Measured on Chrome (2026-11-13): the properties are own, enumerable properties of each `CSSStyleDeclaration`, and `Object.keys(rule.style)` lists them. bare now matches its own `el.style` (shared prototype, no own keys), not Chrome. See `decisions.md`, "CSS rule declarations share one prototype". Needs the JS engine to support accessor properties on a prototype with an internal slot. This depends on `design-js.md`. | **yes** |
| C3 lazy materialization | A rule object is created when JS reads `cssRules[i]`, not when `cssRules` is read. A weak cache keeps identity (`cssRules[3] === cssRules[3]`). | 0 for rules never read. | A page that reads `cssRules.length` or one rule pays for one object. | Medium: identity needs a cache. | **yes**, with C2 |
| C4 raise or remove the cap | | | The cap hides the cost. With C2 and C3 the cap can go up to the number of rules (the rest costs 478 B per rule). | | **after C2** |

### 3.6 Sub-problem F: sheet loading and linking

**Measured** (counters in `sheets.rs` `tell_links`, `resources.rs` `document_base`, `browser_computed.rs` `elements_under`/`plan`; 43 journeys):

| Walk | Nodes visited | Calls |
|---|---:|---:|
| `tell_links` (once for each sheet that arrives) | 15.76 M | 11,259 |
| `sources_capped` (inside `plan`) | 4.73 M | |
| `document_base` | 3.44 M | |
| Sum of the three | **23.9 M** | |
| Elements styled in full walks | 0.217 M | |

So the three scans visit 110 times more nodes than the cascade styles. The CPU report agrees: `tell_links` 4.97%, `document_base` 1.00%, `sources_capped` 1.15%, `flatten` 0.54% of journey instructions. Worst journeys: `rust-book-search` (8.16 M `tell_links` node visits in the newest run), `wikipedia-search` (1.04 M), `python-org-search` (20.9 M in an earlier run and 1.66 M in the newest run: the site changes).

Why it is quadratic: a page has S sheets and N nodes. Each sheet that arrives calls `tell_links`, which walks N nodes. Script-inserted content makes N large when the sheets finish (hypothesis, from the CPU report; not measured per page).

| Option | Data and algorithm | Complexity | Gain (estimate) | Risk | Verdict |
|---|---|---|---|---|---|
| L1 now | Walk the DOM when needed | O(S x N) | | | |
| L2 dirty marks: remember the answer until a relevant node changes | The document keeps a small set of "sheet-relevant" node ids (`<link>`, `<style>`, `<base>`), updated on insert, remove and attribute change of those elements (the DOM already has a mutation path: the `Mark` list for restyle, `cascade.rs`). `plan`, `document_base` and `tell_links` read this list. | O(S x L) where L is the number of such nodes (usually under 30) | The walk cost goes to about 0: 23.9 M node visits to under 0.1 M. The loader share 10.7% (7.7% leaf) to about 1 to 2% (the rest is `flatten`/`feed_styles` copies). | Low to medium. The set must be exact. A missed mutation (for example `innerHTML` that adds a `<link>`, a parser-inserted node, a moved subtree, a shadow tree) gives a missed sheet. Test: after each mutation in the test suite, compare the set with a full walk (debug assertion). | **yes** |
| L3 event registry: `HashMap<href, Vec<NodeId>>` of `<link>` elements | The map is filled when a `<link>` is inserted and its `href` is set. | O(1) per sheet | Same as L2 for `tell_links`. It does not help `plan` or `document_base`. | Same as L2. | **include in L2** |
| L4 text copies | `plan` copies all sheet text on each revision. `flatten`/`absolute_urls` rewrite URLs in the sheet text (4.7 MiB on `github-docs-home`). | | Keep the text once, and rewrite URLs when the value is read (the `url()` token), not for the whole text. Memory: `absolute_urls` 4.7 MiB of 12 MiB style on `github-docs-home` (memory report). | Medium. | **later**, with P2 (lazy values) |

Blink keeps pending sheets and an "active style sheet" list in the style engine and updates them on events. Stylo has a `StylesheetSet` with dirty flags. I did not open these files. **To verify** before copying details. The principle (events, not scans) does not depend on the details.

## 4. The recommended design

Name: **Style pipeline v2**. Five parts. Each removes a kind of work from the table in section 0.

| Part | Sub-problems | What it is |
|---|---|---|
| 1. Sheet registry | F (L2, L3) | The document remembers its `<link>`, `<style>` and `<base>` nodes. No DOM scans for sheets. |
| 2. Ancestor-keyed index | A (M2), C (I4, I5) | Big buckets are split by an ancestor key. The step charge counts real work. |
| 3. Signature sharing | A (M3), B (S2) | Interned signatures share the matched list between like elements. |
| 4. Lazy rule bodies and a faster tokenizer | D (P2, P4, maybe P3) | Parse a rule body when it first matches. |
| 5. Cheap CSSOM objects | E (C2, C3, C4) | Accessors on a shared prototype. Objects on demand. |

### 4.1 Phases

The order follows the gain per risk, and each phase can be measured alone. After each phase, run the frozen benchmark again on the whole browser (48 journeys, the ramps, the cost matrix), 3 runs each, on a rebased main, and record the numbers in `docs/progress.md`.

| Phase | Build | Moves these numbers | Estimated change | How to measure |
|---|---|---|---|---|
| 1 | Sheet registry (part 1) | Loader share of journeys. `python-org-docs-link` (30.5% loader). `rust-book-search` (24.9%). The `tell_links` counters. | Loader 10.7% to 1 to 2%. Journeys total about -8 to -9 points. The worst journeys: -20 to -30 points. | Same 48 journeys; instruction counts before and after; also the node-visit counters in the instrumented build. |
| 2 | Ancestor-keyed index and the real-work charge (part 2) | `compm_22000`, `rest_13`, `comp_*`, journeys with 100+ candidates per element. | `compm_22000` 11.7 G to about 0.6 to 1.0 G. `rest_13` 11.3 G to about 0.5 to 1.0 G. Journey matching about -3 to -4 points. | Ramps (3 runs each, instructions), cost matrix, 48 journeys. Add a fuzz test: random tree and rules, old matcher against new matcher, must return the same list. |
| 3 | Signature sharing (part 3) | Journey matching. `wikipedia-*`, `hn-*`, `lobsters-*`, `python-docs-tutorial`. | A further -1 to -3 points on journeys, if M2 left enough work. **Decide after phase 2 with a new measurement.** | Same. Count `own_key` cost with a micro-benchmark. |
| 4 | Lazy bodies, faster tokenizer (part 4) | CSS parse share. `docs-rs-home` (40% parse), `debian-org-home` (32%), `github-docs-home` (27%). Peak RSS on rule-heavy pages. | CSS parse 8.9% to 3 to 4%. Journeys about -5 points. Rule memory about 478 B to about 250 B (estimate). | `bare-bench` css rows, 48 journeys, `bare-stress scale css` memory. |
| 5 | Cheap CSSOM (part 5, depends on the JS side) | `sheety_16383` and `sheet_4096` peak RSS and time. | 947 MiB to about 280 MiB. | Ramps with the `cssRules` read (`run2.steps`), RSS from `wait4`. |

Total on the journeys: the sum is about -17 to -21 points, so **about 80 to 85% of today's instructions** (estimate). The error range is large: M3 and M2 overlap, and the CPU shares come from one set of 144 runs.

### 4.2 Limits that the design lets us lower or remove

A limit that protects against a slow algorithm is a sign of a design problem (from `CLAUDE.md`). I list what each phase makes unneeded. I do not change any limit in this document.

| Limit | Where | Phase | What to do |
|---|---|---|---|
| `computed-work` (nodes x rules over 2^30) | `browser_computed.rs` `ComputedLimits` | 2 | Lower the cost model to real work. Today it refuses `comp_34000` and `compm_24000`. After M2, `compm` grows about linearly. Raise the point at which it refuses, or charge elements + matched rules only. |
| `STEPS_PER_RESTYLED_ELEMENT` = 64, plus candidates | `browser_computed.rs:224` | 2 | Charge the real work. Today 14 restyles of `rest` hit the step budget after 1.05 s. |
| `computed-rules` | `ComputedLimits` | 4 | The limit counts rules. With lazy bodies a rule costs less. Lower the per-rule weight. |
| `cssom-objects` / `MAX_OBJECTS` 65,536 | `browser_cssom.rs:15` | 5 | Remove or raise to the rule count. |
| `css-rules` memory limit (stress, n = 10 M), `css-selector-list` (5 M), `css-unique-styles` (2 M) | `bare-stress` | 4 | Re-measure. Selector memory (about 230 B per rule) does not change in this design, so these limits stay until selectors are redesigned. That is a later topic. |
| `css-resource-bytes` 16 MiB | `bare` network | 4 | Keep. It is a network cap, not an algorithm cap. |

### 4.3 What this design does not change

- Selector memory (about 230 B per rule, 289 B per `:is()` argument). A design for selectors (8-byte components in an arena, as Ladybird did) is the next topic. It is not part of this proposal because I have no measurement that says how it splits with the index change.
- The JS side of the CSSOM change (part 5). It depends on `docs/design-js.md`.
- Layout and paint.
- Parallel style traversal.

## 5. Risks

| Risk | Phase | Mitigation |
|---|---|---|
| A wrong signature gives a wrong style. | 3 | List every matcher input. A debug build runs the matcher on every element and compares. Fuzz test. Start with a strict key and relax. |
| M2 misses a rule (an ancestor test is skipped by mistake). | 2 | Fuzz test against the old matcher. Keep the old path in a debug assertion until the benchmark passes. |
| A missed DOM mutation gives a missed sheet. | 1 | A debug assertion compares the registry with a full walk on every change in tests. |
| Lazy parse changes error recovery. | 4 | Run the existing parser tests and a differential test: eager parse against lazy parse of every real sheet in `/data/bare-tmp/design/style/css/` and the journeys' sheets. |
| M3 costs more than it saves on pages with unique class names (`nextjs-docs` 80% new). | 3 | Turn it off per document when the hit rate is low after the first 1,000 elements. This is a limit, but it protects a constant cost, not an algorithm. |
| The estimates are wrong. | all | Each phase is measured alone. The next phase is decided after the numbers. |
| The live sites change between my runs. | measurement | The ramps are fixed pages. The journey numbers have run-to-run noise. |

## 6. What I did not verify

- The sign of the gain of M3 on real pages. I measured the hit rate and the share of work in first-seen signatures. I did not build the cache. The cost of `own_key` (100 to 200 instructions) is a guess.
- The share of the 7.94 M candidates that sit in buckets of 16 or more entries (the part M2 removes). I did not count it per bucket size.
- The `other` bucket (38% of candidate tests, measured over walks and restyles together). I do not know which selectors fill it.
- The 0.14% of invalid hits of the signature estimate. I did not find the cause of each.
- The claim that Bloom rejects cost 24 instructions each on real pages. It is derived on the two ramps (11.7 G / 484 M).
- The share of rule bodies that are used (12.8%) is from 12 journeys, counted per walk, not per text rule.
- Lazy parse totals (12 M for `bootstrap.min.css`): the selector part (8 M) is an assumption.
- The memory numbers of the new structures. I computed them from field sizes. I did not run `size_of`.
- Stylo's style sharing cache constants, the WebKit sharing resolver and selector JIT, the Blink lazy property parser, the Stylo `StylesheetSet`, the Meyerovich and Bodik numbers. I could not open or read these sources, or did not open them. The design does not depend on their details.
- Wall time. All numbers are instructions, except the old ramp times from the benchmark report.
- The instrumented build changes timing a little (counters). Instruction counts of the walks are not changed by the counters in a way that affects the ratios (hypothesis).
- Real logins, windowed mode, pages with shadow DOM at scale, `:has()`-heavy pages. The journeys do not use them in a measured way.
- I did not build any of the proposed code. All gains are estimates.

## 7. Open questions for the owner

1. **Order.** Phase 1 (sheet registry) is the cheapest and the safest. Do you accept it first, and phase 2 next? Or do you want phase 2 first because it removes the worst ramp cases?
2. **Signatures (phase 3).** This is the riskiest part, because a wrong signature gives a wrong style. Do you want it built only after phase 2 shows a remaining matching cost above a threshold? What threshold? My proposal: matching above 3% of journey instructions after phase 2.
3. **Lazy parse (phase 4).** CSSOM reads trigger body parsing. Is it acceptable that a page which reads all rules pays the parse cost at that time (it pays about the same as today)?
4. **CSSOM shape (phase 5).** Browsers put the style properties on a prototype. Do you accept that `Object.keys(rule.style)` changes (it does not match Chrome: Chrome lists the properties as own keys; bare matches its own `el.style`), as this lets us drop the 10.2 KB per object? This needs the JS design agent.
5. **Limits.** After phase 2, do you want the step charge to count real work and the `computed-work` limit to be lowered or removed, or kept as a safety net with a higher value?
6. **Selectors.** Do you want a separate design for the selector data structure (8-byte components in an arena, interned class atoms)? It would cut rule memory (230 B of 478 B) and speed up matching, but I have no measurement of the split yet.
7. **Invalidation (I2).** Do you want the dependency map (Stylo) after phase 3 even if the numbers say a cheap full pass is enough? My view: no. Measure first.
8. **Parallelism.** Is a parallel style pass in scope later? The signature design (phase 3) keeps the data in flat arrays, which suits it, but I did not design for it.
