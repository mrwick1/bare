# 2026-11-23 — Final engine benchmark at f3b30305

This report measures the three JS engines on frozen main `f3b30305` (merge of #956). It repeats the method of [#923](2026-11-22-engine-benchmark-rerun.md) for the standalone engine rows and the method of [#942](2026-11-22-browser-rerun-after-fixes.md) for the browser journeys. It changes no engine code.

## Summary

- Standalone rows: the totals for all four engines are the same as #923 at the shown precision. js2 used 2.640 G instructions on the 16 cases. OLD used 8.404 G, the QuickJS CLI 3.275 G and the QuickJS direct launcher 2.895 G. QuickJS still used the least peak RSS.
- Journeys: js2 used 0.80× the OLD instructions on the 26 ramps and 0.85× on typing. On the 30 clean public rows without MDN, js2 used 0.92×. On the same 30 rows, #942 gave 1.02×.
- 14 journey rows are not in the totals. Two failed in both engines. One hit HTTP 429. Eleven had different reply counts in the two engines. "Excluded rows" lists them.
- The #942 `react-docs` ratio of 0.11× was an artifact. In #942, js2 stopped the page on `Unsupported("clone_out")`. Now js2 logs no diagnostic and uses 1.74× the OLD instructions (2.32× in the recheck).

## Machine and inputs

| Item | Value |
|---|---|
| Machine | archMachine, AMD Ryzen 9 5900HX, 8 cores and 16 threads, Linux `7.2.15-zen1-1-zen` |
| Tools | Rust 1.96.0, GCC 16.2.1, perf 7.3.1-1 |
| Commit | `f3b30305`, a detached worktree. OLD (`crates/js`) and js2 (`crates/js2`) come from the same tree. |
| Engine build | `cargo build --release --locked`, `CARGO_BUILD_JOBS=3`, examples `eval_load` and `decision_old` (bare-js) and `decision_js2` (bare-js2). The two runner files are copies of the #923 files. |
| Browser build | `RUSTFLAGS='-C force-frame-pointers=yes' cargo build --locked --profile profiling -p bare-drive --bin bare-drive -p bare --example type_load`, `CARGO_BUILD_JOBS=3`, as in #942 |
| QuickJS | Bellard 2026-06-04, `-O2`, `CONFIG_LTO=y`. These are the same `qjs`, `qjs-direct` and `qjs-compile` binaries that #923 built. |
| Inputs | OLD `eval_load CASE SCALE --source` generated the scripts again. The case names, expected values and SHA-256 of all 17 scripts match #923. The React bundle SHA-256 is `1992c720…07400`. The public step tree is `36a00342…`, the same as #923 and #942. All 32 fixture files pass the #942 SHA-256 manifest. |

Load average (one minute):

```text
part1 start  03:42:56 load average: 4.85, 3.51, 1.72
part1 end    03:43:07 load average: 4.26, 3.42, 1.71
before public and typing 03:43:07 load average: 4.26, 3.42, 1.71
after public and typing  04:02:47 load average: 0.56, 0.52, 0.79
after ramps and controls 04:04:01 load average: 1.21, 0.73, 0.85
before recheck 04:04:32 load average: 0.95, 0.71, 0.84
after recheck  04:09:30 load average: 0.38, 0.51, 0.71
```

The one-minute load before each journey sample was 0.08–4.26. The engine rows ran at load 4.3–4.9, just after the build. This report uses instruction counts for CPU. It gives no wall times for the engine rows. Treat journey ready times as observations only.

## Method

### Part 1: engine rows

The harness is a copy of the #923 `micro/measure.py`. It has two changes. It runs the QuickJS direct launcher in the same loop as the other engines. It leaves out Node and OLD `eval_load`, which this task does not cover.

- Each case runs three times under `perf stat -e instructions:u` and three times under the static `wait4` RSS wrapper. The order of the engines rotates per trial.
- The result is the minimum instruction count and the median peak RSS. Each RSS run uses a fresh `cp --reflink=never` copy of the binary.
- Each run checks the completion value against the value from `eval_load`. All 432 samples passed.
- Environment: the #923 harness removes all `BARE_*`, `GLIBC_*` and `LD_*` variables and sets `LC_ALL=C`. This run does the same, so the engine rows run without `BARE_SEED` and without the tcache tunable. The task brief named `BARE_SEED=1` and `GLIBC_TUNABLES=glibc.malloc.tcache_count=65535`. This run follows the #923 code to keep the numbers comparable. The browser part uses both variables, as #942 did.
- The 16-case totals leave out `empty_1` and the React bundle, as in #923.

### Part 2: browser journeys

The harness is a copy of the #942 harness (`measure.py`, `run.py`, `recheck.py`, fixtures, typing page, triage lists). Only the source and binary paths changed.

- Both engines use the same binary. `BARE_JS_EXEC=auto` selects OLD. `BARE_JS_EXEC=new` selects js2. Both use `BARE_SEED=1` and `LC_ALL=C`.
- Instructions: the minimum of three passing `perf stat -e instructions:u` samples with at least 99% counter coverage and `GLIBC_TUNABLES=glibc.malloc.tcache_count=65535`.
- Peak RSS: the median of three separate passing samples with the default allocator, a fresh `cp --reflink=never` copy and `/data/claude-tmp/qjs-base/h/rss`.
- Replies: the reply count of the minimum instruction sample.
- Rows: the 44 public journeys of #923 and #942, typing, 27 ramps and 5 controls.
- Exclusions (new in this report): a row stays out of the totals if either engine failed to give three samples per metric, or if the two minimum samples have different reply counts. #942 kept rows with different reply counts in its totals. To compare like with like, each totals table also gives #942 on the same rows.
- Recheck: 13 public rows had a js2/OLD instruction ratio that moved by more than 10% against #942. Each of these rows ran once more with three new samples per metric and engine (`recheck2.py`). The totals use the main sweep. The recheck values are in the tables.

## A. Engine rows

### Totals over the 16 cases

| Engine | Instructions total, G | #923, G | Instructions geomean, M | #923, M | RSS sum, MiB | #923, MiB | RSS geomean, MiB | #923, MiB |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| OLD matched | 8.404 | 8.404 | 102.11 | 102.37 | 147.18 | 152.19 | 7.79 | 8.12 |
| js2 | 2.640 | 2.640 | 38.90 | 39.03 | 106.73 | 105.51 | 6.57 | 6.49 |
| QuickJS CLI | 3.275 | 3.275 | 51.17 | 51.21 | 69.68 | 69.47 | 3.99 | 3.97 |
| QuickJS direct | 2.895 | 2.895 | 46.85 | 46.89 | 66.99 | 66.88 | 3.81 | 3.80 |

js2 used fewer instructions than OLD on all 16 cases. Against QuickJS direct, js2 used fewer instructions on 8 of 16 cases: `throws`, `closures`, `fib_1`, `fib_10` and `concat` at both scales. QuickJS used less peak RSS on 15 of 16 cases. js2 used less on `closures_10` (10.22 against 13.55 MiB).

### Instructions, millions

Each cell gives the minimum, the min–max range when it is visible, and the #923 minimum in parentheses.

| Case | OLD | js2 | QuickJS CLI | QuickJS direct |
|---|---:|---:|---:|---:|
| `empty_1` | 2.05 (2.09) | 0.70 [0.70–0.71] (0.72) | 1.22 (1.23) | 1.05 (1.05) |
| `loop_1` | 226.97 (227.01) | 75.37 (75.38) | 64.68 (64.69) | 64.50 [64.50–64.51] (64.51) |
| `loop_10` | 2,341.52 (2,341.56) | 746.77 (746.78) | 635.29 (635.30) | 635.11 (635.12) |
| `calls_1` | 159.84 (159.88) | 63.90 (63.91) | 60.62 (60.62) | 60.44 (60.44) |
| `calls_10` | 1,584.75 (1,584.79) | 630.90 (630.91) | 594.32 (594.33) | 594.14 (594.15) |
| `objects_1` | 195.06 [195.06–195.08] (195.11) | 59.74 (59.68) | 53.37 (53.38) | 52.97 (52.98) |
| `objects_10` | 1,909.22 [1,909.22–1,909.66] (1,909.39) | 551.66 [551.66–553.14] (551.20) | 509.04 (509.04) | 507.63 (507.64) |
| `throws_1` | 128.08 (128.10) | 27.58 (27.59) | 88.48 (88.49) | 55.10 [55.10–55.12] (55.13) |
| `throws_10` | 1,279.26 (1,279.10) | 268.80 (268.81) | 874.13 [874.13–874.46] (874.13) | 535.81 [535.81–536.41] (535.82) |
| `closures_1` | 43.14 (43.14) | 12.64 (12.65) | 26.06 [26.06–26.08] (26.06) | 25.92 [25.92–25.96] (25.92) |
| `closures_10` | 412.82 [412.82–412.83] (412.56) | 113.25 (113.25) | 279.40 [279.40–279.60] (279.40) | 277.47 [277.47–277.92] (277.48) |
| `fib_1` | 7.06 (7.10) | 3.74 (3.75) | 4.36 (4.37) | 4.18 (4.19) |
| `fib_10` | 56.84 (56.88) | 33.52 (33.53) | 35.54 (35.55) | 35.36 (35.37) |
| `webpack_1` | 6.66 (6.70) | 5.98 (6.02) | 5.40 (5.41) | 4.98 (4.99) |
| `webpack_10` | 47.15 [47.15–47.16] (47.22) | 44.49 [44.49–44.50] (44.72) | 40.77 (40.78) | 38.21 [38.21–38.22] (38.21) |
| `concat_1` | 2.25 (2.29) | 0.82 (0.84) | 1.33 (1.34) | 1.16 (1.16) |
| `concat_10` | 3.37 (3.40) | 1.27 (1.29) | 2.15 (2.15) | 1.97 (1.97) |

### Peak RSS, MiB

Each cell gives the median, the min–max range, and the #923 median in parentheses.

| Case | OLD | js2 | QuickJS CLI | QuickJS direct |
|---|---:|---:|---:|---:|
| `empty_1` | 5.97 [5.88–5.99] (6.19) | 5.95 [5.94–5.96] (5.60) | 3.36 [3.32–3.42] (3.39) | 3.26 [3.04–3.32] (3.12) |
| `loop_1` | 5.90 [5.75–5.90] (6.12) | 5.79 [5.60–5.80] (5.67) | 3.37 [3.33–3.37] (3.41) | 3.20 [3.04–3.21] (3.20) |
| `loop_10` | 5.81 [5.77–5.96] (6.25) | 5.67 [5.59–5.92] (5.53) | 3.32 [3.21–3.34] (3.36) | 3.18 [3.08–3.21] (3.04) |
| `calls_1` | 5.95 [5.53–6.11] (6.07) | 5.70 [5.68–5.97] (5.88) | 3.32 [3.24–3.32] (3.25) | 3.22 [3.14–3.22] (3.18) |
| `calls_10` | 5.96 [5.88–5.98] (6.19) | 5.89 [5.86–5.96] (5.75) | 3.33 [3.22–3.40] (3.25) | 3.22 [3.09–3.25] (3.12) |
| `objects_1` | 6.42 [6.33–6.48] (6.74) | 6.74 [6.56–6.76] (6.73) | 3.76 [3.75–3.89] (3.73) | 3.59 [3.58–3.71] (3.59) |
| `objects_10` | 9.16 [8.91–9.20] (9.50) | 8.81 [8.78–8.89] (8.90) | 6.43 [6.39–6.48] (6.49) | 6.27 [6.23–6.42] (6.46) |
| `throws_1` | 8.09 [7.97–8.10] (8.52) | 6.27 [6.05–6.30] (6.09) | 3.44 [3.36–3.54] (3.46) | 3.23 [3.18–3.29] (3.27) |
| `throws_10` | 31.98 [31.88–32.12] (32.32) | 7.67 [7.59–7.88] (7.42) | 3.39 [3.30–3.39] (3.38) | 3.25 [3.09–3.36] (3.20) |
| `closures_1` | 7.50 [7.38–7.62] (7.76) | 6.26 [6.25–6.54] (6.25) | 4.52 [4.32–4.54] (4.43) | 4.21 [4.12–4.26] (4.23) |
| `closures_10` | 22.96 [22.88–23.20] (23.42) | 10.22 [10.13–10.52] (10.26) | 13.63 [13.57–13.63] (13.58) | 13.55 [13.44–13.61] (13.50) |
| `fib_1` | 5.99 [5.70–6.11] (6.24) | 5.95 [5.68–5.98] (5.72) | 3.41 [3.40–3.45] (3.42) | 3.24 [3.22–3.30] (3.21) |
| `fib_10` | 5.98 [5.88–5.99] (6.25) | 5.90 [5.52–5.98] (5.72) | 3.40 [3.18–3.43] (3.29) | 3.22 [3.05–3.31] (3.19) |
| `webpack_1` | 6.06 [6.04–6.22] (6.39) | 6.30 [6.03–6.34] (6.35) | 3.50 [3.49–3.54] (3.50) | 3.30 [3.27–3.32] (3.32) |
| `webpack_10` | 7.52 [7.31–7.56] (8.01) | 7.66 [7.44–7.72] (7.65) | 4.11 [4.06–4.14] (4.13) | 3.95 [3.88–3.98] (3.94) |
| `concat_1` | 6.00 [5.79–6.09] (6.18) | 5.98 [5.90–6.04] (5.82) | 3.37 [3.26–3.43] (3.40) | 3.21 [3.19–3.31] (3.23) |
| `concat_10` | 5.91 [5.72–5.95] (6.24) | 5.92 [5.65–5.94] (5.77) | 3.38 [3.21–3.38] (3.38) | 3.15 [2.98–3.22] (3.23) |

### React bundle front end

| Engine and work | Instructions, M | #923, M | Peak RSS, MiB | #923, MiB |
|---|---:|---:|---:|---:|
| OLD full parse | 69.64 [69.64–69.65] | 69.67 | 7.39 [7.30–7.40] | 7.15 |
| js2 full parse | 63.51 [63.51–63.52] | 63.49 | 7.27 [7.12–7.31] | 7.16 |
| js2 eager compile | 281.91 [281.91–281.93] | 281.83 | 8.16 [7.96–8.23] | 7.99 |
| QuickJS compile only | 247.31 [247.31–247.35] | 247.32 | 6.80 [6.71–6.86] | 6.79 |

## B. Browser journeys

Each cell gives OLD `auto` → js2 `new` and the js2/OLD ratio. "#942 same rows" uses the #942 values for the same rows. A win means js2 used less.

### Totals over clean rows

| Group | Rows | Instructions now, G | #942 same rows, G | Peak RSS now, MiB | #942 same rows, MiB |
|---|---:|---:|---:|---:|---:|
| Public | 31/44 | 24.40 → 24.66 (1.01×) | 24.14 → 26.38 (1.09×) | 1,251.22 → 1,095.19 (0.88×) | 1,274.72 → 1,074.26 (0.84×) |
| Public without MDN | 30/43 | 21.73 → 20.04 (0.92×) | 21.23 → 21.58 (1.02×) | 1,117.35 → 1,023.97 (0.92×) | 1,140.59 → 1,007.11 (0.88×) |
| Ramps | 26/27 | 39.89 → 31.77 (0.80×) | 39.89 → 31.77 (0.80×) | 1,004.58 → 957.82 (0.95×) | 1,024.02 → 952.25 (0.93×) |
| Typing | 1/1 | 6.39 → 5.41 (0.85×) | 6.38 → 5.68 (0.89×) | 38.41 → 29.94 (0.78×) | 38.51 → 30.10 (0.78×) |
| Other controls | 5/5 | 2.05 → 1.56 (0.76×) | 2.05 → 1.56 (0.76×) | 101.80 → 104.57 (1.03×) | 104.88 → 102.95 (0.98×) |
| All clean rows | 63/77 | 72.74 → 63.40 (0.87×) | 72.46 → 65.38 (0.90×) | 2,396.01 → 2,187.51 (0.91×) | 2,442.14 → 2,159.57 (0.88×) |

| Group | Median CPU ratio / wins now | #942 same rows | Median RSS ratio / wins now | #942 same rows |
|---|---:|---:|---:|---:|
| Public | 0.98× / 18/31 | 0.99× / 17/31 | 0.98× / 24/31 | 0.96× / 27/31 |
| Public without MDN | 0.98× / 18/30 | 0.99× / 17/30 | 0.98× / 23/30 | 0.96× / 26/30 |
| Ramps | 0.99× / 22/26 | 0.99× / 22/26 | 1.00× / 14/26 | 0.96× / 23/26 |
| Typing | 0.85× / 1/1 | 0.89× / 1/1 | 0.78× / 1/1 | 0.78× / 1/1 |
| Other controls | 1.24× / 1/5 | 1.24× / 1/5 | 1.03× / 0/5 | 0.98× / 3/5 |
| All clean rows | 0.99× / 42/63 | 1.00× / 41/63 | 0.99× / 39/63 | 0.96× / 54/63 |

### Totals with the #942 rule

These totals keep every row with three samples per metric in both engines, also rows with different reply counts. The #942 column gives the published #942 totals.

| Group | Rows now / #942 | Instructions now, G | Instructions #942, G | Peak RSS now, MiB | Peak RSS #942, MiB |
|---|---:|---:|---:|---:|---:|
| Public | 42 / 43 | 41.26 → 43.63 (1.06×) | 43.24 → 36.00 (0.83×) | 1,843.19 → 1,626.15 (0.88×) | 1,935.61 → 1,563.27 (0.81×) |
| Public without MDN | 41 / 42 | 38.59 → 39.01 (1.01×) | 40.33 → 31.20 (0.77×) | 1,709.32 → 1,554.93 (0.91×) | 1,801.47 → 1,496.12 (0.83×) |
| Ramps | 26 / 26 | 39.89 → 31.77 (0.80×) | 39.89 → 31.77 (0.80×) | 1,004.58 → 957.82 (0.95×) | 1,024.02 → 952.25 (0.93×) |
| Typing | 1 / 1 | 6.39 → 5.41 (0.85×) | 6.38 → 5.68 (0.89×) | 38.41 → 29.94 (0.78×) | 38.51 → 30.10 (0.78×) |
| Other controls | 5 / 5 | 2.05 → 1.56 (0.76×) | 2.05 → 1.56 (0.76×) | 101.80 → 104.57 (1.03×) | 104.88 → 102.95 (0.98×) |
| All complete rows | 74 / 75 | 89.60 → 82.37 (0.92×) | 91.56 → 75.00 (0.82×) | 2,987.98 → 2,718.47 (0.91×) | 3,103.02 → 2,648.58 (0.85×) |

Under the #942 rule, the public instruction ratio rose from 0.83× to 1.06×. Two rows cause more than all of this rise. On `react-docs`, js2 went from 597 to 5,921 M. On `substack-home`, js2 went from 1,657 to 6,992 M. Both rows have different reply counts in this sweep. The clean-row tables leave them out.

### Rows

Each cell gives OLD `auto` / js2 `new`. Brackets give the min–max range at the shown precision. The ratio is js2 instructions divided by OLD instructions. "recheck" gives the ratio from the second run of a row that moved by more than 10%.

### Public journeys

| Workload | Instructions, M | Peak RSS, MiB | Replies | Ratio now | Ratio #942 | Note |
|---|---:|---:|---:|---:|---:|---|
| `archlinux-org-home` | 79.7 [79.7–81.1] / 77.3 [77.3–77.9] | 22.9 [22.9–23.4] / 22.8 [22.5–23.2] | 10 / 10 | 0.97× | 0.97× |  |
| `blog-rust-lang-home` | 153.9 [153.9–156.4] / 156.7 [156.7–159.2] | 24.6 [24.1–24.8] / 24.1 [23.8–24.5] | 9 / 13 | 1.02× | 1.01× | replies differ, excluded |
| `cnn-lite-front` | 71.6 [71.6–71.8] / 72.6 [72.6–73.0] | 23.2 [23.2–24.0] / 23.8 [23.6–24.0] | 1 / 1 | 1.01× | 1.02× |  |
| `craigslist-home` | 2,353.2 [2,353.2–2,353.6] / 474.7 [474.7–1,967.3] | 93.2 [30.6–94.2] / 57.8 [57.6–57.9] | 3 / 3 | 0.20× | 5.38× | recheck 0.84× |
| `crates-io-home` | 276.7 [276.7–278.2] / 434.4 [434.4–555.2] | 31.5 [31.3–31.8] / 32.5 [32.3–32.6] | 39 / 39 | 1.57× | 1.12× | recheck 1.55× |
| `debian-org-home` | 114.7 [114.7–122.9] / 115.6 [115.6–119.8] | 23.0 [23.0–23.6] / 22.7 [22.5–22.7] | 15 / 15 | 1.01× | 1.00× |  |
| `devto-home` | 4,955.1 [4,955.1–5,897.9] / 5,775.7 [5,775.7–9,309.4] | 97.6 [96.0–100.5] / 93.5 [92.5–97.9] | 1 / 1 | 1.17× | 0.97× | recheck 1.05× |
| `docs-rs-home` | 69.6 [69.6–69.8] / 68.2 [68.2–68.6] | 23.5 [22.7–23.6] / 23.6 [23.3–23.7] | 10 / 10 | 0.98× | 0.98× |  |
| `example-learn-more` | 134.4 [134.4–136.5] / 128.9 [128.9–129.1] | 24.7 [24.6–25.1] / 24.6 [24.2–25.0] | 4 / 4 | 0.96× | 0.95× |  |
| `example-page` | 27.4 [27.4–27.5] / 26.6 | 21.6 [21.4–22.3] / 21.8 [21.6–21.9] | 2 / 2 | 0.97× | 0.97× |  |
| `github-docs-home` | 2,037.7 [2,037.7–2,072.5] / 1,908.3 [1,908.3–1,911.7] | 82.3 [74.9–89.6] / 71.0 [70.8–71.2] | 26 / 24 | 0.94× | 0.70× | error, excluded; recheck 0.95× |
| `go-dev-home` | 1,736.5 [1,736.5–3,815.8] / 1,314.2 [1,314.2–5,919.3] | 52.7 [52.7–52.9] / 40.5 [40.1–40.6] | 14 / 14 | 0.76× | 0.56× | recheck 0.53× |
| `hn-comments-back` | 157.2 [157.2–168.7] / 153.9 [153.9–154.8] | 22.8 [22.7–22.9] / 22.0 [21.8–22.1] | 6 / 6 | 0.98× | 0.99× |  |
| `hn-front` | 208.3 [208.3–208.5] / 206.2 [206.2–207.0] | 22.7 [22.6–22.8] / 22.7 [22.6–23.0] | 3 / 3 | 0.99× | 1.00× |  |
| `hn-new-past` | 205.3 [205.3–205.9] / 186.7 [186.7–209.5] | 22.6 [22.2–22.8] / 22.4 [22.4–22.7] | 6 / 5 | 0.91× | 0.99× | replies differ, excluded |
| `httpbin-cookies` | 36.7 [36.7–36.8] / 33.5 [33.5–33.7] | 21.1 [20.8–21.2] / 20.8 [20.8–21.2] | 4 / 4 | 0.91× | 0.91× |  |
| `httpbin-form-fill` | 46.2 / 45.2 | 22.2 [21.9–22.4] / 21.8 [21.7–22.0] | 1 / 1 | 0.98× | 0.98× |  |
| `httpbin-form-submit` | 58.5 [58.5–58.7] / 63.8 [63.8–64.3] | 22.0 [21.9–22.2] / 22.1 [21.8–22.2] | 2 / 2 | 1.09× | 1.11× |  |
| `kernel-org-home` | 34.4 [34.4–34.5] / 32.5 | 21.3 [21.2–21.5] / 20.9 [20.5–21.0] | 3 / 3 | 0.94× | 0.94× |  |
| `lobsters-front` | 139.0 [139.0–141.1] / 135.9 [135.9–136.8] | 23.6 [23.5–23.6] / 23.7 [23.6–23.7] | 14 / 14 | 0.98× | 0.99× |  |
| `mdn-reference` | 2,674.2 [2,674.2–5,808.5] / 4,620.7 [4,620.7–4,629.9] | 133.9 [133.3–134.2] / 71.2 [49.2–71.4] | 75 / 75 | 1.73× | 1.65× |  |
| `nginx-org-home` | 39.1 [39.1–39.9] / 38.0 [38.0–38.2] | 22.0 [21.6–22.1] / 21.5 [21.2–22.0] | 2 / 2 | 0.97× | 0.96× |  |
| `nodejs-org-home` | 962.3 [962.3–1,485.2] / 1,035.0 [1,035.0–1,247.0] | 53.6 [50.8–53.8] / 48.3 [47.6–48.4] | 23 / 23 | 1.08× | 0.65× | recheck 1.05× |
| `npr-text-story` | 33.6 [33.6–33.7] / 30.6 [30.6–35.8] | 21.9 [21.8–22.3] / 21.5 [21.4–21.6] | 3 / 3 | 0.91× | 0.91× |  |
| `paulgraham-home` | 64.5 [64.5–68.8] / 62.6 [62.6–68.3] | 22.8 [22.5–23.2] / 22.9 [22.7–23.0] | 2 / 5 | 0.97× | 1.05× | replies differ, excluded |
| `python-docs-search-results` | 1,707.3 [1,707.3–2,456.7] / 1,654.5 [1,654.5–1,778.3] | 84.8 [82.9–91.1] / 88.6 [88.2–90.4] | 56 / 50 | 0.97× | 0.93× | replies differ, excluded |
| `python-docs-search` | 1,104.3 [1,104.3–1,104.7] / 1,084.8 [1,084.8–1,087.4] | 75.2 [75.1–76.0] / 74.1 [73.8–75.0] | 40 / 40 | 0.98× | 1.35× | recheck 0.98× |
| `python-docs-tutorial` | 254.1 [254.1–254.2] / 257.0 [257.0–257.5] | 24.5 [24.0–24.5] / 24.4 [23.9–24.5] | 59 / 59 | 1.01× | 1.01× |  |
| `python-org-docs-link` | 1,193.5 [1,193.5–1,223.1] / 1,220.6 [1,220.6–1,257.7] | 38.5 [38.4–38.7] / 36.4 [35.7–36.5] | 18 / 16 | 1.02× | 1.15× | replies differ, excluded; recheck 1.02× |
| `python-org-search` | 1,244.9 [1,244.9–1,295.8] / 1,270.9 [1,270.9–1,290.1] | 38.8 [38.0–39.5] / 36.3 [36.1–36.5] | 16 / 16 | 1.02× | 1.02× |  |
| `react-docs` | 3,398.3 [3,398.3–4,207.2] / 5,920.9 [5,920.9–6,533.0] | 86.3 [79.1–87.1] / 84.4 [82.9–85.7] | 11 / 12 | 1.74× | 0.11× | replies differ, excluded; recheck 2.32× |
| `rust-book-chapters` | 579.3 [579.3–589.2] / 517.7 [517.7–632.8] | 29.8 [29.8–30.0] / 27.5 [27.3–27.5] | 60 / 59 | 0.89× | 0.97× | replies differ, excluded |
| `rust-book-search` | 2,375.1 [2,375.1–2,381.7] / 1,849.2 [1,849.2–1,859.1] | 105.7 [105.4–105.9] / 86.5 [86.4–86.6] | 21 / 21 | 0.78× | 0.79× |  |
| `rust-book-sidebar` | 303.1 [303.1–368.5] / 284.5 [284.5–286.5] | 28.0 [28.0–28.2] / 26.4 [26.3–26.5] | 38 / 40 | 0.94× | 0.93× | replies differ, excluded |
| `rust-lang-org-home` | 80.5 [80.5–91.6] / 89.1 [89.1–89.2] | 24.4 [24.0–24.5] / 23.8 [23.7–24.1] | 14 / 14 | 1.11× | 1.11× |  |
| `sqlite-org-home` | 109.2 [109.2–150.5] / 127.7 [127.7–149.8] | 25.2 [25.2–25.5] / 23.2 [22.7–25.4] | 6 / 7 | 1.17× | 0.85× | replies differ, excluded; recheck 0.83× |
| `substack-home` | 7,349.6 [7,349.6–7,755.9] / 6,991.9 [6,991.9–7,893.1] | 187.4 [187.1–189.2] / 136.3 [135.8–138.2] | 22 / 142 | 0.95× | 0.28× | replies differ, excluded; recheck 0.90× |
| `w3c-home-links` | 720.7 [720.7–722.7] / 683.1 [683.1–1,550.3] | 29.5 [29.1–29.6] / 29.0 [28.8–29.1] | 7 / 7 | 0.95× | 1.25× | recheck 0.95× |
| `wikipedia-article` | 1,793.5 [1,793.5–1,903.3] / 1,844.0 [1,844.0–1,912.5] | 41.8 [41.3–42.1] / 38.7 [38.5–38.7] | 23 / 20 | 1.03× | 1.00× | replies differ, excluded |
| `wikipedia-main-random` | — / — | — / — | — / — | — | — | error, excluded |
| `wikipedia-portal` | 510.9 [510.9–514.2] / 514.3 [514.3–516.6] | 33.8 [33.8–33.9] / 31.3 [30.8–31.4] | 5 / 5 | 1.01× | 1.01× |  |
| `wikipedia-search` | 1,849.4 [1,849.4–1,977.2] / 1,873.0 [1,873.0–1,987.2] | 42.0 [42.0–42.1] / 39.3 [39.0–39.5] | 16 / 16 | 1.01× | 1.01× |  |
| `wikipedia-toc-languages` | 1,237.0 [1,237.0–1,365.9] / 1,365.3 [1,365.3–1,378.9] | 38.7 [38.6–39.9] / 36.6 [36.4–36.7] | 4 / 4 | 1.10× | 1.00× | recheck 0.99× |
| `wiktionary-portal` | 817.1 [817.1–832.5] / 792.1 [792.1–808.4] | 37.5 [37.4–37.7] / 33.9 [33.7–34.0] | 5 / 5 | 0.97× | 0.97× |  |

### Load-test ramps

| Workload | Instructions, M | Peak RSS, MiB | Replies | Ratio now | Ratio #942 | Note |
|---|---:|---:|---:|---:|---:|---|
| `churn_1m` | — / — | — / — | — | — | — | error, excluded |
| `compm_22000` | 405.1 / 403.2 [403.2–403.3] | 37.4 [37.3–37.8] / 37.0 [36.9–37.4] | — | 1.00× | 1.00× |  |
| `compm_24000` | 445.1 / 443.3 | 38.1 [38.0–38.6] / 38.0 [37.7–38.2] | — | 1.00× | 1.00× |  |
| `compm_26000` | 482.7 / 480.8 | 39.9 [39.6–40.3] / 39.9 [39.2–40.1] | — | 1.00× | 1.00× |  |
| `compm_30000` | 552.4 [552.4–560.1] / 550.5 [550.5–550.7] | 42.3 [42.1–42.5] / 42.6 [42.2–42.7] | — | 1.00× | 1.00× |  |
| `compm_34000` | 623.3 [623.3–623.4] / 621.4 [621.4–626.1] | 45.9 [45.7–47.3] / 46.4 [45.7–46.6] | — | 1.00× | 1.00× |  |
| `compm_38000` | 692.9 [692.9–697.9] / 691.0 [691.0–691.1] | 48.4 [48.4–50.1] / 48.4 [48.4–48.8] | — | 1.00× | 1.00× |  |
| `compm_40000` | 728.0 [728.0–728.1] / 726.2 [726.2–731.0] | 49.4 [49.3–50.3] / 49.8 [49.6–50.6] | — | 1.00× | 1.00× |  |
| `compm_43000` | 780.8 [780.8–787.3] / 779.0 [779.0–780.6] | 51.9 [51.5–52.7] / 51.7 [51.6–54.8] | — | 1.00× | 1.00× |  |
| `ctor_100k` | 904.5 / 811.4 [811.4–812.3] | 20.4 [20.0–20.8] / 20.2 [19.6–20.2] | — | 0.90× | 0.90× |  |
| `ctor_10k` | 95.9 / 85.5 [85.5–85.6] | 20.4 [20.2–20.8] / 19.8 [19.7–20.0] | — | 0.89× | 0.89× |  |
| `ctor_1k` | 15.1 / 12.9 | 20.3 [20.1–20.7] / 20.2 [20.1–20.4] | — | 0.86× | 0.86× |  |
| `ctor_1m` | 8,990.1 / 8,069.9 | 20.0 [19.4–20.0] / 20.1 [19.6–20.2] | — | 0.90× | 0.90× |  |
| `ctor_read_100k` | 1,897.5 / 1,881.6 [1,881.6–1,882.2] | 64.0 [63.9–64.1] / 32.0 [31.2–32.1] | — | 0.99× | 0.99× |  |
| `ctor_read_10k` | 191.4 / 194.0 | 25.0 [24.7–25.4] / 23.0 [22.8–23.0] | — | 1.01× | 1.01× |  |
| `ctor_read_1k` | 24.7 / 23.9 | 20.3 [20.0–20.4] / 20.6 [20.5–20.8] | — | 0.97× | 0.97× |  |
| `loop_100000` | 95.4 / 33.1 | 19.7 [19.7–20.4] / 20.1 [19.5–20.3] | — | 0.35× | 0.35× |  |
| `loop_1000000` | 902.7 / 297.7 | 20.0 [19.4–20.1] / 20.2 | — | 0.33× | 0.33× |  |
| `loop_10000000` | 8,975.7 / 2,943.7 | 19.7 [19.3–19.9] / 19.6 [19.3–20.0] | — | 0.33× | 0.33× |  |
| `readall_fwd` | 1,492.0 [1,492.0–1,494.4] / 1,559.3 [1,559.3–1,560.0] | 78.1 [77.4–78.2] / 71.8 [71.5–73.1] | — | 1.05× | 1.04× |  |
| `readall_rev` | 1,462.1 [1,462.1–1,462.3] / 1,505.5 [1,505.5–1,509.8] | 78.2 [77.9–78.6] / 71.8 [71.6–71.9] | — | 1.03× | 1.03× |  |
| `readall_spans` | 1,263.3 [1,263.3–1,263.7] / 1,319.7 | 73.0 [72.7–73.0] / 68.2 [68.0–68.3] | — | 1.04× | 1.04× |  |
| `rest_13` | 1,079.4 [1,079.4–1,079.5] / 1,078.2 | 51.6 [51.4–51.9] / 51.9 [51.5–52.6] | — | 1.00× | 1.00× |  |
| `style_100k` | 2,159.4 [2,159.4–2,160.3] / 2,011.3 [2,011.3–2,011.8] | 31.0 [30.7–31.4] / 32.5 [32.3–32.9] | — | 0.93× | 0.93× |  |
| `style_10k` | 219.8 [219.8–220.1] / 203.4 | 21.1 [20.9–21.4] / 23.1 [22.9–23.1] | — | 0.93× | 0.92× |  |
| `style_1k` | 27.4 / 24.7 | 20.8 [20.4–20.8] / 20.4 [20.1–20.9] | — | 0.90× | 0.90× |  |
| `style_250k` | 5,385.6 / 5,021.2 [5,021.2–5,021.4] | 47.7 [47.7–47.9] / 48.7 [48.7–48.8] | — | 0.93× | 0.93× |  |

### Typing and other controls

| Workload | Instructions, M | Peak RSS, MiB | Replies | Ratio now | Ratio #942 | Note |
|---|---:|---:|---:|---:|---:|---|
| `typing` | 6,392.8 [6,392.8–6,392.9] / 5,409.6 [5,409.6–5,412.2] | 38.4 [38.2–38.5] / 29.9 [29.9–30.4] | — | 0.85× | 0.89× |  |
| `loop_local_10000000` | 1,985.7 / 1,463.8 | 19.6 [19.3–20.4] / 19.7 [19.4–20.1] | — | 0.74× | 0.74× |  |
| `object_1000` | 8.6 / 9.0 | 20.0 [19.7–20.0] / 20.5 [20.5–20.7] | — | 1.05× | 1.05× |  |
| `object_2000` | 11.6 / 14.4 | 20.1 [20.0–20.4] / 20.9 [20.3–21.1] | — | 1.24× | 1.24× |  |
| `object_4000` | 17.9 / 24.3 [24.3–24.4] | 20.5 [20.0–21.2] / 21.3 [21.0–21.4] | — | 1.36× | 1.36× |  |
| `object_8000` | 30.4 / 43.8 [43.8–43.9] | 21.7 [21.7–22.2] / 22.2 [21.4–22.5] | — | 1.44× | 1.44× |  |


### Excluded rows

| Row | Reason | Recheck |
|---|---|---|
| `wikipedia-main-random` | Both engines failed. The `#mp-tfa p a` assertion timed out after 40 s, as in #923 and #942. | — |
| `churn_1m` | Both engines failed. OLD hit the `dom-nodes` limit. js2 went idle before `window.__done`. Same as #942. | — |
| `github-docs-home` | HTTP 429 left the title assertion unsatisfied in both engines. | 0.95× on 2 samples per engine. The recheck stopped on HTTP 429. |
| `blog-rust-lang-home` | Replies 9 / 13 | — |
| `hn-new-past` | Replies 6 / 5 | — |
| `paulgraham-home` | Replies 2 / 5 | — |
| `python-docs-search-results` | Replies 56 / 50 | — |
| `python-org-docs-link` | Replies 18 / 16 | 1.02×, replies 16 / 16 |
| `react-docs` | Replies 11 / 12 | 2.32×, replies 12 / 6 |
| `rust-book-chapters` | Replies 60 / 59 | — |
| `rust-book-sidebar` | Replies 38 / 40 | — |
| `sqlite-org-home` | Replies 6 / 7 | 0.83×, replies 7 / 7 |
| `substack-home` | Replies 22 / 142. The minimum OLD sample loaded 22 replies. The other five OLD samples loaded 141 or 142. | 0.90×, replies 141 / 142 |
| `wikipedia-article` | Replies 23 / 20 | — |

### Public rows outside the #942 set

`tools/drive/public` has 48 step files. #923 and #942 used the 44 that passed triage. The other 4 ran once with the same harness. They are not in the totals.

| Row | Result |
|---|---|
| `lobsters-comments-back` | Complete in both engines. 210.7 / 211.4 M instructions (1.00×), 23.0 / 22.7 MiB, replies 10 / 10. |
| `nextjs-docs` | The page stopped on `Unsupported("checkVisibility with content-visibility…")`. No memory sample passed in either engine. |
| `curl-se-home` | Both engines failed. The `a[href='/docs/']` link has no box. |
| `duckduckgo-html-search` | Both engines failed. The page had no result links. |

## Recheck of rows that moved more than 10%

| Row | Ratio #942 | Ratio sweep | Ratio recheck | OLD recheck, M | js2 recheck, M | Replies recheck |
|---|---:|---:|---:|---:|---:|---:|
| `craigslist-home` | 5.38× | 0.20× | 0.84× | 2,353 | 1,965 | 3 / 3 |
| `crates-io-home` | 1.12× | 1.57× | 1.55× | 279 | 432 | 39 / 39 |
| `devto-home` | 0.97× | 1.17× | 1.05× | 4,930 | 5,172 | 1 / 1 |
| `github-docs-home` | 0.70× | 0.94× | 0.95× | 2,012 | 1,908 | 24 / 24 |
| `go-dev-home` | 0.56× | 0.76× | 0.53× | 1,864 | 989 | 13 / 15 |
| `nodejs-org-home` | 0.65× | 1.08× | 1.05× | 989 | 1,038 | 23 / 23 |
| `python-docs-search` | 1.35× | 0.98× | 0.98× | 1,104 | 1,084 | 40 / 40 |
| `python-org-docs-link` | 1.15× | 1.02× | 1.02× | 1,200 | 1,221 | 16 / 16 |
| `react-docs` | 0.11× | 1.74× | 2.32× | 2,589 | 6,014 | 12 / 6 |
| `sqlite-org-home` | 0.85× | 1.17× | 0.83× | 151 | 125 | 7 / 7 |
| `substack-home` | 0.28× | 0.95× | 0.90× | 7,642 | 6,883 | 141 / 142 |
| `w3c-home-links` | 1.25× | 0.95× | 0.95× | 710 | 677 | 5 / 7 |
| `wikipedia-toc-languages` | 1.00× | 1.10× | 0.99× | 1,242 | 1,228 | 4 / 4 |

The recheck confirms these moves: `crates-io-home`, `nodejs-org-home`, `python-docs-search`, `python-org-docs-link`, `react-docs`, `substack-home` and `w3c-home-links`. It does not confirm the sweep values for `craigslist-home`, `devto-home`, `sqlite-org-home` and `wikipedia-toc-languages`. These rows have bimodal samples. `go-dev-home` has a wide spread in both runs (989–5,919 M).

Console evidence for the confirmed moves, from the stored driver reports:

- `react-docs`: every #942 js2 sample with 12 replies logged `page: Unsupported("clone_out")`, and those samples used 597–599 M instructions. No sample in this run logged a page diagnostic. js2 now uses 5,921–7,242 M. The #942 ratio measured a page that stopped early.
- `crates-io-home`: in #942, all samples in both engines logged an uncaught promise error and used 85–99 M. Now no sample logs an error, and both engines use more (OLD 277–285 M, js2 432–555 M). js2 now uses 1.55× the OLD instructions.
- `nodejs-org-home`: two of three #942 js2 samples logged `Uncaught (in promise) [object Object]`. No sample logs it now. js2 went from 852 to 1,035 M. OLD went from 1,306 to 962 M.
- `python-docs-search`: js2 fell from 1,498 to 1,085 M with the same 40 replies and 42,122 bytes. OLD stayed at 1,104 M.
- `substack-home`: both engines loaded 142 replies in #942 and now. js2 went from 1,657 to 6,883–8,651 M. OLD went from 5,924 to 7,350–7,817 M.

## Noise notes

- `craigslist-home` is bimodal. In #942, one OLD sample took 377 M and the others 2,350 M. Now all six OLD samples took 2,353 M, and one of six js2 samples took 475 M against 1,963–1,967 M. The minimum rule keeps the short sample. The sweep ratio of 0.20× is not a real gain. The recheck gives 0.84×.
- `mdn-reference` has 75 / 75 replies in the minimum samples, so it stays in the totals. Its OLD samples span 2,674–5,808 M. The minimum OLD sample is low, as in #923 and #942.
- `hn-front` fell from 788 to 208 M in both engines, and the ratio stayed at 0.99×. hypothesis: the live page changed.
- The ramp and control instruction counts match #942 within 0.2% in both engines. OLD typing rose by 0.3% (6,375.4 to 6,392.8 M).
- Peak RSS on ramps and controls: OLD fell by a median of 0.74 MiB against #942. js2 rose by a median of 0.20 MiB. Because of this, js2 now wins RSS on 14 of 26 ramps instead of 23 of 26. hypothesis: the shared Intl components (#952) and the removed startup relocations (#955, #956) lowered the base RSS of the browser binary, and OLD gained more because its Intl code moved to the shared crate. This report does not verify the cause.
- Standalone js2 `empty_1` peak RSS rose from 5.60 to 5.95 MiB, while its instructions fell from 0.72 to 0.70 M. The OLD `empty_1` RSS fell from 6.19 to 5.97 MiB. The RSS ranges are about 0.1–0.4 MiB.
- Live public servers can send different content in each run. Reply counts and bytes appear with each row. The minimum samples of 31 clean public rows have equal reply counts. This report does not compare their bytes or console errors.

## What changed since #942

Fifteen first-parent merges are in `ab09c54b..f3b30305`, among them #943–#956: js2 public-journey fixes, typed host indexes, data literal spans, public `TextEncoding`, numeric host probes, the `crates-io` cost fix, shared Intl components, and the Intl relocation removal. This report measures the combined tree. It does not attribute changes to PRs.

## Evidence and limits

Raw JSONL, logs, the summarizer and `summary.json` are in `/data/bare-tmp/r23/final2/` on archMachine. The build target directory was deleted after the run.

Not measured: Node, OLD `eval_load`, a QuickJS browser (none exists), wall time on an idle machine, Windows, windowed runs, recorded-response replays, test262 and the code-ordered build. Ready times are in `summary.json` but are not compared here.
