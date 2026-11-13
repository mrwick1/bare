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
