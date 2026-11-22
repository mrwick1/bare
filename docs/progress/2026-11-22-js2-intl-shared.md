# 2026-11-22 — Share Intl locale data and formatters

Move the locale tables, ICU data, date patterns, time zone names, decimal conversion, plural rules, display name lookup, and relative time patterns to `bare-intl`. Keep the OLD module paths. The engines own JavaScript values, property access, coercion, and errors. NumberFormat shares option validation and retains the OLD order of property reads.

## OLD checks

Run on archMachine. The 43 OLD Intl tests and six ICU tests pass. The `bare-js2` and `bare` typecheck passes. The OLD test262 runner gives the same outcome for each of the 3,393 `intl402` cases. Both builds pass 807 and fail 2,586. The baseline runner uses `cad56767`. The OLD engine and its test runner have identical source in the browser comparison base `60188a32`.

The two release `bare-drive` builds use the same settings. File size goes from 24,485,208 to 24,495,912 bytes (+0.044%).

Run the 20 `intl_load` rows three times per tree with perf and both allocators. The largest instruction increase is on `nf_format 1`, at +0.204% on glibc and +0.178% on jemalloc. The four `nf_locale` and `nf_locale_opts` rows are within the limit after reuse of the OLD property readers. No row exceeds +0.5%.

## Node.js replay

Both OLD builds pass three instruction runs and three separate peak RSS runs on the same tape. Each report has 23 replies. The table gives instruction minima and peak RSS medians, with min–max ranges. An OLD live run produced the tape. Replay reports missing background requests for the same resources in both builds.

| Measure | Baseline | Extraction |
| --- | ---: | ---: |
| Instructions, M | 3,718.480 [3,718.480–3,720.949] | 3,718.273 [3,718.273–3,718.543] |
| Peak RSS, KiB | 92,588 [92,000–92,928] | 92,932 [91,504–96,064] |

Raw logs are under `/data/bare-tmp/r22/js2-intl` on archMachine. Local checks omit Windows, Miri, and the full workspace suite.
