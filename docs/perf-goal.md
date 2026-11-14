# Performance goal (2026-11-13)

The owner set this goal after the benchmark round and the profiles of 2026-11-13. The method is in `CLAUDE.md`. The designs are in `design-style.md` and `design-js.md`. An independent review is in `design-review.md`.

## Goal

Build every step below. Show a visible improvement on the same benchmark.

The baseline is the frozen commit `f6415364`. Each step is measured on the newest main before merge, against that baseline, with the same inputs.

## What the profiles showed

- Per page, subsystem work has a ceiling. If style, layout and paint cost nothing, a journey gets at most about 1.4 times faster. A free parse cache for JS and CSS gives at most 1.18 times.
- Gains over 2 times come from the work of a whole suite: setup reuse across tests, a warm process, and parallel workers.
- Heavy pages can gain much more than the median page: the style ramps are quadratic now.

## Success criteria

All numbers use the recorded corpus from step 1, 3 runs each, against the baseline.

| # | Measure | Baseline | Target |
| --- | --- | --- | --- |
| 1 | The 48 public journeys | all pass | all pass with recorded inputs; test262 gate and `exec_diff` unchanged |
| 2 | Total instructions of the 48 journeys | 100% | 80% or lower |
| 3 | `compm_22000` and `rest_13` ramps | about 11.5 G instructions each | 10 times lower or more |
| 4 | Journeys with heavy sheet scans (`python-org-docs-link`, `rust-book-search`) | 100% | 1.3 times faster or more |
| 5 | Peak RSS of the 16 MiB sheet ramp with a `cssRules` read | 947 MiB | 300 MiB or lower |
| 6 | Peak RSS of `python-docs-search-results` | 116 MiB | 20% lower or more |
| 7 | A suite of related tests with a shared login setup | sequential cold runs | wall time 2 times faster or more, and the sum of instructions 1.5 times lower or more with one worker |
| 8 | Limits | restyle charge about 10 times its cost; `cssom-objects` cap 65,536 | the charge matches the cost; the cap is removed or set by the rule count |

A target that is not met is reported with its numbers. It is not hidden.

### Result (2026-11-14)

The final benchmark compared the baseline `f6415364` with main `8f23dded` after every step merged. Three criteria are met and one is half met. Criterion 1 is met: 47 of 47 journeys pass on replay, test262 shows no loss, and `exec_diff` shows 0 differences. Criterion 5 is met: 273.6 MiB for the 16 MiB sheet ramp (947 MiB before). Criterion 7 is met: the suite is 6.8 times faster with 4 workers and uses 2.10 times fewer instructions with one worker. Criterion 8 is half met: the restyle charge now matches its cost, but the `cssom-objects` cap is a fixed 524,288, not removed and not set by the rule count. Four are not met. Criterion 2: 85.3% of the baseline instructions (target 80%), or 89.5% when `mdn-reference` is left out (the last sentence of this paragraph says why). Criterion 3: the ramps are 6.4 and 8.8 times lower (target 10). Criterion 4: 1.19 and 1.02 times (target 1.3). Criterion 6: 99.3 MiB, 13% lower (target 20%); it was met at 90 MiB after PR #501 and rose again after PR #497. The benchmark also found that `mdn-reference` asks for 47 chunk files at a wrong address since PR #492, so its -73% is not a gain. The table, the numbers, the run counts and what was not tested are in `docs/progress.md`, section "Final benchmark of the perf goal (2026-11-14)".

## Baseline on the recorded corpus (2026-11-14)

Measured with the virtual page clock (`docs/design-virtual-clock.md`): `tools/bench/journeys.sh compare --clock virtual --runs 3 --pss`, corpus `virtual`, 47 journeys (the 48th, `nextjs-docs`, does not pass on replay). Same-binary spread is 0.7% or less on main, so these numbers are not noise. Baseline is `f6415364` plus the replay patch plus the clock. Main is `fe2d54cd` plus the clock. Per-journey table: `docs/progress.md`, section "A virtual page clock for replay mode".

| # | Measure | Baseline | Main | Against the target |
| --- | --- | --- | --- | --- |
| 1 | Journeys that pass with recorded inputs | 47 of 47 | 47 of 47 | met for the 47 that replay. test262 gate and `exec_diff` unchanged. |
| 2 | Total instructions | 105,217 M | 96,345 M (91.6%) | not met (target 80%) |
| 3 | `compm_22000`, `rest_13` ramps | not in this corpus | | |
| 4 | `python-org-docs-link`, `rust-book-search` | 1,438 M, 16,606 M | 1,419 M, 16,531 M (1.01 times, 1.00 times) | not met (target 1.3 times) |
| 5 | Sheet ramp RSS | not in this corpus | | |
| 6 | `python-docs-search-results` peak RSS and PSS | 113.5 MiB, 106.3 MiB | 113.7 MiB, 111.8 MiB | not met (target 20% lower) |

Other totals, medians summed: CPU 13,388 ms to 12,228 ms (-8.7%). PSS 1,724 MiB to 1,565 MiB (-9.2%). Peak RSS 2,056 MiB to 1,882 MiB (-8.5%). Wall time is from a loaded machine.

Use one clock for both sides. The virtual clock does more page work than the real clock on some journeys (the same binary: 66.6 G on the real clock, 96.3 G on the virtual clock), so a number from this table does not compare with a number from the real-clock corpus.

## Steps, in order

1. Benchmark contract. Record the network inputs of each journey once and replay them. Report cold and warm runs apart. Report instructions, time to ready, peak RSS and PSS apart. Add a local login fixture. Count render triggers, cache hits and cost per script origin.
2. Sheet registry (`design-style.md`, phase 1). Paint pixels only when a caller needs them (`design-review.md`, lever 1).
3. Lazy JS functions and lazy regex literals (`design-js.md`, P1). Cheap CSSOM objects: accessors on a shared prototype, rule objects made on demand (`design-style.md`, phase 5).
4. Ancestor-keyed rule index and a restyle charge that counts real work (`design-style.md`, phase 2). Lazy CSS rule bodies and a faster tokenizer (`design-style.md`, phase 4).
5. Fresh documents in a warm driver or MCP process. Then a disk cache of immutable parse results, only if the measured misses justify it.
6. Style on demand, with a fallback to full layout. Count the real dependency closure first.
7. Setup reuse across related tests, and bounded parallel workers.
8. Tail costs: strings (`design-js.md`, P2), compact objects (P5), then the register VM behind its spike gate (P3).

## Deferred

- Signature sharing (`design-style.md`, phase 3): the estimate gave wrong matched lists for 0.14% of elements. Build it only after every mismatch has a cause and a revalidation path.
- Page and process checkpoints, the 8-byte `Value`, and blocking of third-party scripts by default. Resource blocking is allowed only as a named setting per test.
