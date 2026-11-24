# Design: `compm_22000` 10 times lower (criterion 3)

Status: design for review. No product code is changed by this document.
Date: 2026-11-15. Main at `79829d19` (#541 merged).
Goal: `compm_22000` at or below 1,166.4 M instructions (10x lower than the baseline `f6415364`, 11,664 M).

## 0. Summary

- Today `compm_22000` costs 1,558.5 M (7.48x). `rest_13` costs 1,042.5 M (10.43x, goal met).
- The page reads the style of 3 elements. The browser computes the style of all 44,002 elements, twice (one full compute, one restyle after the class change).
- Style identity (hash of the 872-byte computed style, then a compare) is the largest single cost: 370.7 M, 23.8%. Selector matching is 20.6%. Cascade is 14.8%.
- Steps 2 and 3 of the R2-E design (selector arena, winners memo) make this work cheaper. They give about 1,180 M (9.3 to 10.5x). That is a coin flip for criterion 3.
- Recommended: option H, a lazy chain read. `getComputedStyle` styles the element and its ancestors only. The other 44,000 styles are never made. Expected: about 393 M (29.7x; range 370 to 420 M). `rest_13` does not change, because it reads layout.
- H removes a whole kind of work. Steps 2 and 3 are not needed for criterion 3. They may still be built later for pages that read layout.
- Any style design moves the 47 journeys by less than 1.4%: the forced style work is 4.7% of their instructions.

## 1. What I measured

Machine: archMachine. Build: release with symbols, `/data/bare-tmp/target-compm10x`. Phase counters: a temporary patch that reads the `instructions:u` counter (`perf_event_open`) between phases. The patch is not in the repository (`/data/bare-tmp/r2/compm10x/phase-counters.patch`). Each stage was run 3 times; the spread is 0.0% to 0.01%. The sum of the laps (walk and two restyles) is 1,140.5 M against 1,121.6 M from the staged runs (within 1.7%; each lap reads the counter, which costs 32 instructions).

Page: 22,000 `div` with one `span` each (44,002 elements), 6 classes per `div`, 21,999 author rules of three shapes (`.cN`, `body.a .cN`, `.cN span`). The script reads 3 styles and changes `body.className` twice.

### 1.1 Split by phase (instructions, M)

| Phase | M | Share |
| --- | --- | --- |
| HTML parse (stage 0) | 131.9 | 8.5% |
| Rule set build, CSS parse (stage 1 minus 0) | 112.7 | 7.2% |
| First full compute (walk), 44,002 elements | 508.6 | 32.6% |
| Restyle 1, class `a` (38,427 misses) | 476 | 30.5% |
| Restyle 2, class `b` (all memo hits) | 137 | 8.8% |
| Drop at exit (stage 4 minus 3) | 45 | 2.9% |
| Rest, by difference (process start, JS, bindings, glue) | about 147 | 9.4% |
| Layout, paint, network | 0 | the page has no layout read |

"Rest" is a difference, not a measurement. Hypothesis: it is start-up, the three JS reads, bindings and the first parse of the UA sheet.

### 1.2 Split by work kind (walk plus both restyles, 1,140.5 M of laps)

| Work | Walk | Restyles | Total M | Share of 1,558 M |
| --- | --- | --- | --- | --- |
| Style identity: `style_hash` plus `dedupe_find_cmp` | 178.4 | 192.3 | 370.7 | 23.8% |
| Selector matching: `candidates` plus `match_loop` plus sort and hint | 135.2 | 186.0 | 321.2 | 20.6% |
| Cascade: `cascade_as` plus `ensure_lazy` (rule bodies parsed on first use) | 116.0 | 113.9 | 229.9 | 14.8% |
| Ancestor filter: add and remove | 33.6 | 67.4 | 101.0 | 6.5% |
| Memo: lookup and insert | 33.0 | 41.2 | 74.2 | 4.8% |
| Styles vector and hash index push (`styles_push_byhash`) | 11.8 | 7.8 | 19.6 | 1.3% |
| Stack glue (`glue_pop`, `children_push`, `post_style_hidden`) | 7.9 | 16.0 | 23.9 | 1.5% |

Top 5 of the whole run: style identity 23.8%, selector matching 20.6%, cascade 14.8%, HTML parse 8.5%, rule build 7.2%.
JS, layout and paint are 0% or inside "rest". Allocation is inside each row; I did not split it (see section 9).

### 1.3 Per element and per miss

There are 44,002 elements, so the whole run is 35.4 k instructions per element. The walk is 11.6 k per element. Restyle 1 is 10.9 k. Restyle 2 (all hits) is 3.3 k.
Per miss lap (walk): `style_hash` 2,462, `dedupe_find_cmp` 2,075, `cascade_as` 1,916, `candidates` 1,699, `match_loop` 1,193, `ensure_lazy` 1,032, `memo_insert` 676, `filter_add` 386, `leave_filter_remove` 378.
A miss costs about 6.7 k for identity plus cascade; a hit costs 0.2 k.

### 1.4 What the memo does today

The walk has 39,335 misses and 4,669 hits. The 39,335 misses make 13,681 unique styles. Restyle 1 has 38,427 misses.
The key of the memo is the parent and the list of matched rules. 22,000 divs have 6 classes each, so almost no two divs match the same rule list. The key is rarely found.
The winners are another matter. Two elements that match different rules often win the same declarations. Distinct winner keys (with the parent style) are 9,632 in the walk and 9,308 in the restyle. So 75.5% and 75.8% of the misses are "found keys": the cascade and the style identity would repeat work that was done before.

### 1.5 The 47 journeys

Counter build, 47 journeys counted, 1 failed (nextjs-docs), corpus `/data/bare-tmp/corpus/virtual-load`. Total 118,240 M. The forced style work is 5,525 M (4.7%). The forced layout work is 3,856 M (3.3%). These are shares, not wall time.
Style work moves the journeys by at most 4.7%. A design that halves it gives 2.3% on the journeys. The `getComputedStyle` rows (compute 262.7 M and restyle 152.3 M) are at most 0.35% of the total.

## 2. What the other engines do not do

| Engine | Mechanism | Work that does not exist there |
| --- | --- | --- |
| Stylo (Firefox) sharing cache | A cache of up to 32 candidates per depth. A candidate shares when tag, classes, state and the parent style are equal. A hit skips selector matching. | Matching and cascade for a shared element. Hypothesis (to verify): it would not hit on this page, because the 22,000 divs have 22,000 different class lists. |
| Stylo rule tree | A matched rule list is a node in a tree. The computed style hangs on the node and on the parent style. | The hash and the compare of the style value. Identity is a pointer. |
| Blink `MatchedPropertiesCache` | The key is the matched declaration list. A hit gives the computed style without the cascade. | The cascade of repeated declaration lists. Matching still runs. To verify: the exact hit rules. |
| WebKit style resolver sharing | A sibling or cousin with the same attributes shares the style. | Matching and cascade for shared elements. Same limit as Stylo: needs equal classes. |
| Gecko lazy resolve (`Servo_ResolveStyleLazily`) | Gives the style of one element. It reuses the stored style, then the cache of undisplayed elements, then calls `resolve_style`. | A full traversal for one read. To verify: how far `resolve_style` walks up. I read the function header and not its body. |

For `compm_22000`:

- The work that a rule tree removes is style identity: 370.7 M, 23.8%. Today it is a hash and a compare over 872 bytes. A rule tree has no such work.
- The work that a winners cache removes is the cascade of found keys: about 125 M (75.6% of 165 M).
- Sharing by equal classes would not hit. The page was built so that no two divs have the same class list.
- None of these designs removes the walk of 44,002 elements. They make each element cheaper. The only design that removes the walk is the lazy read (H).

## 3. Current data structures and complexity

All in `crates/css/src/cascade.rs` unless noted. N is the number of elements, R the number of rules, d the tree depth, k the classes of an element.

- `RuleSet`: rules in `entries`, bucketed by the rightmost compound (id, class, tag, universal) plus ancestor-keyed split lists (`Index`, `Big`) for big buckets. `candidates_with` gathers the bucket lists of an element. Cost per element: O(k + candidates). Here the candidates are 768 to 1,699 instructions per element.
- Rule bodies are parsed lazily (`ensure`, `read_lazy`). One parse per rule that matches at least once.
- `AncestorFilter`: counting filter of the ancestor hashes. `add_element` and `remove_element` per element: 386 and 378 instructions. O(1) per element, but paid for every element, also for a `span` that has no children.
- `walk` (`compute_kept`): a stack walk in document order. For each element: gather candidates, match, sort the matched rule indexes, look the list up in `Memo`.
  - `Memo { cache, by_hash }`: `cache` maps (parent style, matched list) to a `CacheEntry { parent, rules: Box<[u32]>, hint, style }`. On a miss, `cascade_as` builds a `ComputedStyle` (872 bytes, 108 core properties, 8 bytes per `Value`).
  - Dedupe: `style_hash` (hash of all values) and then `==` against the styles with the same hash. Cost O(size of style) per miss: about 4.5 k.
  - Total: O(N (k + candidates)) plus O(misses x style size).
- `restyle_with`: the same walk, over the subtree of the marked elements. A class change on `body` marks the whole document.
- `browser_computed.rs`: `getComputedStyle` calls `cascade_inner`. It asks for a full compute or a restyle, and caches the result by (revision, text revision, length, user revision, `want`) in `built.last[slot]`. The limits `computed-nodes` and `computed-work` guard against huge documents.
- `dom.rs`: `ClassTable` interns class names. `has_class_id` reads a per-element list of ids, or scans the attribute (section 4).

Complexity of one `getComputedStyle` read after a write: O(N) for any N, even to read one element. This is the cost that H removes.

## 4. The `has_class_id` rule from #541

Rule: `has_class_id` scans the attribute until the scans outnumber the nodes. After that it makes a class list per element and reads it.

What it is: an amortization policy, the "ski rental" kind. It pays a small price for each question and buys the list when the total price would pass the price of the lists. The scan total is capped at N scans, so the worst case is about 2x the best choice. It is not a guard against an unbounded slow algorithm; the scan total is bounded by the rule itself.

What I found, by reading the code (`dom.rs:1607`):

- The threshold depends on the size of the document. A small document makes lists early. A large document scans for a long time.
- The count is global to the document, not per element. One element that is asked 1,000 times scans 1,000 times if the document has more than 1,000 nodes.

Options:

1. Per-element rule. Scan on the first read of an element. Make the list on the second read of the same element. This needs one marker per element (the `slots` vector already has a free value). It has the same 2x bound, no global state and no dependence on N. Expected gain: small. Hypothesis: below 1% of `compm_22000`; I did not measure it.
2. Make the list when the attribute is set, like Gecko and Blink (to verify). The cost is paid by every element of every page, also by pages that never ask by class. Per element: tokenize and intern 6 classes, about 1 k instructions by my estimate, so about 44 M (2.8%) on this page. This is worse than the rule for script pages. #541 chose the rule because of this.
3. Remove the questions. The questions come from selector matching of class rules. Option H and the winners memo ask fewer questions. This is the only option that removes the rule by removing the work.

Answer: a better data structure exists (option 1). It removes the dependence on N, but it does not give a measurable gain. I do not recommend a build for criterion 3. The rule stays until a profile shows `has_class_id` in the top costs. Nothing in the profile of `compm_22000` shows it: class tests are inside `match_loop` (20.6% together with candidates), and I did not split them out.

## 5. Options

Numbers for options other than H are estimates from the lap counters, not builds. Method: take the lap cost of the work removed, scale by the share of found keys (75.6%), add the cost of the new work.

### Option H: lazy chain read (recommended)

Idea: `getComputedStyle(el)` for a property that needs no layout styles `el` and its ancestors, and stops. The other elements are not styled.

Data structures:

- `chain: HashMap<NodeId, Rc<ComputedStyle>>` (or a vector indexed by node, with a revision stamp). It holds the styles that were made lazily. It is valid for one document revision, like `built.last`.
- No new rule structure. The primitive exists: `hidden_with` / `style_hidden` already styles one element with its ancestor chain for a hidden element. H makes it the general path.

Algorithm for a read of node `n`:

1. Walk up to the first ancestor that is in `chain` (or the root).
2. Walk down. For each element: add it to the ancestor filter, gather candidates, match, look up `Memo`, cascade on a miss, store in `chain`.
3. Return the style of `n`.
4. A write to the DOM drops `chain` (revision changed). A layout request, or a read that needs layout, takes the full path. A full compute fills the same store, so a later read hits.

Complexity: O(d x per-element cost) for the first read; O(1) for a sibling of a cached element plus the filter rebuild O(d x 388). A loop that reads all N elements costs N x (11.6 k + d x 0.4 k) = about 1.3x the full walk, so it does not need a limit or a switch to the full path. The worst case is bounded by the design. (If measurement shows a worse factor, a cache of the last filter makes document-order reads O(1) per element.)

Memory layout: one 872-byte style per styled element, shared through `Rc` with `Memo`. For 3 reads on this page: about 3 x d = 12 styles.

Correctness risks to check (these are the reason for slice H1's tests):

- Selectors that depend on other elements: sibling combinators and `:nth-child` use `sibling_place` (already one pass per document). `:has()` reads descendants; the match is evaluated on the element with the whole document, so it works. To verify: any rule feature that needs the parent's matched rule list (for example `@container`).
- Inherited values and custom properties come from the parent in the chain. They match the full compute because the parent is styled by the same code.
- The test: for random elements of many pages and after class writes, the chain style must equal the full-compute style.

Expected numbers (hypothesis, not built):

| | `compm_22000` | `rest_13` |
| --- | --- | --- |
| Floor, measured: the same page with the same writes and no reads | 252.9 M | not applicable (it reads layout) |
| Plus 3 chain reads: about 12 element styles at 11.6 k = 0.14 M, plus a reserve for set-up work I did not measure | 370 to 420 M, point 393 M | 1,042 M (no change) |
| Factor against baseline | 27.8x to 31.5x (point 29.7x) | 10.4x |

The reserve is about 140 M. It is not measured. Hypothesis: it is the first-read set-up (index lists, class table) and the exit drop. This is the main uncertainty. Even at the high end of the range (420 M), the factor is above 27x. The criterion needs 10x.

Effect on the 47 journeys: at most 0.35% of the total (the compute and restyle of the `getComputedStyle` rows, 415 M of 118,240 M), and only where the caller does not read layout later. The journeys that read `offsetWidth`, `getBoundingClientRect` and similar still need every style. Effect on the cost matrix: the rows that only read `getComputedStyle` (the `compm` family) improve. I did not run the 813 rows (section 9).

Honest limit: this meets criterion 3 for a page that reads a few styles. It does not make the full style pass faster. `rest_13` and every page with a layout read still pay the full pass.

### Option B+D: winners memo plus selector arena (steps 3 and 2 of R2-E)

Data structures:

- Winners memo: key = (parent style id, list of winning declaration values). Value = style id. On a found key the cascade, the hash and the compare are skipped.
- Selector arena plus direct parser (D): rules in one arena with no per-rule allocation, parsed straight into the arena. It cuts rule build time and the pointer chasing in `match_loop`.

Algorithm: match as today, pick the winners (about 1.3 k per miss, measured as `cascade_pick_winners_customs` on `rest_13`), look the key up. A found key costs about 1.6 k instead of 6.7 k.

Estimate for `compm_22000`:

- B: found misses 0.756 x 77,762 = 58,800. Saving 5.1 k each = about 300 M. (The R2-E document said 190 to 290 M.)
- D: rule build and match: about 50 to 90 M.
- Total: 1,558 - 350 to 390 = about 1,170 to 1,210 M, range 1,110 to 1,260 M. Factor 9.3x to 10.5x. This is a coin flip against the 10x goal.
- With a leaf skip (an element without children does not add itself to the ancestor filter; 22,001 spans): about -50 M more, so about 1,120 to 1,160 M (10.0x to 10.4x).

`rest_13`: about 900 to 960 M (11.3x to 12.1x). Rule build is 183 M there, so D helps most.
Journeys: style forced work 4.7%. If the style cost falls by 30%, the journeys fall by about 1.4%.

### Option C: bytes style identity (rule-tree-like identity without a tree)

Data structures: a compact byte image of the computed style (the values that differ from the parent's, or the whole 872 bytes in a flat form), and a hash and `memcmp` over the image. Style identity per miss: about 0.8 k instead of 4.5 k.
Estimate: identity -296 M. Plus D (-70 M) plus a leaf skip (-50 M): about 1,140 M (10.2x). `rest_13`: about 960 M.
Risk: the `ComputedStyle` layout must have no padding that varies; all `Value` fields must be comparable as bytes. This is a wide change in the type, so it conflicts with other work in `ComputedStyle`.

### Option I: restyle of dependent rules only

For a class change on `body`, only the rules that name the class in an ancestor position (`body.a .cN`) can change. An invalidation map (class -> selector -> scope) lets restyle test only those rules on the descendants. Stylo uses this idea (invalidation maps; to verify).
On this page 7,333 rules depend on `a`, and each `div` has about 2 of them, so every `div` is still restyled. Estimate: only the matching cost of restyle 1 falls: about 100 M less (186 M of restyle matching, a third of it saved), so about 1,450 M (8.0x). Not enough alone, and it needs the invalidation map (new data structure, high effort). Not recommended for this goal. It is useful for pages where a class change affects a small part.

### Option E: no style dedupe (G in R2-E)

Remove the hash and the compare and let each miss keep its own style. Saves the 370.7 M, but memory grows: 39,335 styles x 872 bytes = 34 MB against 13,681 x 872 = 12 MB today. Peak memory is the owner's first priority. I do not recommend it.

### Comparison

| Option | `compm_22000` M | Factor | `rest_13` M | Built effort | New risk |
| --- | --- | --- | --- | --- | --- |
| Today | 1,558 | 7.5x | 1,042 | | |
| H | 370 to 420 (393) | 27.8x to 31.5x | 1,042 | small: the primitive exists | wrong value on a rare selector; tests cover |
| B + D | 1,170 to 1,210 (1,110 to 1,260) | 9.3x to 10.5x | 900 to 960 | large: R2-E steps 2 and 3 | merge conflicts in `cascade.rs` |
| B + D + leaf skip | 1,120 to 1,160 | 10.0x to 10.4x | about 900 | large | same |
| C + D + leaf skip | about 1,140 | 10.2x | about 960 | large, wide type change | `ComputedStyle` conflicts |
| I | about 1,450 | 8.0x | about 1,000 | large, new data structure | invalidation bugs |
| E (no dedupe) | about 1,190 | 9.8x | about 1,020 | small | +22 MB peak on this page |

## 6. Are steps 2 and 3 still worth building?

For criterion 3: no. B and D together are a coin flip. H alone is far over the goal. If H is built, B and D add nothing to `compm_22000`, because the page then styles about 12 elements.

On their own merits: yes, probably, but later and measured on their own. They lower the cost of every page that reads layout, which H does not help. The upper bound is about 30% of the forced style work, so about 1.4% of the journeys. This is a small gain for the journeys. The R2-E document asks for a design that removes work. B removes the cascade of found keys; D removes the allocation of rules. Neither removes a kind of work from the journeys. I recommend that the owner decide after H, with a fresh profile of a layout-reading page (for example `rest_13`, where rule build is 183 M of 1,042 M).

## 7. Recommendation and build slices

Build H. Build slices, each one a PR:

1. H1: `RuleSet::style_chain(doc, node, parent_cache)`. It reuses `hidden_with`. Tests: for all elements of the ramp pages, the fixtures and the journey pages, the chain style equals the full style; also after a class write, an attribute write and a stylesheet write; also for inherited and custom properties and for `:has()`, sibling and `:nth-child` selectors. Check: the equal-style test passes.
2. H2: wire it into `browser_computed.rs` `cascade_inner`. Use it when `want` needs no layout. Drop `chain` on a revision change. Fall back to the full path for layout reads, for features that H1 lists as unsupported, and for pseudo-elements if H1 does not cover them. Check: `compm_22000` and `rest_13` instruction counts, 3 runs each, on the frozen commit; the 47 journeys still pass (`tools/bench/journeys.sh run --clock virtual`).
3. H3: record the numbers in `docs/progress.md`. If `compm_22000` is above 420 M, find the reserve (section 5, H) with the phase counters.

No limit and no threshold: the worst case of a read-all loop is about 1.3x the full walk by design. If H2 measures a worse factor, add the cache of the last ancestor filter instead of a limit.

Expected gain per criterion: criterion 3 `compm_22000` from 7.5x to about 29.7x (range 27.8x to 31.5x); `rest_13` unchanged at 10.4x. Other criteria: no change expected.

## 8. Overlap with other work

- Lazy freeing of DOM nodes (DOM arena, GC): the exit drop is 45 M on `compm_22000` (2.9%) and 30 M on `rest_13` (2.9%). H does not touch it. If the arena agent lowers it, the H floor of 252.9 M falls too. Both agents edit different files: H is in `cascade.rs` and `browser_computed.rs`; the arena is in `dom.rs`. Option 1 of section 4 would touch `dom.rs` (the `slots` marker) and conflicts with the arena work, so build it only after the arena merges.
- SharedWorker stub and corpus refill: no overlap. The journey corpus is `/data/bare-tmp/corpus/virtual-load`; `craigslist-home` (47 G) dominates the total. It changes the share numbers: style is 4.7% of the total with it.
- GC inside a long call: no overlap. H adds the `chain` store, which holds styles for one revision only; it frees on the revision change.

## 9. What I did not measure

- Option H is not built. Its numbers are the measured floor (252.9 M) plus an estimate. The 140 M reserve is a hypothesis.
- B, C, D, I and E numbers are estimates from the laps, not builds.
- Allocation is not a separate row. It is inside the other rows.
- Wall time on an idle machine. All numbers are instruction counts (`instructions:u`), one machine.
- Peak memory of each option.
- The 813 cost-matrix rows. Only the `getComputedStyle` rows of the 47 journeys are counted.
- `nextjs-docs` fails in the journey run; it is left out of the 47.
- The Gecko and Blink claims in section 2 are from source reading and are marked "to verify" where I did not read the whole function.
- `has_class_id` options 1 and 2: not measured.

## 10. Files outside the repository

`/data/bare-tmp/r2/compm10x/`: `README.txt`, `phase-counters.patch`, `stages.sh`, `stages.out`, `cnt.*.txt`, `cg.compm.out` (callgrind), `force.txt`, `jr.tsv`, `pages/` (read-free and one-read variants of the page). Symbol build: `/data/bare-tmp/target-compm10x`. Counter build: `/data/bare-tmp/target-compm10x-cnt`.
