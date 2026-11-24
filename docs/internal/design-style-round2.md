# Design: style round 2 (criterion 3) and the CSSOM object cap (criterion 8)

Task R2-E. Status: **proposal for the owner to review. No product code changed.**
Base: main `28a8fbc2`. Method: `CLAUDE.md` and the round 2 brief.
Related: [design-style.md](design-style.md) (round 1), [perf-goal.md](perf-goal.md), [progress.md](progress.md) (section "Style round 2: measurements and design").

## 0. Summary

**Criterion 3.** `compm_22000` costs 1,835 M instructions. It needs 1,166 M (-36.5%). `rest_13` costs 1,240 M. It needs 1,087 M (-12.3%). I measured where the instructions go. The first selector parse is **not** the lever. It is 3.0% of `compm_22000` and 7% of `rest_13`.

The cost is per element, in three passes over 44,002 elements (one full compute and two restyles):

| What | `compm_22000` | Share |
|---|---:|---:|
| A pass where every element hits the memo (no style change) | 236 M per pass, 5.4 K per element | 39% (3 passes, 713 M) |
| A memo miss (cascade, full-style hash, full-style compare, memo insert) | 9.2 K per miss, 77,769 misses | 39% (716 M) |
| Rule set build (parse, index) | 130 M | 7% |
| HTML parse | 131 M | 7% |
| JS, host glue, exit | 169 M | 9% |

**Recommended design** (three parts, build in this order, measure after each):

1. **A. Class atoms.** Keep the class tokens of each element as atoms. Key the class and id index by atom, in a flat array. This removes string hashing and string compare from the three per-element passes. Estimate: -264 to -403 M on `compm_22000` (middle -304 M) and -171 to -250 M on `rest_13`.
2. **D. Selector arena and a direct selector parser.** One arena for all selector parts and a parser that skips the general tokenizer for plain selectors. Estimate: -50 to -90 M on `compm_22000` and -50 to -90 M on `rest_13`.
3. **B. Cascade by winners.** A second memo, keyed by the winning declaration values, answers a miss without the full-style hash and compare. Estimate: -192 to -288 M on `compm_22000`.

Sum on `compm_22000`: -506 M (low), -614 M (middle), -781 M (high). The target is -669 M. **Only the high estimate reaches the target. The middle estimate is 55 M (8%) short (9.5x).** Part A decides it: A must save at least 2.7 K per element if D and B give their middle values. Build a spike of part A first, and measure the saving per element before the rest is built. If A falls short, add option C (about -30 M) and the owner decides on option H (section 3). Sum on `rest_13`: -221 to -340 M (middle -263 M) against -153 M needed. `rest_13` reaches the target in all three cases.

**Signature sharing (M3) is not the lever.** I counted it on both ramps: all 22,000 divs have a different list of matched rules (39,341 misses of 44,004 style computations, 89%). The gain of M3 is 0% on these ramps. The earlier text in `design-style.md` was right. M3 stays a journey optimization.

**Criterion 8.** The cap `cssom-objects` (524,288) guards a list of GC roots that never shrinks (`State::roots`). Each rule object that a script reads goes in and never leaves. The cap protects memory, not an algorithm. The design: remove the roots that a deleted rule or a removed sheet owns, and remove the cap. Memory is then bounded by the live rules and by the existing JS heap limits.

## 1. What I measured

All numbers are instructions in user mode (`perf stat -e instructions:u`) unless a section says otherwise. Machine: archMachine. Each case ran 3 times. **The spread was 0** for every driver and example total (instruction counts are exact on this machine). Pages: `/data/bare-tmp/bm/b/pages/` (the benchmark pages, read only). Builds: release, own target dir. A frame-pointer build was used for `perf record`. Callgrind ran only on a single-thread example (it matches `perf` within 0.3%). A callgrind run of the whole `bare-drive` is not valid: it was 21x inflated and I dropped it.

### 1.1 The two pages

- `compm_22000`: 22,000 rules (`.cN{color}`, `body.a .cN{margin}`, `.cN span{padding}`), 22,000 `div` with 6 classes each and one `span` each (44,002 elements, 2.0 MB). The script reads `getComputedStyle` of one element, sets `body.className` to `"a"`, reads again, sets `"b"`, reads again. That is one full compute and two restyles.
- `rest_13`: 36,000 rules, 3,000 `div` with 6 classes and one `span` each (6,002 elements, 1.0 MB). The script sets `body.className` 13 times and reads `getBoundingClientRect` of the last child. That is one full compute, 13 restyles and layout.

### 1.2 Verification of the PR #494 and #502 facts

| Fact | Result |
|---|---|
| Each ramp builds its rule set once | **Confirmed.** One `RuleSet` per page. The rule cache cannot help. |
| Build is 6.1% of `compm_22000` and 14.5% of `rest_13` | **Confirmed, a bit higher.** Staged example: 7.1% and 16.9% of the driver total. `perf` (frame pointers): 6.7% and 17.1%. |
| The selector parse is 51% of a build | **Partly.** `parse_scoped` is 54.6 M of the 130 M build on `compm_22000` (42%). The index push is 13.8 M (11%), the CSS tokenizer 20 M (15%). |
| The rest is cascade work on unique lists | **Confirmed.** See 1.4. |

### 1.3 Split by stage (staged example, M instructions)

The example `r2e_ramp` (kept as a patch, see 7) runs the stages in one thread: HTML parse, rule set build, first compute, then the restyles. The driver adds JS, layout, host glue and exit.

| Stage | `compm_22000` | share of 1,835 | `rest_13` | share of 1,240 |
|---|---:|---:|---:|---:|
| HTML parse | 131 | 7.1% | 25 | 2.0% |
| Rule set build (CSS tokenize, rule parse, selector parse, index) | 130 | 7.1% | 210 | 16.9% |
| First full compute | 581 | 31.7% | 103 | 8.3% |
| Restyles (2 and 13) | 825 | 45.0% | 456 | 36.8% |
| Rest of the driver (JS, layout, glue, exit) | 169 | 9.2% | 450 | 36.3% |
| Total (driver, 3 runs, spread 0) | 1,835 | | 1,240 | |

The 450 M in `rest_13` is mostly layout: `layout_areas` is 18.5% of the samples in the `perf` profile.

### 1.4 Split by call subtree (`perf`, frame-pointer build, inclusive)

`compm_22000` (16,897 samples): `restyle` 47.7%, `compute_kept` 33.8%, `add_in_layer` (build) 5.5%, `collect_split` 11.8%, `matches_from` 10.9%, `cascade_as` 6.4%, `parse_scoped` 2.5%, HTML parse 4 to 6.5%.

By category (a sample goes to the first category that has a frame in its stack; order: match, cascade, style):

| Category (`compm_22000`) | Share |
|---|---:|
| Style: hash, dedupe, memo (`style_of`, `hash_values`, `ComputedStyle ==`) | 31.6% |
| Match: collect candidates (`gather`, class and tag buckets, filter) | 17.3% |
| Restyle and walk loop (the rest of it) | 13.4% |
| Match: selector tests (`compound_matches`) | 10.9% |
| HTML parse | 6.5% |
| Cascade: declarations to values | 6.4% |
| Exit: drop of the page | 3.1% |
| JS | 2.7% |
| Rule index build | 2.6% |
| CSS tokenize | 2.4% |
| Selector parse | 1.7% |

(The category "layout" of the same script counts any frame that has `bare_layout::` in a generic argument. It gave 92% for `rest_13`. That is an artefact of the names. For `rest_13` I use the inclusive call tree: `cascade_with` 71%, `restyle` 42.4%, `add_in_layer` 13.9%, `compute_kept` 10.1%, `layout_areas` 18.5%, `parse_scoped` 6.7%, `collect_split` 10.7%.)

### 1.5 The cost of one pass and one miss (exact counts)

I added counters to `style_of` and `restyle_with` (patch `counters-cascade.patch`, not in the repo). Counts for `compm_22000`:

| Event | Count |
|---|---:|
| `style_of` calls in the full compute | 44,004 |
| memo misses in the full compute | 39,341 (89%) |
| of the misses: the new style equals an existing one (`by_hash` found) | 29,712 (76%) |
| restyle 1 (class `b`, no change): memo hits | 44,002 of 44,002 |
| restyle 2 (class `a`): misses | 38,428 |
| restyle 2: elements whose style changed | 19,665 |
| restyle 2: misses whose new style equals an existing one | 34,376 (89%) |
| restyle 3 (class `b` again): memo hits | 44,002 of 44,002 |
| candidates per element | 3.5 |

`rest_13`: 5,948 misses of 6,004 in the full compute (534 equal an existing style). The `a` pass has 4,000 misses (2,000 changed). The other 12 passes are all hits.

Cost per pass (the example, `n` restyles, 3 runs, spread 0):

| Pass | `compm_22000` | `rest_13` |
|---|---:|---:|
| Full compute | 581 M (13.2 K per element) | 103 M |
| Restyle, all memo hits | **236 M (5.4 K per element)** | 31.7 M (5.3 K per element) |
| Restyle with misses | 589 M | 75.3 M |
| Extra cost of a miss | **9.2 K per miss** ((589 - 236) M / 38,428) | |

Model: `cost = elements x 5.4 K + misses x 9.2 K`. For `compm_22000`: 132,006 x 5.4 K = 713 M, plus 77,769 x 9.2 K = 716 M, total 1,429 M. The model gives 1,429 M. The measured compute and restyles are 581 + 825 = 1,406 M (the model is 2% high).

### 1.6 Where the 5.4 K per element goes (a pass that only hits the memo)

Callgrind, exclusive cost, difference of the runs with 1 and 0 restyles (235.6 M):

| Function | M | Per element |
|---|---:|---:|
| `RuleSet::restyle` (loop, filter add and remove, hash fold, memo lookup, `rules == matched`) | 66.6 | 1.5 K |
| `compound_matches` (the class test splits the `class` string for each test) | 51.5 | 1.2 K |
| `Index::gather` closure (bucket look-ups by `Box<str>` key, `extend`) | 35.8 | 0.8 K |
| `Index::collect_split` (the `span` bucket of 7,334 rules, split by ancestor) | 28.1 | 0.6 K |
| `Document::attr` (scan of the attribute list for `class` and `id`) | 20.9 | 0.5 K |
| `matches_from` | 13.3 | 0.3 K |
| other | 19.4 | 0.4 K |

About 3 K of the 5.4 K per element is string work on class names: hash a `str`, compare a `str`, split a `str`, scan attributes.

### 1.7 The cost of a miss (9.2 K)

From callgrind of the full compute (568 M, 39,341 misses):

| Part | M | Per miss |
|---|---:|---:|
| `cascade_as` (pick winners, compute values) | 73.8 | 1.9 K |
| `ComputedStyle ==` against the style found by hash (`[Value]` compare of equal styles, always the full length) | 59.5 | 2.0 K (29,712 compares) |
| `hash_values` of the whole style (perf self 6.5% of the driver) | about 119 (of 1,835) | 1.5 K |
| memo insert (`Box<[u32]>` and a `Vec` push), `ensure`, `styles.push` of new styles, the rest | | about 3.8 K |

The full-style hash and compare is 3.5 K of the 9.2 K. It is paid even when the answer is "the same style" (82% of the misses on `compm_22000`).

## 2. Research

I read these sources in this task (sources of round 1 are in `design-style.md` section 2).

- **Stylo `selector_map.rs`:** the class and id maps are keyed by `Atom`. The element's classes are atoms, so the look-up needs no string hash.
- **Blink `rule_set.h`:** `id_rules_`, `class_rules_` and `tag_rules_` are maps keyed by `AtomicString`. A class is an interned string with a pointer compare.
- **Blink `UpdateStyleAndLayoutTreeForElement`:** it checks dirtiness, then runs the recalc for the whole document. So Blink has **no** lazy style for one rendered element.
- **Gecko `Servo_ResolveStyleLazily`:** a precedent for a lazy resolve of one element. I know its name from the sources. I did not read its implementation (**to verify**).
- **Round 1 (not re-read here):** the Stylo rule tree and style sharing cache, the Blink matched properties cache, the Ladybird arena of 8-byte selector parts.

What the engines do that bare does not: they key classes and ids by atom. This is the largest gap in the measured cost. Stylo and Blink also do **not** deduplicate computed styles by value after a miss. bare does, and pays 3.5 K per miss for it (1.7).

## 3. Criterion 3: the options

Target: `compm_22000` -669 M, `rest_13` -153 M. All gains are **estimates**. Each needs a spike. Arithmetic uses the unit costs of section 1.

### A. Class atoms (recommended)

- **Now.** `Simple::Class(Box<str>)`. `by_class: HashMap<Box<str>, Vec<u32>>`. `has_class` splits the attribute value for every test. The filter hashes each class string for every element (`name_hash(2, class)`).
- **Data.** Intern each class token in the document name table (it exists: `Document::intern`, atoms per thread). `Simple::Class(Atom)`. Per element, a slice of class atoms in one arena `Vec<Atom>` (8 bytes per element: `start` and `len` as `u32`). `by_class` and `by_id` become `Vec<ListId>` indexed by `atom.index()` (atoms are dense): a look-up is one bounds check and one load. The filter hash of an atom is a function of its number.
- **Keeping the slice correct.** There are 6 writers of attributes in `dom.rs` (`attrs.push` at 5 places and `set_attr_atom`). I do not edit all of them. The slice is a cache that is checked by the attribute value: `class_atoms[node] = (value span, range)`. It is valid when the `class` attribute still has the same span. `set_attr_atom` writes a new span. **To verify:** that a span of the text arena is never reused for a different value (compaction, reuse). If it can be, add a counter of class writes per document.
- **Algorithm.** The first pass over a node tokenizes and interns the class value once (about 60 instructions per token, 132 K tokens on `compm_22000`, about 8 M once). Every later pass reads the slice.
- **Complexity.** O(classes of the element) per pass, with no hashing. Memory: 8 bytes per element plus 4 bytes per class token (about 0.5 MB on `compm_22000`) plus the name table (22,000 names, about 1 MB).
- **Gain.** Per element in a pass: `compound_matches` 1.2 K to about 0.3 K, `gather` 0.8 K to about 0.3 K, `Document::attr` 0.5 K to about 0.1 K, filter hash and `restyle` loop 1.5 K to about 1.0 K. The parts add to 2.3 K saved per element (5.4 K to 3.1 K). I take 2.3 K as the middle, 2.0 K as the low case and 3.05 K as the high case (the parts are better than I think). Node-passes on `compm_22000`: 132,006. **-264 M (low), -304 M (middle), -403 M (high).** Share of 1,835: -14%, -17%, -22%.
  On `rest_13`: 13 passes of 6,002 elements = 78 K node-passes, plus the compute. -171 M, -193 M, -250 M (-14% to -20%) against -153 M needed.
- **Risk.** Every class writer must keep the cache valid (the span check covers it). Case-insensitive class match in quirks mode: the interned atom must fold the case like `has_class` does today. **To verify** how quirks mode is handled now.
- **Verdict: yes, first.** Both engines key by atom. It removes a whole kind of work (string work), not one row.

### B. Cascade by winners (recommended, third)

- **Now.** The memo key is `(parent style, matched rule list, hint)`. A miss runs `cascade_as`, then hashes all values of the new style, looks in `by_hash`, and compares the whole style with each candidate (`ComputedStyle ==`). 82% of the misses on `compm_22000` find an equal style.
- **Idea.** The style is a function of the parent style and the **winning declarations** only. Many different lists have the same winners (`.c3{color:#009}` and `.c9{color:#009}`). Add a second memo after the winner pick and before the value computation.
- **Data.** `Memo2: HashMap<u64, Vec<Entry2>>`. `Entry2 { parent: u32, set: u128, winners: Box<[Value]>, style: u32 }`. The key is a hash of `(parent, set, winner values)`. `set` is the bit set of properties that have a winner (it exists in `cascade_as`). Only the set properties are hashed (2 to 3 on this page), not the 53 or so values of a full style.
- **Algorithm.** In `cascade_as` pass 0: pick the winners (as now). Look up `Memo2`. If found, return the style id. Else compute the values, do the full-style dedupe (as now) and insert in `Memo2`.
- **Gate.** Use `Memo2` only when the winners have no `var()`, no pending transition input, no `picture` pass and no `rtl` pass, and the element has no inline style and no hint. Else take the present path. (These are the inputs of the value computation that are not the parent style and not the winner values.)
- **Complexity.** O(set properties) per miss, instead of O(all properties).
- **Gain.** Saved per found-miss: hash 1.5 K + compare 2.0 K + value computation about 0.8 K, minus the `Memo2` key and probe about 0.5 K. That is 3.8 K (low 3.0 K, high 4.5 K). Found-misses: 29,712 + 34,376 = 64,088. **-192 M (low) to -288 M (high).** -10% to -16% of 1,835.
- **Memory.** One `Entry2` per distinct winner set: about 9.6 K + 4 K on `compm_22000`, about 100 bytes each: 1.4 MB.
- **Risk.** A missing input of the value computation gives a wrong style. The gate above lists the inputs I found in `cascade_as` and `style_of`. A test must compare the style from `Memo2` with the style of the present path for random rule sets (differential test).
- **Verdict: yes, after A and D.** Blink and Stylo get a similar effect for the cascade through the matched properties cache and the rule tree. They do not deduplicate styles by value. B keeps the dedupe, which bare needs for the memo of the children.

### C. Parent key by inherited values (Stylo's child cascade requirement)

- **Now.** The key of a child contains the parent style number. A parent whose **non-inherited** values changed (`margin`) gets a new number. All its children miss. On `compm_22000` restyle 2: 38,428 misses, 19,665 changed styles, so 18,763 misses are children that end with the same style.
- **Idea.** Key children on the number of the parent's **inherited** value set (interned), not on the parent style number.
- **Data.** `inherited_id: Vec<u32>` per style (4 bytes). An intern table from the inherited subset to a number.
- **Gain.** Alone: 18,763 x 9.2 K = 172 M (-9%). **After B:** the same children become cheap misses (about 3 K): 56 M (-3%). Use C only if A, D and B do not reach the target.
- **Risk.** Which properties are inherited, and the `inherit` keyword and `em` units on a child. Medium. The key must also include the parent's font size, which is inherited.
- **Verdict: later, measured after B.**

### D. Selector arena and a direct parser (recommended, second)

- **Now.** `parse_scoped` builds a general tokenizer for each selector and returns `Vec<Compound>`, each with a `Vec<Simple>`, each `Simple::Class(Box<str>)`. That is 3 or more allocations per rule. At exit, `drop_in_place<RuleSet>` and `drop<Vec<Simple>>` cost 2.6% + 1.9% of the example. `malloc` is 4.0%.
- **Data.** One arena `Vec<Simple>` for the whole rule set. A selector is a range of `u32`. With A, `Simple::Class(Atom)` has no allocation. A direct parser handles the plain grammar (`ident`, `.class`, `#id`, `[attr]`, descendant, child, `,`) from the bytes. Anything else goes to the existing general parser.
- **Complexity.** O(bytes) with no token objects.
- **Gain.** `parse_scoped` is 54.6 M and the allocation and drop about 50 M on `compm_22000`. A direct parser should cost about a third. **-50 M (low) to -90 M (high)** on `compm_22000` (-3% to -5%). On `rest_13`: 87 M to 107 M of parse plus the allocations: **-50 M to -90 M** (-4% to -7%).
- **Risk.** The direct parser must give the same result as the general one. A differential test over all selectors in the corpus and a fuzz test.
- **Verdict: yes.** Stylo uses a compact selector list in one allocation. Ladybird uses an arena of 8-byte parts.

### E. Precise invalidation (Stylo `InvalidationMap`, Blink invalidation sets)

A class change on `body` makes a restyle of the whole subtree. A dependency map would restyle only elements that a rule `body.a .cN` can reach. On these pages each div has 6 classes and one third of the class numbers are in `body.a` rules. The chance that a div has no such class is (2/3)^6 = 8.8%. So a precise invalidation skips only **9%** of the elements (**estimate**, from the page generator). **Not the lever here.** It stays a journey optimization (round 1, option I2).

### F. Signature sharing (M3)

Measured: 89% of the full compute is a miss, all lists are different. M3 shares the matched list between elements that look the same to CSS. No two elements here look the same: the gain is **0%** on both ramps. M3 changes nothing for these two criteria. It stays as a journey candidate (round 1: 54% fewer match instructions where 71% of the elements share a signature).

### G. No dedupe of styles (the Stylo and Blink way)

Drop the full-style hash and compare. Gain: 3.5 K per miss x 77,769 = 272 M (-15%). Cost: memory. Today `compm_22000` keeps 13,681 unique styles (about 11.7 MB at 856 bytes). Without the dedupe it keeps one style per miss, 77,769 (about 67 MB), and children no longer share the parent number, so they miss more. This breaks the memo of the children. **Rejected:** B gets 70% of this gain without the memory cost.

### H. A lazy style for one element (Gecko `Servo_ResolveStyleLazily`)

`getComputedStyle(el)` of one element needs the styles of its ancestors and itself only. `compm_22000` reads 3 elements and never lays out. A lazy resolve would remove the full compute (581 M) and both restyles (825 M): **-77% on `compm_22000`**. It does not change `rest_13` (layout needs all styles) and it does not change any journey that lays out a page. Blink does not do this. **I do not recommend it for criterion 3:** it fits this ramp (a script that reads a few styles and never lays out) and would not move the journeys. It is a valid design for pages that read a few styles during load. The owner decides if that case matters.

### Comparison

| Option | `compm_22000` | `rest_13` | Journeys | Risk | Verdict |
|---|---:|---:|---|---|---|
| A class atoms | -264 to -403 M | -171 to -250 M | all: every pass | medium | **yes, first** |
| D selector arena + direct parser | -50 to -90 M | -50 to -90 M | rule-heavy pages | low | **yes, second** |
| B cascade by winners | -192 to -288 M | small | pages with unique lists | medium | **yes, third** |
| C inherited-values key | 0 to -56 M after B | 0 | restyles | medium | later |
| E precise invalidation | -9% of restyle node work | -9% | restyles | high | later |
| F signature sharing (M3) | 0 | 0 | -1 to -3 points | high | journeys only |
| G no dedupe | -272 M | small | memory +56 MB | low | rejected |
| H lazy one-element style | -1,406 M (-77%) | 0 | none | high | owner decides |

### Expected result on the criterion (A + D + B)

| | low | middle | high | needed |
|---|---:|---:|---:|---:|
| `compm_22000` saving, A | 264 | 304 | 403 | |
| D | 50 | 70 | 90 | |
| B | 192 | 240 | 288 | |
| Total saving (M) | **506** | **614** | **781** | **669** |
| Result (M) | 1,329 | 1,221 | 1,054 | 1,166 |
| Ratio to baseline 11,664 M | 8.8x | 9.6x | 11.1x | 10.0x |
| `rest_13` saving, A + D | 221 | 263 | 340 | **153** |
| Result (M) | 1,019 | 977 | 900 | 1,087 |
| Ratio to baseline 10,873 M | 10.7x | 11.1x | 12.1x | 10.0x |

`compm_22000` reaches 10x only in the high case. With C added (about 30 M in the middle case) it is still 25 M short. If the spike of A shows a saving below 2.7 K per element, decide on H or accept a result of about 9.5x. **All three parts overlap a little** (D removes allocation that B's memo inserts also pay). I did not subtract the overlap: it is under 5% (**hypothesis**).

### Limits in this area

The step charge of the restyle (`STEPS_PER_RESTYLED_ELEMENT` = 64 plus candidates) and the `computed-work` limit were designed for the old matcher. After A the real cost per element is lower, so the charge is too high. Re-measure the charge against the instruction counts after A (round 1, option I5). I do not change a limit in this document.

## 4. Criterion 8: the `cssom-objects` cap

### 4.1 What the cap protects

`crates/bare/src/browser_cssom.rs`: `MAX_OBJECTS = 1 << 19` (524,288). `keep(d, o)` pushes each CSSOM object that the host creates (rule, sheet, declaration, list, media list) into `State::roots: Vec<ObjId>`. `roots()` gives the whole vector to the GC at each collection. **Nothing removes an entry.** `deleteRule`, `replaceSync`, removing a `<style>` element and dropping all script references leave the root in place. The host tables (`rules`, `lists`, `style_rules`, `medias`, all `IdMap` keyed by `ObjId`) hold an entry per object too, and a table entry for a collected object would point at a stale handle (`HeapError::Stale`). So the object is rooted to keep the host table valid.

So the cap protects **memory growth** of a root list that does not shrink. It is not a guard for a slow algorithm. It is still a design problem: it has a fixed number that does not depend on the page, and it fails a page that loops with `insertRule` and `deleteRule`.

The JS heap has its own limits: `HeapLimits.max_bytes = 1 << 30` and `max_objects = 1 << 24`. The GC runs only between tasks (`gc_due`, `eval.rs`), and `collect` is refused during a call. A loop inside one task grows the heap like any other JS loop, up to those limits.

### 4.2 The cost per object today (after PR #491)

Measured with generated pages (`gen_cssom.py`, probe pages in `/data/bare-tmp/r2/R2-E/pages/`), 3 runs, spread under 0.5%:

| Case | Instructions | Memory |
|---|---:|---:|
| Read a rule object (`sheet.cssRules[i].selectorText`) | 17.8 K | 750 to 960 B |
| Read a rule and its `.style` | 31.4 K | 1,258 B |
| One `insertRule` + `deleteRule` (churn) of a one-rule sheet | 34.8 K | **839 B leaked** |

The 17.8 K is mostly `rule_from_chunk` (a new parse of the chunk) and the host table inserts. A one-rule sheet that is edited in a loop hits the cap after **about 506 K iterations at 494 MiB**. The deleted rules are garbage. The cap fails a page that is correct. At the cap the worst case is 524,288 x 1,258 B = 660 MiB, below the heap limit of 1 GiB.

### 4.3 The options

**D1. Roots that follow the live rules (recommended).**

- `roots` becomes a set keyed by `ObjId` (a `HashSet`, or an `IdMap<ObjId, ()>`). `deleteRule`, `replaceSync`, `replace`, a `cssText` write on a sheet owner and the removal of a `<style>` or `<link>` owner **un-keep** the objects that leave the list: the rule, its `.style`, its nested list and its media list.
- The host table entries of an un-kept object stay while a script may still hold the object. A script that holds a deleted rule keeps the object alive through the JS heap. The entry is removed when the heap says the handle is stale: add `Heap::is_live(ObjId)` (one generation compare; `objects.get(index, gn)` exists) and prune the tables **at the start of `roots()`**, which the GC calls. Prune only when a table has grown to twice its size at the last prune: amortized O(1) per object.
- A rule that is in a live list stays rooted: the identity `sheet.cssRules[0] === sheet.cssRules[0]` and an expando on the rule object must hold (a browser keeps the wrapper while the rule exists).
- The cap: **remove** `MAX_OBJECTS` from `keep`. The set cannot grow past the live rules (plus one declaration object and one list per rule). `MAX_RULES` (the length of one list, `Entry` of 12 bytes) stays: it bounds a data size that comes from the sheet size, which the network limit (16 MiB) already bounds. The `adoptedStyleSheets` length check (`len > MAX_OBJECTS`) is an argument size check: charge it with `it.charge(len)` (it exists on the next line) and drop the constant.
- Complexity: O(1) per create, O(1) per delete, O(table) amortized O(1) per object at prune.
- Memory: a churn loop keeps 0 bytes after a GC (today 839 B per iteration). A page that reads all 187,852 rules of the biggest sheet keeps 585 B x 187,852 = 105 MiB while the sheet is live. That is the same as today.

**D2. Keep the cap and tie it to the rule count.** `cap = 4 x rules + 4096`. It is a three-line change. It fails the churn loop **sooner** than today (after about 4 K iterations for a one-rule sheet), because deleted objects stay rooted. It makes the real page worse. **Rejected**, unless D1 is also done (then the cap is never reached and is dead code).

**D3. Weak list entries.** Let a list hold its rule objects weakly and rebuild them from the chunk (17.8 K instructions) when the handle is stale. Memory would then follow what the script holds. **Rejected:** an expando or identity of a rule object would be lost between two reads. A browser keeps them. It changes what a script can see.

### 4.4 Expected result

| | now | D1 |
|---|---|---|
| Churn of one rule, objects rooted after 1 M iterations | cap error at 506 K (494 MiB) | 0 after a GC |
| Memory per rule that a script read | 585 to 1,258 B | the same |
| Cap that a page can hit | 524,288 objects | none (heap limits only) |
| Criterion 8 | not met | met: the cap is removed |

### 4.5 Risks and what to verify

- A host table entry for a collected object that is used later. A test: create, drop, force a GC, create again, read. It must not panic and must not read the old entry.
- The order of `un-keep` and the call back into script (a `cssText` setter that throws).
- A rule that is moved between lists (`@import`, `@media` nesting). **To verify** that `parent` and `list` links are updated in one place.
- GC runs only between tasks, so a single task can still create 16 M objects before the heap limit. This is the limit for every JS object. I do not change it.

## 5. Order of work

1. **Spike A** (class atoms, in a branch, not merged): measure the cost per element of the three passes on both ramps. The saving per element must be 2.7 K or more for `compm_22000` to reach the target with D and B at their middle values. If it is lower, add C and decide on H.
2. Build **A**, then **D**, then **B**. After each: the frozen benchmark on the whole browser (`bare-drive` journeys with the virtual clock, the ramps, the cost matrix), 3 runs, on newest main. Record the numbers in `docs/progress.md`.
3. Build **D1** of criterion 8 on its own. It touches `browser_cssom.rs` and one function in `crates/js/src/heap.rs`. A test for stale entries and a test for the churn loop are the check.
4. Re-measure the step charge and the `computed-work` limit after A.

## 6. What I did not measure or verify

- None of the gains is built. Every gain is an estimate from unit costs. The error is large for A (the saving per element) and B (the cost of the winner key).
- Whether a span of the text arena can be reused (A, the cache check).
- How quirks mode folds the case of a class today (A).
- The journeys: I did not run them. I do not know how A, B and D change them (A should help every pass; B helps pages with unique lists).
- The Gecko implementation of `Servo_ResolveStyleLazily`, the Stylo rule tree and the Blink matched properties cache in this task (only from round 1).
- Layout, paint and JS execution inside `rest_13` beyond the call-tree shares (layout is about 18.5%).
- A wall-clock time (the machine was not idle). Only instruction counts.
- Windows and macOS.

## 7. Files kept outside the repo

`/data/bare-tmp/r2/R2-E/`: `r2e_ramp.rs` (the staged example: parse, build, compute, restyles), `counters-cascade.patch` (the counters of 1.5), `prof.py`, `split.py`, `split2.py`, `glue.py`, `diffcg.py`, `gen_cssom.py`, `meas.py`, the callgrind and `perf` data, and the CSSOM probe pages.
