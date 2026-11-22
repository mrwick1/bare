# Next work after the final engine benchmark (2026-11-23)

Source: [final benchmark](progress/2026-11-23-final-engine-benchmark.md) (#957) on frozen main `f3b30305`. Earlier sources: [benchmark rerun](progress/2026-11-22-engine-benchmark-rerun.md) (#923) and [browser rerun](progress/2026-11-22-browser-rerun-after-fixes.md) (#942).
The owner's session keeps this list current. Each item gives the problem, the numbers and the next step.
Work is paused on 2026-11-23 at this checkpoint. No agents run.

## State

- Engine alone, 16 programs: js2 uses 2.640 G instructions. QuickJS direct uses 2.895 G. QuickJS CLI uses 3.275 G. The old engine uses 8.404 G. These equal #923.
- Engine alone, peak RSS sum: js2 106.73 MiB, QuickJS direct 66.99 MiB, old engine 147.18 MiB.
- Browser, OLD `auto` to js2, clean rows only (both engines pass, same reply count):
  - Public, 31 of 44 rows: 1.01x instructions, 0.88x RSS, median 0.98x, js2 wins 18.
  - Public without MDN, 30 rows: 0.92x instructions, 0.92x RSS.
  - Ramps, 26 of 27 rows: 0.80x instructions, 0.95x RSS, js2 wins 22.
  - React typing: 0.85x instructions, 0.78x RSS.
- The #942 public total (0.83x) was too low. In #942, js2 stopped `react-docs` early on `Unsupported("clone_out")`. With the full rule of #942, public reads 1.06x now. `react-docs` and `substack-home` cause all of the rise.
- Since #942: structured clone and typed-array index reads (#944, #946), UTF-8 codecs (#948), the crates-io binding fix (#950), round 2 of real-page fixes (#943, #945, #947, #949, #953, #954), the shared Intl crate (#952), packed Intl tables (#955) and js2 Intl bindings (#956).

## Items, in order of impact

1. **Real pages.** The median public journey is 1.01x. js2 wins 18 of 43 journeys. The engine alone is about 3x faster than OLD.
   Hypothesis: the cost is outside the bytecode loop, for example DOM calls through the engine traits.
   Worst journeys (M instructions, OLD to js2):
   | Journey                                                                                                                      |   Instructions | Ratio |
   | ---------------------------------------------------------------------------------------------------------------------------- | -------------: | ----: |
   | rust-book-search                                                                                                             | 2,369 to 4,261 | 1.80x |
   | devto-home                                                                                                                   | 4,974 to 6,110 | 1.23x |
   | python-org-search                                                                                                            | 1,246 to 1,494 | 1.20x |
   | rust-lang-org-home                                                                                                           |      87 to 100 | 1.15x |
   | Next step: profile the whole browser on these journeys and split the cost by subsystem (CLAUDE.md, "How to improve a part"). |
   Done 2026-11-22. [The profile](design-js2-real-pages.md) (#927) found the causes. Fixes: #930 and #940 (`for...in` setup), #933 and #938 (regexp setup), #934 (JSON.parse keys), #935 (no OLD speculative parse in js2 sessions), #936 (`RegExp.test`), #937 (substring range).
   [Remeasure](progress/2026-11-22-real-pages-after-fixes.md) on `9a187630` (#939): rust-book-search 1.30x, python-org-search 1.02x, rust-lang-org-home 1.05x, devto-home 1.03x on the tape. #940 then cut rust-book-search to 1,866 M against OLD 2,370 M (0.79x).
   Done 2026-11-22: #942 reran the full cohort. Round 2 (#943 to #954) fixed the next losers. #957 is the current state.
   New worst clean rows (#957, js2/OLD instructions): `mdn-reference` 1.73x (OLD samples span 2,674 to 5,809 M, so the OLD minimum can be a short run), `crates-io-home` 1.55x (confirmed), `nodejs-org-home` 1.05x, `python-org-docs-link` 1.02x.
   Next step: profile `react-docs` and `substack-home` (item 10), `crates-io-home` and `mdn-reference` on tapes.
2. **React typing.** js2 uses 7.66 G and OLD uses 6.38 G (1.20x). The decision benchmark gave OLD 14.4 G on the same frozen OLD commit.
   Done 2026-11-22. The 14.4 G count was OLD `tree` mode (#926). #930 reads `for...in` keys from the shape: js2 5.71 G, 0.896x OLD `auto`. After #940: 5.68 G.
3. **Startup memory.** An empty js2 process uses 5.6 MiB. QuickJS uses 3.0 MiB. This base is most of the per-program RSS gap.
   Done 2026-11-22. #925 defers free-list writes: 5,776 to 5,452 KiB. #929 orders the code: empty js2 5,528 to 4,300 KiB, empty bare 19,188 to 8,744 KiB.
   Note: the code order applies only to builds made with `tools/code-order/build.sh` and the Linux release workflow. A plain `cargo build --release` does not use it.
4. **Remaining QuickJS CPU gap.** js2/QuickJS: loop 1.18x, webpack 1.17x, objects 1.09x, calls 1.06x.
   The next global-access fusion step exceeds the 128 KiB code-page budget and slows 4 of 5 micro rows. It needs a new design.
   Designs: [loop](design-js2-qjs-gap-loop.md), [calls](design-js2-qjs-gap-calls.md), [objects](design-js2-qjs-gap-objects.md), [webpack](design-js2-qjs-gap-webpack.md).
5. **Main housekeeping (owner session).**
   - Run the full cost matrix on main. About 15 PRs merged since the last full run.
   - Add the debt rows: `interp_load fib 27` +0.89% at codegen-units=1 (#921). `interp_load calls` +0.53% (#922).
   - Write the test262 js2 baseline on main.
   - Done 2026-11-22 (#928): matrix 821 of 823 rows, test262 js2 29,217 and OLD 27,748 with no lost passes. #931 fixes `session_load chain 1`. The owner accepted `interp_load calls` (+0.55%) and `raw_collect` (+1.39%) as debt.
6. **Journeys that fail on both engines.** `wikipedia-main-random` and `churn_1m` fail their assertions with OLD and js2.
7. **Time to ready.** The rerun ran on a shared machine (load 0.09 to 5.14). Measure ready time again on an idle machine.
8. **Phase 3.** Make js2 the default engine. Test it on the Windows VM.
9. **js2 session example.** The `session_load` example fails with `BARE_JS_EXEC=new` on an old-engine dereference (#931). The virtual session test covers the js2 session path. Fix it before js2 becomes the default.
10. **New js2 losers after the clone fix.** `react-docs` now runs to the end on js2. js2 uses 1.74x the OLD instructions (2.32x in the recheck), and the reply counts differ. `substack-home` rose from 1,657 M to 6,992 M on js2. js2 now gets 142 replies against 22 for OLD, so js2 loads more of the page. Hypothesis: the clone and Intl work let js2 run code that it skipped before. Next step: profile both journeys by subsystem.
11. **Reply-count differences.** 11 public journeys give different reply counts on OLD and js2: `blog-rust-lang-home`, `hn-new-past`, `paulgraham-home`, `python-docs-search-results`, `python-org-docs-link`, `react-docs`, `rust-book-chapters`, `rust-book-sidebar`, `sqlite-org-home`, `substack-home`, `wikipedia-article`. Next step: record tapes and compare the requests, to separate live-site noise from engine differences.
12. **Two-level noise.** `craigslist-home`, `devto-home`, `sqlite-org-home` and `wikipedia-toc-languages` give samples at two levels. `go-dev-home` spreads from 989 M to 5,919 M. `github-docs-home` gave HTTP 429. Next step: measure these rows on tapes only.
