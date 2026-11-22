# Next work after the engine benchmark rerun (2026-11-22)

Source: [benchmark rerun](progress/2026-11-22-engine-benchmark-rerun.md) on frozen main `3c46337e`, compared with [the decision benchmark](progress/2026-11-21-engine-decision-benchmark.md).
The owner's session keeps this list current. Each item gives the problem, the numbers and the next step.

## State

- Engine alone, 16 programs: js2 uses 2.64 G instructions. QuickJS direct uses 2.90 G. The old engine uses 8.40 G.
- Engine alone, peak RSS geomean: js2 6.49 MiB, QuickJS 3.80 MiB.
- Browser, OLD `auto` to js2: public journeys 0.98x instructions (0.92x without MDN), ramps 0.80x, React typing 1.20x.

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
   Next step: run the full public cohort and the ramps again for the new median and the new worst journeys.
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
