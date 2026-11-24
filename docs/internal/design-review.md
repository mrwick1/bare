# Independent review: remove work at the journey boundary

2026-11-13. Read-only review. Only this requested report was written.

## Verdict

Keep the two subsystem designs, but do not sell their estimates as a route to 2x across the public journeys. The strongest additional design is an explicit observation boundary: defer rendering work until a browser API or driver operation needs it. Start with rasterization, then style, then layout dependencies. Cross-run immutable code reuse is useful, but has a much smaller ceiling than its analogy with V8 might suggest.

A credible gain above 2x requires removing repeated application startup across related tests, changing which resources the test runs, or improving suite throughput through concurrency. None has demonstrated 2x CPU reduction across these 48 journeys. Preserve this distinction in the next proposal.

## Evidence and accounting

Read both design documents in full. References below use their section numbers:

- Style: `/home/mrwick/.t3/worktrees/bare/t3-2c9d44b4-2e9bcc1e94c9fd55/docs/design-style.md`.
- JS: `/home/mrwick/.t3/worktrees/bare/t3-ba9de86c-4963a6648ea0fbb5/docs/design-js.md`.
- Frozen measurements: `/data/bare-tmp/prof/{cpu,mem}/report.md`; benchmark reports `/data/bare-tmp/bm/{a,b,c}/report.md`.
- Inspected current source at `b7bd1d5a41a3a2f049c600a812c0573103d67730`. Current source is corroboration, not proof of frozen-commit behavior.

The attributed CPU shares partition sampled user instructions. Inclusive shares overlap and cannot be added. Instruction reduction, elapsed time, peak RSS and suite throughput are different objectives.

The CPU report says network waits and kernel time are excluded. The public benchmark also compares an entire bare journey with Chrome's first-navigation load time. Its Chrome RSS sums shared mappings repeatedly. These figures establish neither matched readiness nor a precise private-memory ratio. Real logins were not measured.

At the median, removing every non-image byte could save only about 23% RSS. A 2x median RSS improvement needs a smaller resident program image, or sharing measured by PSS/private bytes across workers. It cannot come from AST or object changes alone.

The combined proposals remove roughly 22–30 percentage points if their independent estimates hold: about 1.28–1.43x, with 1.35x a reasonable midpoint. Do not multiply microbenchmark speedups or count overlapping savings twice.

## 1. Review of the existing designs

### Style: sound priority, insufficiently supported totals

The sheet registry is the best first change. It removes O(sheets × DOM nodes) discovery work. Ancestor indexing addresses the synthetic quadratic cases directly. Lazy declarations and CSSOM prototype accessors also remove identifiable work.

**Biggest error: signature correctness is already disproved by its own experiment.** Section 3.1 M3 reports 99.86% agreement, not equivalence. It then estimates cost using 28.6% distinct signatures, although the measured first-seen signatures account for 47.2% of candidates. Multiplying the element fraction by mean candidate cost loses that correlation. A corrected signature can also reduce sharing. No journey saving from M3 should enter the committed forecast yet.

A parent signature does not encode preceding-sibling state for `.a + .b` or `.a ~ .b`. Ancestor compounds can themselves contain sibling relations. Relational selectors, form state, language and shadow boundaries need revalidation or a conservative fallback. Resolve every observed mismatch before optimizing the signature. A random differential test is useful but cannot establish coverage of all selector dependencies.

Section 3.6 also mixes attribution: `sources_capped` is in style/cascade in the CPU report, but is included in the loader's list of removable work. The 10.7% loader estimate must exclude that style-owned saving. Build a disjoint function/subtree ledger before adding phase estimates.

Section 3.4's 12.8% is an entry-per-walk proxy on 12 journeys. It is not a text-rule lifetime hit rate. Neither the numerator nor denominator establishes the fraction of declaration parsing that can be skipped. The selector cost is explicitly guessed. A 5-point journey reduction is plausible, but remains hypothesis until selectors, boundaries and declarations are timed separately.

M2's ramp result is credible as an order-of-growth improvement. Its exact 12–19x forecast is hypothesis. Repeated ancestor hashes need deduplication; otherwise one entry can be emitted several times. Deep paths can turn path lookup into the next multiplier. Index only mandatory ancestor keys; retain the full matcher and source order. Measure the universal/rest bucket separately.

I would move cheap CSSOM ahead of signature sharing. Its memory failure is measured directly, and its observable API shape can be checked against browser behavior. Compact selector storage is also a better memory follow-up than a high-risk sharing key when rule-heavy pages dominate.

### JS: lazy functions first is right; leaf dispatch is not a ceiling

P1 removes unused ASTs and first walks. P2 removes quadratic concatenation. The VM spike gate is much better than assuming bytecode is fast. The heavy-page memory direction is sound.

**Biggest error: sections 2.2 and 6.1 treat a 3.4% sum of self costs as the scope of execution-model savings.** Changing frames, register allocation and scope access can remove work attributed to heap lookup, environment allocation, interning and call setup. The 2.6-point VM prediction is a scenario, not an upper bound. Conversely, applying 4.5x uniformly to the ramps' entire 57.7% JS share assumes regex, CSSOM objects and builtins accelerate like dispatch. They do not. Profile representative call subtrees and separate VM-removable costs from native builtin costs.

The memory forecast is less secure than the headline: section 2.3 has no log for the heaviest journey and assumes 90% dead AST there. Section 6.6's 72% unused source-byte fraction excludes the 5 MB rust-book data script. Node share does not predict parse time for large literals. Validate P1 on both data-heavy search journeys before claiming the 20 MiB saving.

Section 6.3 describes a “32 to 40 B record” with three 16 B inline values. That cannot include the values. The following row uses 40 + 48 = 88 B, which is coherent. Specify header bytes, slot bytes and allocator overhead separately before accepting the 12 MiB saving.

Section 6.6 says a persistent cache would make repeat visits “near free.” It can remove parsing/compilation, not execution, DOM creation or network effects. Also, there is already a thread-local cross-realm source-and-grammar parse cache (`crates/js/src/parse_cache.rs:1`, constants at :26). Measure its hits and eviction before inventing another layer.

Keep early errors in lazy functions and regex literals. Keep resource limits for source scanning and validation as well as materialized AST size. Laziness does not make adversarial input free. V8's preparser explicitly preserves syntax and scope information: [V8 preparsing](https://v8.dev/blog/preparser).

## 2. Third design, part A: demand-driven rendering

### What I would build

Separate current document state from computed style, geometry, display lists and raster pixels. Give each an invalidation epoch and an explicit “ensure” operation. A mutation marks work; an observation performs the required work once. Several mutations before one read should coalesce.

DOM selector lookup needs DOM state. A computed color read needs target style and inheritance dependencies. A width read needs used geometry. Driver text/visibility may need many elements. Pointer actionability needs geometry, clipping and occlusion. A screenshot needs all relevant rendering stages. Never make a DOM-only command silently promise rendered visibility.

Current code already distinguishes some reads: `browser_computed.rs:46–49` avoids layout for values that do not need it, and `:171–179` caches cascades. The new design must measure the remaining unnecessary work. `crates/drive/src/snap.rs:540`, :607 and :641 inspect a frame for whole-page snapshots/find/read. This contradicts treating all driver observations as single-element requests.

### Expected gain

Measured style + layout is 17.7 + 6.9 = 24.6%: even eliminating both gives only 1.33x. Adding all paint (4.7%) gives a 29.3-point ceiling, or 1.41x. Sheet discovery and parse savings belong to other proposals.

Hypothesis: avoiding 30–60% of style/layout and 50–100% of paint would save 9.7–19.5 baseline points before overlap and bookkeeping, about 1.11–1.24x. This is a sensitivity range, not a prediction from observed demand. After faster matching, savings shrink. If regular snapshots force broad style, the result could be near zero or a regression.

Memory: do not forecast a large median gain from sparse styles; style §3.2 already finds about 700 unique computed styles per journey. Hypothesis: avoiding fragment/display-list/pixel allocation helps some pages. Those retained bytes were not isolated here.

### Correctness boundaries

- “Only target and ancestors” is insufficient for layout. Preceding flow, floats, flex/grid siblings, intrinsic descendants, tables and scroll extents can affect a target. Compute a dependency closure; fall back to the formatting root or document when necessary.
- Visibility needs ancestor display/visibility, clipping and dimensions. `visibility: hidden` descendants can override visibility. Clicks also depend on unrelated overlapping elements and stacking order.
- `offsetWidth`, rect reads and some resolved computed-style values must see up-to-date geometry. [CSSOM View](https://www.w3.org/TR/cssom-view/#dom-htmlelement-offsetwidth) defines the observable result; it does not permit stale geometry in headless mode.
- `:has()`, sibling selectors, inherited custom properties, fonts and viewport changes can invalidate dependencies outside a local subtree. Container queries add layout-to-style dependencies where supported.
- Observers, animation events, requestAnimationFrame and resource load events are observable even without a screenshot. Audit supported APIs. Demand must include browser scheduling, not only driver requests.

Blink already skips style resolution in some display:none subtrees and can ensure style for an explicit query. Its source treats absent/ensured styles conservatively when layout may affect style: [StyleEngine::StyleMaybeAffectedByLayout](https://raw.githubusercontent.com/chromium/chromium/main/third_party/blink/renderer/core/css/style_engine.cc), and [Element computed-style handling](https://chromium.googlesource.com/chromium/src/+/500947f508e484696c809b5ea84568bfd51b320a/third_party/blink/renderer/core/dom/element.cc).

[CSS Containment Level 2](https://www.w3.org/TR/css-contain-2/) supplies containment and content-visibility rules that allow skipped rendering. These are dependency guarantees, not permission to ignore arbitrary unqueried elements. Do not inject containment into pages as an optimization. Modern [Chrome headless](https://developer.chrome.com/docs/automation-and-testing/headless/) shares its browser implementation with headful mode; headless does not imply layout-free execution.

## 3. Third design, part B: immutable reuse across runs

Yes, but first extend reuse within the existing driver/MCP process. Keep immutable source, parser metadata, regex programs and eventual bytecode apart from realm-owned values. Rebuild closures, globals and mutable CSSOM state for each document.

Use a content digest plus grammar goal, engine/cache-format version and semantic compile options as the artifact identity. URL is provenance and a fetch key, not proof of equal source. Verify response content before reuse. CSS also needs base URL/import context: prefer context-independent parsed tokens with per-document URL resolution, rather than caching flattened text under a content-only key.

Include HTTP validation, cache partitioning, CSP/CORS decisions, atomic writes, byte bounds, corruption recovery and deterministic cache-off runs. A source artifact must not confer permission to execute it. Do not cache realm handles, inline-cache receiver shapes or DOM node ids.

Measured JS + CSS parse is 15.5%. Even a free, universal cache hit saves at most 1.18x from those two categories. Hypothesis: at 60–90% instruction-weighted hit rate and 10–30% hit overhead, savings are 6.5–12.6 baseline points (1.07–1.14x), before lazy parsing. Actual hit rate on these journeys is unmeasured. Cold runs can regress. Cross-run regex/code-link reuse may add savings but needs separate attribution.

After lazy JS/CSS, their stated remaining parse shares total roughly 7–8 baseline points; that is the new parsing ceiling, not another 15.5 points. Persistence is not inherently a memory optimization: retained ASTs can increase RSS. Compact serialized artifacts or shared read-only mappings can help a worker fleet; measure private bytes and PSS.

[V8 code caching](https://v8.dev/blog/code-caching-for-devs) documents in-memory and serialized disk caches. Its benefit depends on repeated code and which functions were compiled. [Improved V8 code caching](https://v8.dev/blog/improved-code-caching) describes serialized compilation output, not saved page execution. A minimal bare cache can begin with function spans and validation metadata; it need not wait for a full VM.

## 4. Other architectural levers, ranked by gain per risk

| Rank | Lever | Gain for this workload; qualification | Risk / judgment |
|---|---|---|---|
| 1 | Separate rasterization from frame inspection | Paint is 4.7% overall: maximum 1.05x overall, but curl-se-home's 86.7% paint gives a 7.5x theoretical row ceiling. Hypothesis: substantial row gains if its paint is avoidable. | Low–medium. Keep canvas pixel reads, screenshots and action geometry correct. Trace paint callers first; do not assume headless currently paints every frame. |
| 2 | Warm process with fresh isolated document contexts | Existing driver `run.rs:106` explicitly shares HTTP cache/connections across opens; JS parse cache already shares across realms. Hypothesis: more reuse reduces suite CPU, TLS and repeated parse, but 2x across these journeys is unsupported. | Low–medium. Preserve cookie/storage isolation, close documents/tasks, bound caches. Reuse an existing process before proposing a daemon. |
| 3 | Explicit fixture/setup reuse across related tests | If setup fraction is p and N tests share it, speedup is 1 / (1-p+p/N). Hypothesis: p=0.7, N=10 gives 2.70x suite CPU. This is a credible path above 2x. No p measurement exists here. | Medium. Reset application/server state and auth deliberately. Report login coverage separately; sharing login setup cannot validate login on every test. |
| 4 | Immutable artifact cache plus lazy parse | 1.07–1.14x cache sensitivity before overlap; lazy JS also targets heavy-page AST memory. | Medium. Versioning and context separation are tractable. No blanket 2x claim. |
| 5 | Demand-driven style and layout | Sensitivity above: 1.11–1.24x before overlap, potentially little on broad snapshots. | Medium–high. Start with coalescing and conservative full-layout fallback. |
| 6 | Explicit resource policy: block analytics/ads; defer nonessential resources | Hypothesis: can exceed 2x on tracker-heavy pages only if removed direct and induced work exceeds half the original cost. These public journeys have no script-origin attribution proving that condition. | High semantic risk. Third-party auth, CAPTCHA, consent and tag-managed app code can be essential. Use per-test allow/block rules; do not disable third-party JS by default. |
| 7 | Independent journey workers | Hypothesis: bounded concurrency can exceed 2x suite throughput on this 16-core machine when tests are independent. It does not reduce summed instructions or memory. | Medium. Cap workers by peak memory; avoid shared login state and server rate limits. Parallel style alone has a 1.22x ideal ceiling from its 17.7% share. |
| 8 | Post-load page/process checkpoints | Hypothesis: repeating many actions from an identical local fixture could amortize most startup and exceed 2x. No generic public-login benefit is established. | Very high. A JS heap snapshot omits DOM, network, timers and server state. Whole-process restore retains stale sockets, clocks and credentials. Defer. |

Image decode is only 0.3% overall; removing it cannot change the aggregate much. Decode pixels on demand, but retain intrinsic image dimensions, completion/error events and JS decode behavior. Fonts affect geometry even with no pixels. Defer glyph rasterization, not necessary font metrics or shaping. Font-specific CPU/RSS was not isolated, so any larger font claim is hypothesis.

V8 [startup snapshots](https://v8.dev/blog/custom-startup-snapshots) amortize initialized JS contexts. The historical article explicitly distinguishes heap state from external interactions. It does not validate arbitrary post-login page checkpoints. Bare's measured realm creation is only 0.5% (JS §2.2), so builtin-context snapshots alone have a small CPU ceiling.

For median RSS, inspect resident text and feature reachability in a headless executable. Hypothesis: separating optional window/raster code can shrink the image. No link map or resident-page analysis here proves enough cold code to halve RSS. Stripping debug symbols alone does not remove executing text. For the 116 MiB tail, lazy ASTs and compact objects remain the best supported targets; a 2x reduction is unproved.

## 5. Recommended order and acceptance gates

1. Repair the measurement contract: fixed recorded inputs where possible; same passing journeys; explicit cold/warm state; separate CPU, ready time, peak RSS and PSS. Add actual login fixtures. Trace render triggers, cache hits and per-script-origin cost. Do not credit skipped/failed steps as performance wins.
2. Ship the sheet registry. In parallel planning, define separate style/geometry/raster observation APIs. Implement raster deferral once callers show avoidable paints. Preserve image metadata and font metrics.
3. Ship JS P1 and cheap CSSOM prototypes/lazy wrappers. Measure heavy search pages directly. Keep syntax/early-error behavior unchanged. Do not require the VM rewrite for prototype accessors.
4. Add ancestor indexing for the quadratic ramps, with deep-path and duplicate-key cases. Move lazy CSS declarations ahead of signature sharing. Measure declaration cost independently.
5. Exercise fresh contexts in a warm driver/MCP process, then add immutable persistent artifacts if measured misses justify them. Keep cache-off behavior as the correctness oracle.
6. Prototype target-style demand with full geometry fallback. Count the actual dependency closure and whole-page snapshot frequency before building partial layout. Re-measure after the earlier changes; do not add baseline percentages twice.
7. For gains above 2x, prototype explicit suite setup reuse and bounded independent workers. Offer resource suppression only as a named test configuration. Distinguish full-browser validation from tests with omitted resources/setup.
8. Follow measured tail costs: ropes, compact objects, then the gated VM. Reconsider signature sharing only after every known mismatch has an explanation and conservative revalidation exists. Defer general page checkpoints and NaN boxing.

Acceptance: differential DOM/CSSOM/geometry/actionability results; identical successful driver steps; three runs with spread; cold and warm reports; no false pass from blocked login resources. A VM gate also needs realistic application callbacks, not only loop/fib rows. Each phase must improve the intended whole-browser metric after rebasing.

## Not tested

No code changes, builds, benchmark reruns, browser sessions, real logins, cache prototype or checkpoint experiment. No new measurements. Source inspection and published primary sources support mechanisms; all proposed performance ranges and unverified workload explanations above are marked hypothesis. Only the requested report was created; nothing was committed or pushed.
