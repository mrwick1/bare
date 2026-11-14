# Design: share the rule set between documents

Status: proposal. The owner reviews this document before any code is built.
Written on 2026-11-14 against main `5b42d061` (PR #494 merged). All numbers come from archMachine (16 cores, shared). Instruction counts are `instructions:u`.
Nothing here is built. A number marked "estimate" is computed from measured numbers. A claim marked "hypothesis" or "to verify" is not checked.

## 0. Summary

The question: does the browser build the rule set (`RuleSet`: entries, selector index, split lists) again for each document, and how do we stop that?

Findings, in short:

1. **The hypothesis is right, and it is larger than said.** Nothing at the CSS level is cached. Each document tokenizes the sheet, parses every selector, makes every entry and pushes it into the index again. The earlier note said "the parsed sheet is cached". It is not. The only parse caches are for JS.
2. **The share fell since the step 6 count.** The step 6 count said 30.9% of the suite. On main `5b42d061` the build is 16.3% of the suite, and the build plus the split lists plus the on-demand body reads are 18.1%. Lazy rule bodies (#498) removed most of the old cost. On the 47 corpus journeys the build is 2.16% (2.61% with the split lists and body reads), not 7.8 to 8.3%.
3. **Where the build cost goes (suite fixture, 11.2 M per document):** selector parse 51%, sheet tokenizer 22%, index push 12%, entry creation and the rest 11%, rule bodies 4%.
4. **The work repeats.** In the suite, 191 documents build from one sheet list: 190 builds repeat. In the 47 journeys, 41 of 140 builds repeat a whole list, and 63% of the build instructions are for a sheet that the process built before. Part of that repeat is inside one document: a late `<link>` or an edited `<style>` makes a full rebuild of all sheets (41 such rebuilds).
5. **The two ramps do not gain.** `compm_22000` and `rest_13` build their rule set once. The rule set is 8.5% and 17.3% of them. The "0.6 G fixed cost" of step 4a is not the rule-set build (0.11 G and 0.18 G).

Recommendation (section 5): **a cache of immutable rule sets, keyed by the ordered sheet list and the viewport (`Rc<RuleSet>`), built on process-wide name atoms.** The atoms are the blocker. A selector holds document-local `Atom`s, so no rule set can be shared before that changes. Stage 2, behind a gate: a per-sheet cache of parsed selectors, for lists that change inside one document.

Expected gain (estimate, not built):

| Criterion | Now | After the recommendation |
| --- | --- | --- |
| 7: suite, one worker, instructions | V2 13,101 M. 1.62 times lower than the frozen V0 (21,160 M). 1.38 times lower than V0 of the same binary (18,139 M) | about 10.75 G: 1.97 times (frozen V0), 1.69 times (same-binary V0) |
| 2: 47 journeys, instructions | 92.6 G (88.0% of 105.2 G) | 0.4 G less (0.4%). All sharing together: at most 1.6% |
| 3: `compm_22000`, `rest_13` | 6.4 times, 8.8 times | no change. A cheaper first build (option F, hypothesis) gives 6.5 times and 9.3 times |

## 1. Measurements

### 1.1 Method

- Binary: main `5b42d061`, `bare-drive`, release. Reference build: `/data/bare-tmp/base/5b42d06119`. A second build of the same commit has temporary counters (not committed; the patch is `/data/bare-tmp/rs/instrumentation.patch`). With the counters off, that build costs the same: 13,105 to 13,107 M against 13,090 to 13,094 M on the reference build (suite, 2 of 3 runs; the third run of the counter build gave 12,984 M, which I did not explain).
- Counters read `perf_event_open` (user instructions) around: `parse_stylesheet_lazy`, the copy of the sheet text, the selector parse, the body step, the index push, the rest of `push`, `@media` and `@supports`, `Index::make_bigs`, `read_lazy`, `plan`, the key compare and the whole build.
- A timed interval has an overhead. Calibrated: one `start()` costs 71 instructions, a start and add pair 149. About 75 instructions fall inside each interval. A run with only the outer read (one read pair for each build, mode `total`) gives the exact build total. The fine run is 12% higher. The split below uses the fine run, with 75 instructions taken from each selector, body and index interval. The total is the exact one.
- Cross-check with a method I did not write: `tools/drive/force_count.py` on the reference build gives "rules 2,115 M (16.1%)" for the suite. The counters give 2,134 M.
- Suite: `bare-drive --suite tools/suite/tests --workers 1` (V2: one warm worker, shared login), fixture server, 40 of 40 pass. Journeys: the 47 that pass in the corpus `virtual` (`BARE_CLOCK=virtual`, `BARE_SEED=1`, replay). `nextjs-docs` does not pass on replay and is left out. Ramps: `/data/bare-tmp/bm/b/pages`, `run.steps`.
- Runs: totals are 3 runs each (spread in the tables). Counter runs are 1 run each: they are counts of fixed work under a virtual clock, and two suite runs of the build total were equal to 0.1%.

### 1.2 The suite (one worker, shared login)

Main, 3 runs: V2 13,101 M (13,099 to 13,103). Same run set: V0 18,139 M (18,118 to 18,147), V1 15,206 M. The frozen baseline `f6415364`, V0: 21,160 M (21,159 to 21,160).

| Item | Instructions | Share of V2 |
| --- | ---: | ---: |
| Rule-set build, 191 documents (one build per document, 11.17 M each) | 2,134 M | 16.3% |
| of which the UA sheet (4.6 KB, 0.58 M each) | 111 M | 0.8% |
| Split lists (`make_bigs`, at the first look-up of each document) | 152 M | 1.2% |
| Rule bodies read on demand (`read_lazy`, 14,771 reads) | 82 M | 0.6% |
| `plan` on a read that finds the rule set right (560 passes) | 10 M | 0.1% |
| Rule-set work in all | 2,368 M | 18.1% |

Split of the build (each part is a share of the 2,134 M):

| Part | M | Share | Cost for each |
| --- | ---: | ---: | --- |
| Sheet tokenizer and rule list (`parse_stylesheet_lazy`) and text copy | 461 | 22% | 1.2 M for the 60 KB sheet |
| Selector parse (`parse_selector_list_in`) | 1,085 | 51% | 2,690 per selector (415,425 selectors) |
| Body step (`lazy_block`, `may_declare`, `Body`) | 91 | 4% | 220 per rule |
| Index push (`Index::push`) | 256 | 12% | 590 per entry |
| Entry creation, `Scopes::add`, ancestor hashes, drop of the parse tree, the rest | 241 | 11% | |

The document builds from one sheet list: 1 distinct list in 191 builds (60 KB, 2,272 entries). So the repeat is 190 of 191 builds, 99.5% of the build instructions.

Check with a small sheet: the fixture with `--css-kb 1` gives 10.28 G instead of 13.03 G on main, 2 of 40 tests fail with it. The 2.75 G difference is at least the build, and the rule matching of a larger rule set. It agrees, and it is not exact.

### 1.3 The 47 corpus journeys

Total 92,643 M (3 runs: 92,434 / 92,624 / 92,643 M, spread 0.23%).

| Item | Instructions | Share |
| --- | ---: | ---: |
| Rule-set build, 140 builds for 94 documents | 2,005 M | 2.16% |
| Split lists | 41 M | 0.04% |
| Rule bodies read on demand | 322 M | 0.35% |
| `plan` only | 53 M | 0.06% |
| Rule-set work in all | 2,421 M | 2.61% |

Why not the 7.8 to 8.3% of the step 6 count. Hypothesis: the step 6 count ran before lazy rule bodies (#498), on the real clock, and its "rules" outcome also holds the walk over the sheets. I did not run that commit again.

Repeats:

| Count | Value |
| --- | --- |
| Builds, documents | 140, 94 |
| Full rebuilds inside one document (the list changed, not only grown at the end) | 41 |
| Incremental builds (sheets added at the end) | 5 |
| Builds whose whole ordered list (and viewport) an earlier build of the process had | 41 (338 M, 0.36% of all instructions) |
| Journeys with such a repeat | 24 of 47 |
| Sheet occurrences, distinct sheets (per journey, summed) | 1,062, 325 |
| Build instructions in a sheet that an earlier build had (per-sheet identity, the build cost split by bytes) | 1,257 M = 62.7% of the build: 890 M in the same document, 316 M in an earlier document, 51 M the UA sheet |
| Saved by a rollback to the longest common prefix of the last list of the same document | 765 M |
| Saved by a prefix cache over every earlier list of the process | 1,077 M |

Why a document rebuilds. In `wikipedia-search`, the list grows from 19 to 23 sheets, one sheet at a time. Each new sheet lands in the middle of the list, so the test `plan.parts.starts_with(&b.key)` (`browser_computed.rs:548`) fails and all sheets are built again: 5 builds of 17 to 32 M. In `python-org-docs-link`, an inline `<style>` of 157 bytes becomes 83 bytes and then goes. The 515 KB of the other sheets are built 3 times (27 to 30 M each). The second document of that journey repeats the same three lists.

The five journeys (build and the lists):

| Journey | Total (M, 3 runs) | Build (M) | % | Split lists + body reads (M) | Builds, documents | Distinct lists | Shareable by sheet (M) |
| --- | ---: | ---: | ---: | ---: | --- | ---: | ---: |
| `wikipedia-search` | 7,284 (7,143 to 7,292) | 217.5 | 3.0 | 42.8 | 9, 2 | 9 | 163 |
| `python-org-docs-link` | 1,248 (1,248 to 1,249) | 171.7 | 13.8 | 22.9 | 6, 2 | 3 | 145 |
| `github-docs-home` | 4,935 (4,933 to 4,935) | 195.4 | 4.0 | 28.4 | 3, 2 | 3 | 98 |
| `rust-book-chapters` | 537 (537) | 20.5 | 3.8 | 7.3 | 6, 2 | 2 | 17 |
| `hn-comments-back` | 115 (115) | 3.8 | 3.3 | 1.0 | 3, 3 | 1 | 2 |

Selector parse costs 6,175 instructions per selector on `wikipedia-search`, 5,689 on `python-org`, 3,751 on `github-docs` and 2,690 on the fixture. `github-docs-home` has one 2.6 MB sheet (9,000 entries): 95 M for one build.

All 47 journeys: `/data/bare-tmp/rs/jt1` (build lines), script `perj.py`.

### 1.4 The ramps

Each ramp is one document with one sheet. The rule set builds once.

| Page | Total (M, 3 runs) | Sheet, entries | Build | Split lists | Body reads | All three | Share |
| --- | ---: | --- | ---: | ---: | ---: | ---: | ---: |
| `compm_22000` | 1,834 (1,833.9 to 1,834.0) | 524 KB, 22,154 | 111.6 M | 6.0 M | 38.9 M | 156.5 M | 8.5% |
| `rest_13` | 1,241 (1,240.9 to 1,240.9) | 865 KB, 36,154 | 180.5 M | 9.5 M | 24.6 M | 214.6 M | 17.3% |
| `rest_2` | 580 (580.4 to 580.5) | the same sheet as `rest_13` | 180.5 M | 9.5 M | 24.6 M | 214.6 M | 37% |

- The baseline `f6415364` gives 11.664 G and 10.873 G, so the ratios are 6.4 and 8.8. The target of 10 times is 1.166 G and 1.087 G.
- If the whole rule-set work cost nothing: `rest_13` 1,026 M (10.6 times), `compm_22000` 1,677 M (7.0 times). So even the best sharing would not meet criterion 3 on `compm_22000`, and a cache cannot help at all: the ramp has one document.
- The "0.6 G fixed parse and index build" of step 4a is not the build. `rest_2` costs 580 M, the build 215 M. An IBS profile of `rest_13` (`/data/bare-tmp/rs/ibs-rest13.data`) shows `RuleSet::restyle` 13.4%, `Index::gather` 9.9%, `compound_matches` 6.5%, `collect_split` 5.7%. Hypothesis: the rest of the fixed cost is the page parse, JS and layout. Not measured.

### 1.5 Verdict on the hypothesis

| Claim | Verdict |
| --- | --- |
| The rule set and its index are built again for each document | Confirmed (suite: 191 builds; journeys: 94 documents, 140 builds). |
| The parsed sheet is cached | Rejected. The tokenizer and the selector parse run in every build (73% of it). |
| The step 6 share (30.9% suite, 7.8 to 8.3% journeys) holds today | Rejected. 16.3% and 2.2% (18.1% and 2.6% with the split lists and body reads). |
| The sheet registry (#490) and the parse cache did not change it | Right, and they could not: the registry is the list of `<link>`, `<style>` and `<base>` nodes. |
| The fixed cost of the ramps is the build | Rejected. 8.5% and 17.3%. |

## 2. The current design

All paths are at `5b42d061`.

**When a build happens.** `Dom::cascade_inner` (`browser_computed.rs:525`) makes the list of sheets of the document (`plan`, `browser_computed.rs:344`): the text of each `<style>`, each loaded `<link>` and each adopted sheet, in tree order. The list is `Built::key`. When the list or the viewport is not equal to the key of the cache, a new `RuleSet` is built. The one exception is a list that only grew at the end (`starts_with`, line 548): then the sheets at the end are added to the old rule set. A `Computed` (one for each document) holds one `Built`. Nothing outlives the document.

**What a build does** (`RuleSet::add_rules`, `cascade.rs:895`; `UA_CSS` first, then the sheets):

1. `parse_sheet` (`cascade.rs:833`): copy the text into `sources`, then `parse_stylesheet_lazy`. It makes a `Vec<Rule>` with the selector text and the place of each block (the block is not read).
2. For each style rule: `parse_selector_list_in(prelude, doc, mode)` makes the `Selector`s. It interns the tag and attribute names into the table of the document `doc` (`Atom`).
3. A rule with a block that cannot declare anything makes no entry (`may_declare`). Else a `Body` (place of the text, `unread`) is added.
4. `RuleSet::push` (`cascade.rs:965`): `Scopes::add`, `user_flags`, a push into `Index` (five maps from key to `Vec<u32>`: id, class, tag, attribute and `other`), `ancestor_hashes`, then an `Entry` is added.
5. `@media` and `@supports` are evaluated at this time against the viewport (`media_matches`, `supports_matches(doc, ..)`). A rule in a false block is not added.

**After the build**, lazily and once: `Index::make_bigs` splits the long lists by an ancestor key (#494), at the first look-up. `read_lazy` reads the declarations of a rule when an element first matches it (#498). Both are interior state of the `RuleSet`.

**What a `RuleSet` holds** (`cascade.rs:403`): `entries: Vec<Entry>`, `bodies`, `sources` (the text of every sheet), `decls`, `side` and `customs` (the read declarations and what `Value`s point to), `layers`, `index: Index`, `shadows: HashMap<NodeId index, Shadowed>`, `parts`, `scopes`, `reach` (lazy), `pending` (the retained CSSOM inline inputs of this document), and counters (`tested`, `styled`).

**Entry** (`cascade.rs:119`): `selector: Selector` (owned), `body`, `layer`, `origin`, `ancestors: [u32; 4]`, `ctx`.

**Why it is bound to one document.** Four things:

| Item | Where | Bound to |
| --- | --- | --- |
| `Simple::Tag(Atom)`, `Simple::Attr { name: Atom }`, `Buckets<Atom>` for tags and attributes | `selector.rs:111`, `cascade.rs:164` | The name table of the document. `Atom` is a number in `Document::names`. "Atoms of different documents mean nothing together" (`dom.rs:72`). Only ids 0 (`id`) and 1 (`class`) are fixed. The others come in the order of first use. |
| `tag_hash(atom)` in `ancestors` and in the split keys | `cascade.rs:2956` | The atom number. |
| `shadows` keyed by the `NodeId` of the shadow root, and `scope` in each rule | `cascade.rs:965` | The nodes of the document. |
| `pending` (`set_inline_inputs`) and `add_inputs` for a sheet that a script made with the CSSOM | `cascade.rs:791`, `browser_computed.rs:558` | The document. |

Class and id names are `Box<str>` (text, not atoms), so they do not bind the rule set.

**Complexity.**

| Part | Time | Memory |
| --- | --- | --- |
| Tokenizer and rule list | O(sheet bytes), 1.2 M for 60 KB (20 per byte) | text copy + `Vec<Rule>` (freed at the end of the build) |
| Selector parse | O(selectors x length), 2,690 per selector on the fixture | about 230 B per selector (design-style.md) |
| Index push | O(entries), about 590 per entry, a `Box<str>` clone for each class or id key | `Entry` about 105 B; the index 4.2 to 4.9 B per rule after the split (#494) |
| Split lists | O(entries in long lists), 0.8 M for 2,272 entries | the peak 18 to 19.5 B per rule, freed at once |
| Build, all | O(sheet bytes) for each document: 182 instructions per byte of the fixture, 4,900 per entry | about 478 B per rule at peak (design-style.md) |

The cost is the same for the second document with the same sheet, and for the fifth rebuild of one document.

## 3. Prior art

Sources I read are named. What I could not open or did not find is marked "to verify".

### 3.1 Stylo (Firefox, Servo)

Source: [`style/stylist.rs`](https://github.com/servo/stylo/blob/main/style/stylist.rs) (first 100,000 characters of 195,710), [`style/author_styles.rs`](https://github.com/servo/stylo/blob/main/style/author_styles.rs), [`style/stylesheet_set.rs`](https://github.com/servo/stylo/blob/main/style/stylesheet_set.rs).

- **UA cascade data is shared by all documents.** `static UA_CASCADE_DATA_CACHE: LazyLock<Mutex<UserAgentCascadeDataCache>>`, "A cache of computed user-agent data, to be shared across documents". The cache is `CascadeDataCache<Entry>`: `FxHashMap<CascadeDataCacheKey, Arc<Entry>>`. The key is `CascadeDataCacheKey { media_query_results: Vec<MediaListKey>, contents: StyleSheetContentList }`. `contents` is a list of `Arc<StylesheetContents>` compared by pointer. The media query results are in the key; the device is not (only through those results). A comment says the cache "may need to be keyed on quirks-mode too".
- **Author data in a shadow tree is cached the same way.** `Stylist.author_data_cache: CascadeDataCache<CascadeData>`, "A cache of CascadeDatas for AuthorStylesheetSets (i.e., shadow DOM)", the same key. `remove_unique_author_data_cache_entries` calls `take_unused()`: an entry that nobody but the cache holds is dropped. That bounds the cache, and it means the cache lives while some document uses an entry.
- **Document author data is not shared.** `DocumentCascadeData { user_agent: Arc<..>, user: CascadeData, author: CascadeData, .. }`. Only the UA data is `Arc`. The author data of the main document is built from the sheets of the document and owned.
- **A rebuild.** `UserAgentCascadeData::rebuild` is full: new `CascadeData`, `add_stylesheet(.., SheetRebuildKind::Full, ..)` for each sheet. A TODO says that an incremental rebuild may be worth it. `Stylist::flush` rebuilds only when `stylesheets.has_changed()`.
- **What a sheet change costs.** In `stylesheet_set.rs`: `append` leaves the validity alone ("Appending sheets doesn't alter the validity of the existing data"), and an uncommitted sheet is a `Full` rebuild. `insert_before` sets `DataValidity::CascadeInvalid`. A removal of a committed sheet sets `FullyInvalid`. `rule_changed` sets `FullyInvalid` for every kind ("TODO"). A sheet that is appended and removed before a flush does not cause a rebuild (bug 1434756, named in a comment).
- **The parse is shared elsewhere.** Selectors are parsed once into `StylesheetContents` (`Arc`). The `CascadeData` builder inserts those parsed rules in its maps. I read that `CascadeData::add_stylesheet` takes the parsed rules, but the file I could read does not show its body: **to verify**.
- Atoms in Stylo are process-wide (Gecko atoms, and `string_cache` in Servo): **to verify**, I did not open the file.

### 3.2 Blink (Chrome)

Source: [`style_sheet_contents.h`](https://github.com/chromium/chromium/blob/main/third_party/blink/renderer/core/css/style_sheet_contents.h), [`scoped_style_resolver.h`](https://github.com/chromium/chromium/blob/main/third_party/blink/renderer/core/css/resolver/scoped_style_resolver.h), [`style_engine.h`](https://github.com/chromium/chromium/blob/main/third_party/blink/renderer/core/css/style_engine.h). Headers only: no `.cc` file.

- **The RuleSet belongs to the sheet.** `StyleSheetContents` has `Member<RuleSet> rule_set_` and `RuleSet& EnsureRuleSet(const MediaQueryEvaluator& medium, ..)`. The comment says that `medium` must be the same at each call. A `CreateUnconnectedRuleSet` makes a copy not owned by the sheet.
- **A sheet is shared.** The flags `IsUsedFromTextCache` ("StyleEngine::CreateSheet()") and `IsUsedFromResourceCache` (`CSSStyleSheetResource::CreateParsedStyleSheetFromCache`) say the contents "may be shared between multiple CSSStyleSheets". `ClientSize`, `HasOneClient`, `IsMutable`, `StartMutation` and `Copy()`: a sheet that is in the resource cache is copied before a change ("We must not modify this StyleSheetContents while this is true").
- **The text cache is per document.** `text_to_sheet_cache_` is a member of `StyleEngine`, which is one for each document. So a `<style>` text is shared in a document. A sheet from a resource is shared through the resource cache. That the `RuleSet` (not only the parse) is shared between documents: the header does not say. **To verify.**
- **Matching goes over the sheets.** `ScopedStyleResolver` has `ActiveStyleSheetVector active_style_sheets_`, a `ForAllStylesheets` helper and a `CascadeLayerMap`. So Blink keeps one `RuleSet` for each sheet and matches each of them, in order. It does not merge them into one index. (From the names and the signatures. The body of `CollectMatchingElementScopeRules` is not in the header: **to verify**.)
- **Sheet changes.** `StyleEngine::ApplyRuleSetChanges(TreeScope&, old_sheets, new_sheets, diffs)` takes both lists and per-sheet `RuleSetDiff`s, so a change is a diff of two lists, not a rebuild. "Passing the same vector for an origin's old and new sheets means that origin is known to be unchanged." The kinds `kActiveSheetsChanged` and `kActiveSheetsAppended` are in another header that I did not read: **to verify**.

### 3.3 WebKit

Source: [`StyleScopeRuleSets.h`](https://github.com/WebKit/WebKit/blob/main/Source/WebCore/style/StyleScopeRuleSets.h), [`UserAgentStyle.cpp`](https://github.com/WebKit/WebKit/blob/main/Source/WebCore/style/UserAgentStyle.cpp), [`StyleSheetContents.h`](https://github.com/WebKit/WebKit/blob/main/Source/WebCore/css/StyleSheetContents.h), a search result for `appendAuthorStyleSheets`.

- **The UA rule set is built once for the process.** `RuleSet* UserAgentStyle::defaultStyle` (also `defaultQuirksStyle`, `defaultPrintStyle`) are static pointers. `initDefaultStyleSheet()` returns at once when `defaultStyle` is set. They use `leakRef()`: never freed. Other UA sheets (SVG, MathML, ...) are added the first time an element needs them. Rules with a media query that is not screen or print go in a separate sheet: "Rulesets for these can't be global and need to be built in document context."
- **Author rules are built for each document.** `ScopeRuleSets` has `RefPtr<RuleSet> m_authorStyle`. `appendAuthorStyleSheets` makes a `RuleSetBuilder` and calls `addRulesFromSheet(cssSheet->contents(), mediaQueries)` for each sheet (from a secondary description of the source, not the file). `resetAuthorStyle` drops it. So WebKit shares the parsed `StyleSheetContents` (clients, memory cache: `isInMemoryCache`, `isCacheable`) and rebuilds the index of each document. The same description says a fix stops the same `StyleSheetContents` being added twice in a row (bug 273455).
- WebKit atoms (`AtomString`) are process-wide: from my knowledge, **to verify**.

### 3.4 Ladybird

Source: [`StyleComputer.h`](https://github.com/LadybirdBrowser/ladybird/blob/master/Libraries/LibWeb/CSS/StyleComputer.h), [`SharedCompiledStyleSheet.h`](https://github.com/LadybirdBrowser/ladybird/blob/master/Libraries/LibWeb/CSS/SharedCompiledStyleSheet.h). The rule data has moved into a Rust style engine; I did not read the Rust side.

- A document has `HashMap<SharedCompiledStyleSheetKey, RefPtr<SharedCompiledStyleSheet>> m_shared_compiled_style_sheets`. It is a member of the `StyleComputer` of one document, not a static: this file shows no process-wide cache. The key is `{ FlatPtr contents_identity; String base_url; }`. It has no media, viewport, layer or scope.
- The entry holds the compiled contents (`StyleSheetState`) and `m_attachment_counts: HashMap<TreeScopeID, size_t>`: which tree scopes use the sheet.
- UA and user sheets "are shared between documents, so a per-document identity can't sit on the sheet" (a comment). A document keeps at most `max_detached_shared_compiled_sheets = 8` entries that no tree scope holds.

### 3.5 What I take from it

| Idea | Engines | Do we copy it? |
| --- | --- | --- |
| The UA rule set is built once and shared | Stylo (cache with a lock), WebKit (static, leaked) | Yes, as part of any option here. |
| Parsed sheet contents are shared by identity; each document builds its own index | Stylo, WebKit | Option B. |
| One rule set for each sheet, matched in order | Blink | Option C. No (matching cost). |
| A cache of built data keyed by the sheet identities and the media query results | Stylo (UA and shadow DOM) | Option A. |
| An unused entry is dropped (`take_unused`); at most 8 detached entries | Stylo, Ladybird | Yes, as the retention rule. |
| A diff of the old and the new sheet list | Blink | Option D (a smaller form). |
| Process-wide atoms, so a parsed selector is not bound to a document | all four (to verify) | **Yes. It is the first step of every option that shares.** |

None of the four engines shares the index of the document's own author sheets across documents; Stylo does it for UA and shadow trees. They avoid the cost another way: the parse is shared, and the index build is cheap (Arc'd selectors inserted in maps).

## 4. Options

Every option except F needs the same first step (section 4.1).

### 4.1 The enabling step: process-wide name atoms

A shared rule set holds `Atom`s. They must mean the same in every document. Options for that:

| Way | Cost | Verdict |
| --- | --- | --- |
| Check at attach that the name table agrees with the atoms of the sheet | A table that the HTML parser filled in the order of its first use. Two pages give two orders. The hit rate is low (hypothesis). | Rejected. |
| A name id inside the selector, and a per-document table from sheet id to atom at match time | One more load in `compound_matches` (37% of `rest_13`'s profile is matching) and two meanings of `Simple::Tag` (a script `querySelector` parses with the document atoms). | Rejected. |
| Rewrite the atoms of the selectors when a document attaches a shared set | A clone of each selector: about 5 to 8 allocations (compounds, simples, `Box<str>` classes). About 700 instructions each. That is the cost of the build we want to avoid. | Rejected. |
| **One interner for the thread. A document keeps what it uses.** | `dom.rs` only: `names`, `atoms`, `ns_atoms`, `sheet_names`, `intern`, `find_atom`, `atom_str`, `atom_at`, `name_count` (all uses of these are inside `dom.rs` except `name_count` and `atom_at`, 10 call sites in `bare` and `bare-css`, which keep their meaning). | **Chosen.** |

How it works. `Atom` is a number in a table of the thread, `Atom::ID` and `Atom::CLASS` stay 0 and 1. A `Document` keeps `used: Vec<Atom>` (in order of first use in this document) and a bitmap by atom number. `intern(name)` looks in the thread table, and adds the atom to `used` when the bit is not set. `name_count()` is `used.len()`. `atom_at(i)` is `used[i]`. So the `dom-names` limit and the scan for new event-handler names (`browser_handlers.rs:316`) keep their meaning. `tag_hash` uses the global number, so the hashes of `Entry::ancestors` do not depend on the document.

Cost: one more bit test for each `intern`; the HTML tokenizer interns each tag and attribute name of a page (a hash lookup now as well). Hypothesis: below 0.1% of a journey. Not measured.

Risk: the table only grows (a `&'static str` for each name). A warm worker keeps names of every document. Bound: a limit on the size of the table, and the caches below drop their entries when the table is cleared (an epoch). The rule for that is an open question (section 7).

### 4.2 Option A: a cache of rule sets keyed by the sheet list

**Idea.** `Rc<RuleSet>`, immutable after the build. A thread-local cache maps `key -> Rc<RuleSet>`. `cascade_inner` computes the key, looks up, and builds only on a miss. A repeated list costs a lookup.

**Key.** `(viewport, [(content hash, length, layer) for each part])`. The UA sheet is part of every rule set, so it is not in the key. The content hash of a sheet is made once, when the text arrives (`set_external_stylesheet`, or the `<style>` text read in `plan`), not at each read. Hash cost is about 0.15 instructions per byte with a word-wise hash (estimate): 9,000 for 60 KB, against 11.2 M for the build.

**What must change in `RuleSet`.**

- Atoms: section 4.1.
- `pending` (the retained CSSOM inline inputs, `set_inline_inputs`) is per document. It moves out of the `RuleSet`, into `Built`, and is passed to the walk. A document with `pending` or with `plan.inputs` (a sheet made by CSSOM calls, `add_inputs`) does not use the cache.
- A list with a part in a shadow tree (`scope != root`) does not use the cache: the key would hold `NodeId`s. This is 1 of 140 builds on the journeys.
- The incremental path (`add` to the old rule set at the end of the list) cannot write into an `Rc` that others hold. It uses `Rc::get_mut` and falls back to a new build (what happens today when the list is not a prefix). A rule set that grew by `add` gets a new key.
- What is already interior and deterministic is shared as it is: `big` (made at the first look-up), `reach`, `decls`, `side`, `customs` (the lazy body reads: a second document finds them read), `tested` and `styled` (the callers take differences around one call).

**Complexity.**

| Item | Value |
| --- | --- |
| Time, a hit | O(parts): the hash of each part is known; a map lookup |
| Time, a miss | the build of today |
| Memory | the rule sets of the cache, shared. The same bytes as today for one document. |

**Retention.** A cold process (one journey) keeps a `Weak` only: an entry lives while a document holds it. So its memory is the same as today (criterion 6 is not changed). A warm worker (`--suite`, `bare-mcp`) keeps the last N rule sets, by a budget of entries (for example 100,000 entries, about 50 MB at 478 B each: **hypothesis**, to measure; Ladybird keeps 8, Stylo keeps what a document holds). Unused entries go first (`take_unused`).

**Invalidation.**

| Event | Result |
| --- | --- |
| `<style>` text changes, `<link>` gets a sheet, a sheet is added or removed | The key changes. A hit if that list was built before, else one build. Same as today. |
| `insertRule`, `deleteRule`, `replaceSync`, an adopted sheet | `plan.inputs` or a new text: no cache (CSSOM retained blocks) or a new key. |
| The viewport changes | The key has the viewport. The media queries are evaluated at the build, so a new size is a new key. |
| `@supports(selector(..))` | Evaluated with `doc` at the build. With global atoms it reads no document state (**to verify**: `supports_matches` interns names into `doc`). |
| A dynamic `<link>` or `<style>` insert | The list changes. If it grows at the end, today's incremental path is lost for a shared rule set (a new build, as for an insert in the middle). Hypothesis: a rare case; 5 incremental builds in 140. |

**`:hover` and focus.** The `RuleSet` holds no user state. `user` (the `USES_*` flags) is a property of the rules and is the same for each document that shares them. The state is in `Document::user()`. No effect.

**Shadow trees.** A list with a shadow-tree sheet is not shared (above). The document tree of the shared rule set has `shadows` empty.

**Expected gain** (estimate; each from the measured parts above).

| Workload | Saved | Result |
| --- | --- | --- |
| Suite, V2 | 190 builds x 11.17 M = 2,122 M; the split lists of 190 documents 151 M; the body reads of 190 documents about 80 M | 13,101 to about 10,750 M (-18%) |
| 47 journeys | the 41 exact repeats: 338 M, and their share of the split lists and body reads (about 60 M) | 92,643 to about 92,240 M (-0.4%) |
| `compm_22000`, `rest_13` | nothing: the first build is a miss | 0 |

### 4.3 Option B: a cache of parsed sheets, an index for each document

**Idea** (Stylo and WebKit). Keep one `SheetParse` for each sheet text: the rule tree after the parse, with `Rc<Selector>` (selectors made once), the place of each lazy block, the `may_declare` flag, the `@media` and `@layer` tree, and for each selector the values that do not depend on the document (`user_flags`, `ancestor_hashes`, the bucket key). `add_rules` walks that tree, evaluates `@media` for the viewport, and pushes entries and index keys. The document keeps its own index, `bodies`, `decls`. No tokenizer, no selector parse.

**Key.** `(content hash, length, mode)`; `mode` is document or shadow (the parse differs for `:host`).

**Needs.** Atoms (4.1). `Entry::selector` becomes `Rc<Selector>` (one more pointer in the hot path, `entries[i].selector`; hypothesis: no cost, to measure). A retained parse tree is large: about 230 B for each selector, 8 B for each byte of CSS (2.6 MB sheet: about 20 MB). Needs a byte budget.

**What stays per build.** Index push (12% of the build), entry creation and the walk (about 11%), the split lists, the body reads. So the residual is 23 to 27% of the build (estimate from the split in 1.2).

**Complexity.** Time per build with a cached parse: O(entries) with a small constant, about 1,100 instructions per entry (index push 590 plus entry 520), against 4,900 now. Memory: the parse trees of the cache, plus the document's own index (as today).

**Invalidation.** Per sheet, by text: an edited `<style>` is a new key and parses again; the other sheets keep their parse. A `<link>` that arrives in the middle of the list costs the index build of all sheets (about 1,100 per entry), not the parse. `:hover`, shadow trees: as in option A, but shadow-tree sheets can be shared (the parse does not hold a `NodeId`; `scope` is given at the attach).

**Expected gain** (estimate; residual 23 to 27%).

| Workload | Saved | Result |
| --- | --- | --- |
| Suite, V2 | 73 to 77% of 2,122 M = 1,550 to 1,630 M | 13,101 to 11,480-11,550 M (-12%) |
| 47 journeys | 73 to 77% of the 1,257 M that repeat a sheet: 920 to 970 M | 92,643 to about 91,700 M (-1.0%). Of this, 890 M x 0.75 = 670 M is inside one document. |
| Ramps | nothing | 0 |

### 4.4 Option C: one rule set for each sheet, matched in order (Blink)

**Idea.** Each sheet has its own `Rc<SheetRules>` with its own `Index`. A document is a list of them. For an element, each sheet's index gives candidates and the cascade merges them by order. Entry id = `(sheet number << 24) | local number`, so the numeric order is the source order.

**Gain.** The build per document is zero for a repeated sheet. A sheet change costs only that sheet.

**Cost.** An element probes every sheet: id, each class, tag, attributes, `other`. `wikipedia-search` has 19 to 23 sheets, so about 20 times the hash probes and a merge of up to 20 sorted lists for each element. The matching is the largest part of the style cost (about 37% of `rest_13`, 4.3 to 8.8% of a journey). The `Value`s in the declarations point into the `side` table of one rule set, so the declaration store must be split by sheet too, and `Styles::calcs` must name the sheet. That is the widest change of the options. Hypothesis: a loss on every page with many sheets. A merged index for the small sheets would bring back a build. Not recommended.

### 4.5 Option D: roll the rule set back to the common prefix

**Idea.** On a rebuild, keep the rule set up to the longest common prefix of sheets with the old list (`truncate` the entries and pop the index lists), and add the changed sheets. A prefix cache over all earlier lists of the process is the cross-document form.

**Gain** (measured, section 1.3): 765 M on the journeys (same document), 1,077 M with all earlier lists. The suite: 0 for the same document and about 2,012 M with all earlier lists.

**Cost.** `RuleSet` must be cloned or undone: the index lists are `Vec<u32>` that grow in order, so a rollback pops while `entry >= n` in every list (O(lists)), and `Scopes`, `user`, `reach`, `layers`, `keyframes`, `logical`, `motion` must be undone too. A cross-document form needs a clone with `Selector` owned (about 700 instructions each) or sharing. Stylo's `append` and `insert_before` rules say the same: an insert in the middle is a `CascadeInvalid` rebuild. It is a good fix for the in-document rebuilds only. It does not help the suite. Not recommended alone.

### 4.6 Option E: the UA sheet built once per process

**Idea** (WebKit, Stylo). Build `UA_CSS` once and share it.

**Gain.** 0.58 M for each document: 111 M on the suite (0.85%), 51 M on the journeys (0.06%).

**Note.** It needs the same step 4.1, and a rule set made of two parts (shared UA entries, then the document's own). It is simple only inside option B or C. Inside option A the UA sheet is in each cached set. Not worth its own change.

### 4.7 Option F: a cheaper first build (no sharing)

**Idea.** Attack the 73% that the cache of option B would remove, but for the first build too. The selector parse costs 2,690 instructions for a selector of 10 to 30 bytes: it tokenizes the whole text into a `Vec<Token>`, then parses (`parse_scoped`: `tokenize(text)` first). A hand-written scan for the common form (`tag`, `.class`, `#id`, `[attr]`, descendant and child combinators) would not allocate tokens.

**Gain** (hypothesis: 500 instructions per simple selector, 70% of selectors simple).

| Workload | Saved | Result |
| --- | --- | --- |
| `rest_13` | about 76 M (6%) | 1,241 to about 1,165 M: 9.3 times (target 10) |
| `compm_22000` | about 47 M (2.6%) | 1,834 to about 1,787 M: 6.5 times |
| Suite, V2 | about 0.5 to 0.7 G | -4 to -5% |

**Note.** It is the only option that helps a cold single-document page, such as the ramps. It does not remove the repeat, so it does not replace A. A and F are independent. Out of scope here: it is a parser change, and it needs its own design and its own differential test against the old parser.

### 4.8 The options side by side

All gains are estimates. "Journeys" is the share of the 92,643 M. The suite is V2.

| | A: list cache | B: parse cache | C: per-sheet sets | D: prefix rollback | E: UA once | F: cheaper first build |
| --- | --- | --- | --- | --- | --- | --- |
| Shares | the whole set | selectors and entries; index per document | everything per sheet | a prefix of the set | the UA set | nothing |
| Needs global atoms | yes | yes | yes | for cross-document only | yes | no |
| Build for a repeated list | none | 23 to 27% | none | none for a prefix | UA part only | full |
| Change inside a document | new build | the changed sheet's parse | only that sheet | rebuilds from the change | new build | new build |
| Match cost | same | same | up to about 20 times the probes with 20 sheets | same | same | same |
| Memory | cached sets (budget) | parse trees (8 B per CSS byte, budget) | per-sheet sets | one set + an undo log | small | none |
| Suite V2 | -2.35 G (-18%) | -1.55 to -1.63 G (-12%) | -2.35 G, matching slower | 0 | -0.11 G | -0.5 to -0.7 G |
| Journeys | -0.4% | -1.0% | -1.4%, matching slower | -0.8% to -1.2% | -0.06% | about -0.5% |
| `compm_22000`, `rest_13` | 0 | 0 | 0 | 0 | 0 | 6.5 times, 9.3 times |
| Change size | medium: atoms, `pending`, a cache | large: atoms, `Rc<Selector>`, a parse tree, a new `add_rules` | largest: split declaration store | medium | small | medium |
| Suite criterion 7 (against frozen V0 21,160 M / same-binary V0 18,139 M) | 1.97 / 1.69 | 1.83 / 1.57 | 1.97 / 1.69 | 1.62 / 1.38 | 1.63 / 1.39 | 1.68 / 1.43 |

## 5. Recommendation

**Build option A on top of the process-wide atoms of section 4.1, in this order, each step its own PR with a measurement:**

1. **Process-wide atoms** (4.1). No change of behavior. Gate: the cost of `intern`, measured on the cost matrix and the 47 journeys; the test suite of `bare-html` and `bare-css` unchanged.
2. **The cache of rule sets** (4.2): `Rc<RuleSet>`, key by sheet list and viewport, `pending` out of the rule set, `Weak` in a cold process, a budget in a warm worker. A fuzz test: the matched list of a cached rule set equals the matched list of a new build, for random lists, viewports and documents (as `index_tests.rs` does for the split).
3. **Stage 2, only if a gate says so:** option B for the case where lists change inside one document. Gate: after step 2, the in-document rebuilds still cost 890 M (1.0%) on the journeys and a real warm suite has lists that change from page to page (a CSS-in-JS site, for example). The fixture of the suite does not. The decision is the owner's.

Why A, not B first:

- It is the only design that makes the build unneeded for a repeat. The suite fixture is a case where it holds for 190 of 191 documents.
- It reaches the most on the suite (-18%) with the smaller change. B reaches 12% with a large change.
- It does not change what an element sees: the rule set is the same, so matching, the split lists and the cascade do not change.
- It gives no gain on the journeys (0.4%) and none on the ramps. No sharing design can: the journeys have 2.6% of rule-set work, so the most all sharing can give is 1.6% (section 1.3). The suite is where it counts.

Why not C: matching gets slower on pages with many sheets, and the declaration store would have to be split. Why not D alone: it does nothing for the suite.

Expected gain per criterion (estimate, with the measured parts):

| Criterion | Now (main `5b42d061`) | After step 2 | After step 3 (if built) |
| --- | --- | --- | --- |
| 7, instructions with one worker | V2 13,101 M. Against frozen V0: 1.62 times (met). Against the V0 of the same binary: 1.38 times. | about 10,750 M. 1.97 times and 1.69 times. | about 10,600 M (in-document rebuilds of the fixture: none). Same ratios. |
| 7, wall time | not measured here | hypothesis: about 18% less CPU time, with workers it follows the pool, not this change | |
| 2, 47 journeys | 92.6 G, 88.0% of the baseline 105.2 G (target 80% = 84.2 G) | about 92.2 G (87.6%) | about 91.3 G (86.8%) |
| 3, `compm_22000` and `rest_13` | 6.4 times and 8.8 times | no change | no change |
| 6, RSS of `python-docs-search-results` | 116 MiB | no change (a cold process keeps a `Weak`) | no change |

The suite section of `docs/progress.md` (line 8646) gives 1.35 times: V0 21,175 M and V2 15,682 M, from an older main. On main `5b42d061` V2 is 13,101 M, so against the frozen V0 (21,160 M, 3 runs) it is 1.62 times. Against V0 of the same binary (18,139 M) it is 1.38 times. Which V0 is the criterion is the owner's call: `docs/perf-goal.md` row 7 says "sequential cold runs", which does not name a binary.

## 6. Risks

| Risk | Why it matters | What to do |
| --- | --- | --- |
| The global table grows without bound in a warm process | A hostile or long-running page adds names; the worker is long-lived. | A size limit with an epoch: when the table passes the limit, caches drop their sets and new documents start a new table. Documents that live keep the old table. An open question (section 7). |
| A stale rule set after a missed change | A wrong style is worse than a slow one. | The key is the content: a sheet that changes has a new hash. The fuzz test compares a cached set with a new build. A debug build that builds again and compares on each hit (as the sheet registry did with a walk). |
| `pending` and the CSSOM inputs make a set document-specific | A script that edits a sheet with the CSSOM must not change another document's rules. | A document with `pending` or `inputs` bypasses the cache. A test: two documents, one edits a rule, the other keeps the old one. |
| Shared lazy state (`read_lazy`, `side`, `big`) is written by a document and read by the next | Single thread: a `RefCell` borrow does not overlap. `Styles` hold indices into `side`, which only grow. | The suite workers are processes; docs of one thread share the cache. Never share across threads: `Rc`, not `Arc`. A test that a `Styles` of an old document is valid after a second document read more bodies. |
| Memory of a warm cache | 478 B per rule (design-style.md, hypothesis for today's code) x 100,000 entries = about 50 MB. | Measure the real size of a rule set with a counting allocator before the budget is set. Peak RSS of a warm worker is a number to record (26.5 MiB for the suite worker now). |
| The gain does not show on the criteria that are open | Criterion 2 needs 8.5 G less; this gives 0.4 G. Criterion 3 gets 0. | Say so in the PR. The gain is for the suite and for the next steps (the rule set is 18% of the suite). |
| Process-wide atoms change a core type | `Atom` equality across documents was "meaningless" and now is meaningful. Code that relied on a small table (`atom_at(i)`) | Keep the per-document `used` list; test that `name_count` and `atom_at` give what they gave. |

## 7. What I did not verify

- **Hypothesis:** `intern` with a process-wide table costs below 0.1% of a journey.
- **Hypothesis:** the hit rate of the cache on real warm suites. The fixture has one sheet list for all pages. A real app with route-specific `<style>` tags (CSS-in-JS) may not repeat a list. The journeys show 41 of 140 exact repeats, which is a different case (public sites, a fresh process each).
- **Hypothesis:** a cached `Rc<RuleSet>` has the same match speed as today. The `Rc` adds no hot-path pointer, but I did not build it.
- **Hypothesis:** a rule set is 478 B for each rule today (a number from design-style.md for the frozen commit). I did not measure the live size of one rule set at `5b42d061`.
- The fine split (section 1.2) is from one run with counters that cost 12% of the build. The correction (75 instructions for each interval) is an estimate. The totals are exact (two runs with only outer reads, and the independent `force_count.py` figure).
- The step 6 share (30.9%, 7.8 to 8.3%): why it differs from today is a hypothesis (lazy bodies). I did not run the old commit.
- The first suite counter run lacked the phase split of the first build (a counter bug, fixed for the later runs): 1 of 191 builds. It does not change the totals.
- I did not run the cost matrix, the test262 gate or the full workspace tests: nothing was built.
- Wall time of the suite and of the journeys (the machine is shared; load 6 to 22).
- The `.cc` bodies of Blink, `CascadeData::add_stylesheet` of Stylo, and the Rust style engine of Ladybird. A claim about them is marked "to verify".
- `supports_matches(doc, ..)` reads the document only to intern names (from its signature `&mut Document`): **to verify** that it reads nothing else.
- The step-4a fixed cost of the ramps: where the 365 M of `rest_2` outside the rule set goes. Not measured.
- A second machine, `bare-mcp` sessions, `--window`.

## 8. Noticed, not changed

- `plan` copies the text of every sheet for each read that follows a change (`text.to_string()` in `add`, `browser_computed.rs:365`) and compares it with the key: 8,600 instructions for the 60 KB sheet, 560 times in the suite (10 M), 0.6 to 2.1 M for each read on `wikipedia-search`. A content hash would remove both.
- `Index::push` clones a `Box<str>` for each class or id key (`by_class.entry(class.clone())`). Part of the 590 instructions of a push.
- `Scopes::add` and `ancestor_hashes` run for every selector at the build, also for rules that no element can match.
- A late `<link>` always forces a full rebuild, because the list is no longer a prefix (41 times on 47 journeys, 890 M).
- The loader's `flatten` (rewrites the sheet text for each document, `browser_loader/sheets.rs:316`) is 1% of the suite by self time in an IBS profile (`/data/bare-tmp/rs/ibs1.data`). It repeats for each document with the same sheet, as the build does. Not measured by inclusive time.
- The measurement counters are not in the repository: the patch is `/data/bare-tmp/rs/instrumentation.patch` and `rsstat.rs`, the scripts are in `/data/bare-tmp/rs/`.
