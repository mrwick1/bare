# 2026-11-22 — Engine decision benchmark rerun

js2 used 2.640 G instructions across the 16 standalone workloads. This is 25% less than the js2 build in #906, 69% less than OLD matched, and 9% less than the QuickJS direct launcher. QuickJS still used less peak memory in aggregate. [#906 engine decision benchmark](2026-11-21-engine-decision-benchmark.md)

Across 43 complete public pairs, js2 used 2% fewer instructions and 13% less peak RSS in aggregate than OLD `auto`. Excluding MDN, it used 8% fewer instructions. Across 26 completed ramps it used 20% fewer instructions. MDN fetched the compatibility data in all js2 samples and the prior startup rejection did not recur.

## Frozen inputs

| Input | Commit or version | Role |
|---|---|---|
| OLD | `48ccf6928970f52a79a7ed50a8aad4bfa03b84c7` | Standalone OLD matched and original `eval_load` |
| js2 | `3c46337ef009e88153ef9f152452dcb8ada007a3` | Standalone js2 and both browser modes |
| #906 js2 | `05b520eb1876997a91b4e43368f33d493071f1e5` | Prior scratch build used in the comparison table |
| QuickJS | Bellard 2026-06-04 | GCC 16.2.1, `-O2`, `CONFIG_LTO=y` |
| Node | 22.18.0, V8 12.4.254.21-node.27 | Same executable as #906. Default JIT and `--jitless` |

The machine was `archMachine`, AMD Ryzen 9 5900HX with 8 cores and 16 threads, 30.7 GiB usable RAM, Linux `7.2.15-zen1-1-zen`, glibc 2.44, Rust 1.96.0 and perf 7.3.1-1. Rust builds used locked dependencies, eight Cargo jobs and one target directory per tree. The QuickJS archive SHA-256 was `b376e839b322978313d929fd20663b11ba58b75df5a46c126dd19ea2fa70ad2a`. The rebuilt CLI and direct runner match the prior stripped sizes.

## A. Standalone engines

Each case ran three times in fresh processes under `perf stat -e instructions:u` and three more times under the same static `wait4` wrapper. Each instruction count includes startup, workload and shutdown. Use the minimum instruction count and median peak RSS. Brackets show the min–max spread where visible. The RSS samples use a fresh `cp --reflink=never` binary copy, which adds a step to the #906 standalone RSS script.

- The frozen OLD `eval_load CASE SCALE --source` generated the scripts and expected completion values. The source hashes and checksums match #906 for every case. Both Rust runners and QuickJS direct read the raw source as a global script. The CLI cases pass the same bytes to indirect `eval`. The empty case evaluates `0`. All instruction runs passed the checksum. All RSS runs exited successfully.
- OLD matched uses `Exec::Auto` and the system allocator, with no forced collection at exit. OLD `eval_load` retains its allocation counters and final collection. The js2 runner uses the script completion API. The QuickJS direct launcher uses the CLI's LTO engine objects.
- The 314,192-byte React typing bundle has SHA-256 `1992c72031ad2bad490a784172a2e9400b822cb0eb4d0e539f4a021af7c07400`. OLD and js2 parse the full script. The js2 eager compile row compiles function bodies with `script::compile`. The QuickJS launcher uses `JS_EVAL_FLAG_COMPILE_ONLY`. Each bundle row stops before evaluation.

The 16-case totals exclude startup and the bundle controls. js2 won all 16 instruction comparisons with OLD matched and eight with each QuickJS runner. QuickJS used less peak RSS on 15 of 16 rows. js2 used less peak RSS on `closures_10`.

The largest absolute standalone CPU reduction from #906 was `objects_10`, from 1,023.58 M to 551.20 M. Its peak RSS fell from 21.83 to 8.90 MiB. Eager React compilation fell from 323.21 M to 281.83 M instructions. The OLD and QuickJS CPU totals reproduced to the shown precision. OLD matched and Node peak RSS increased from #906 despite the unchanged workloads. Use the paired measurements in this run for RSS comparisons.

### Standalone totals and geometric means

| Engine | Instructions total, G | Instructions geomean, M | RSS sum, MiB | RSS geomean, MiB |
|---|---:|---:|---:|---:|
| OLD matched | 8.404 | 102.37 | 152.19 | 8.12 |
| OLD eval_load | 8.686 | 110.10 | 165.36 | 8.69 |
| js2 | 2.640 | 39.03 | 105.51 | 6.49 |
| QuickJS CLI | 3.275 | 51.21 | 69.47 | 3.97 |
| QuickJS direct | 2.895 | 46.89 | 66.88 | 3.80 |
| Node jitless | 6.313 | 262.60 | 900.34 | 56.22 |
| Node JIT | 4.456 | 166.19 | 1015.02 | 63.28 |

### Instructions, millions

| Case | OLD matched | OLD eval_load | js2 | QuickJS CLI | QuickJS direct | Node jitless | Node JIT |
|---|---:|---:|---:|---:|---:|---:|---:|
| `empty` | 2.09 | — | 0.72 | 1.23 | 1.05 | 126.70 [126.70–126.99] | 94.47 [94.47–94.59] |
| `loop_1` | 227.01 | 230.59 | 75.38 | 64.69 | 64.51 | 218.37 [218.37–218.83] | 117.36 [117.36–119.96] |
| `loop_10` | 2,341.56 | 2,373.04 | 746.78 | 635.30 | 635.12 | 1,043.16 [1,043.16–1,043.40] | 183.44 [183.44–186.31] |
| `calls_1` | 159.88 | 162.17 | 63.91 | 60.62 | 60.44 | 195.76 [195.76–196.06] | 115.76 [115.76–116.90] |
| `calls_10` | 1,584.79 | 1,603.28 | 630.91 | 594.33 | 594.15 | 815.49 [815.49–815.61] | 146.91 [146.91–148.04] |
| `objects_1` | 195.11 [195.11–195.12] | 198.48 | 59.68 [59.68–59.69] | 53.38 | 52.98 | 174.44 [174.44–174.59] | 146.20 [146.20–146.85] |
| `objects_10` | 1,909.39 [1,909.39–1,909.45] | 1,935.55 [1,935.55–1,935.56] | 551.20 [551.20–551.52] | 509.04 | 507.64 | 554.97 [554.97–555.77] | 333.09 [333.09–334.24] |
| `throws_1` | 128.10 | 139.72 | 27.59 | 88.49 [88.49–88.52] | 55.13 [55.13–55.16] | 291.58 [291.58–292.33] | 295.93 [295.93–296.35] |
| `throws_10` | 1,279.10 | 1,395.34 | 268.81 | 874.13 [874.13–874.80] | 535.82 [535.82–536.09] | 1,763.35 [1,763.35–1,763.59] | 2,171.54 [2,171.54–2,171.78] |
| `closures_1` | 43.14 | 49.05 | 12.65 | 26.06 [26.06–26.11] | 25.92 [25.92–25.94] | 141.28 [141.28–141.55] | 111.05 [111.05–111.51] |
| `closures_10` | 412.56 | 467.33 | 113.25 [113.25–113.26] | 279.40 [279.40–279.65] | 277.48 [277.48–277.93] | 273.78 [273.78–277.10] | 198.28 [198.28–203.13] |
| `fib_1` | 7.10 | 7.66 | 3.75 | 4.37 | 4.19 | 129.53 [129.53–129.81] | 98.86 [98.86–99.11] |
| `fib_10` | 56.88 | 58.24 [58.24–58.25] | 33.53 | 35.55 | 35.37 | 157.24 [157.24–157.72] | 109.43 [109.43–110.53] |
| `webpack_1` | 6.70 | 7.53 | 6.02 | 5.41 | 4.99 | 131.72 [131.72–132.05] | 99.39 [99.39–99.47] |
| `webpack_10` | 47.22 | 50.88 | 44.72 [44.72–44.73] | 40.78 [40.78–40.79] | 38.21 [38.21–38.22] | 168.84 [168.84–169.29] | 138.47 [138.47–138.73] |
| `concat_1` | 2.29 | 2.79 | 0.84 | 1.34 | 1.16 | 126.78 [126.78–127.02] | 94.71 [94.71–94.94] |
| `concat_10` | 3.40 [3.40–3.41] | 4.00 | 1.29 | 2.15 | 1.97 | 127.20 [127.20–127.63] | 95.18 [95.18–95.25] |

### Peak RSS, MiB

| Case | OLD matched | OLD eval_load | js2 | QuickJS CLI | QuickJS direct | Node jitless | Node JIT |
|---|---:|---:|---:|---:|---:|---:|---:|
| `empty` | 6.19 [5.99–6.22] | — | 5.60 [5.57–5.68] | 3.39 [3.38–3.41] | 3.12 [3.09–3.18] | 54.66 [54.50–54.74] | 56.82 [56.75–56.94] |
| `loop_1` | 6.12 [6.12–6.25] | 6.52 [6.10–6.59] | 5.67 [5.52–5.73] | 3.41 [3.36–3.42] | 3.20 [2.94–3.20] | 54.66 [54.58–54.75] | 63.37 [63.32–63.41] |
| `loop_10` | 6.25 [6.13–6.31] | 6.32 [6.23–6.58] | 5.53 [5.39–5.64] | 3.36 [3.22–3.41] | 3.04 [2.98–3.24] | 54.73 [54.56–54.75] | 63.39 [63.32–63.41] |
| `calls_1` | 6.07 [6.02–6.15] | 6.36 [6.23–6.53] | 5.88 [5.84–5.88] | 3.25 [3.24–3.34] | 3.18 [3.09–3.32] | 54.75 [54.62–54.82] | 63.50 [63.41–63.63] |
| `calls_10` | 6.19 [6.12–6.30] | 6.45 [6.41–6.56] | 5.75 [5.63–5.82] | 3.25 [3.23–3.31] | 3.12 [3.09–3.29] | 54.59 [54.56–54.62] | 63.53 [63.50–63.56] |
| `objects_1` | 6.74 [6.57–6.85] | 7.04 [6.86–7.04] | 6.73 [6.59–6.88] | 3.73 [3.67–3.74] | 3.59 [3.54–3.67] | 56.09 [56.02–56.12] | 64.93 [64.81–65.00] |
| `objects_10` | 9.50 [9.50–9.61] | 12.70 [12.59–12.73] | 8.90 [8.86–8.96] | 6.49 [6.38–6.54] | 6.46 [6.44–6.46] | 61.66 [61.48–61.71] | 71.04 [71.00–71.10] |
| `throws_1` | 8.52 [8.51–8.63] | 8.89 [8.86–8.95] | 6.09 | 3.46 [3.42–3.54] | 3.27 [3.17–3.27] | 56.50 [56.48–56.50] | 65.22 [63.34–65.25] |
| `throws_10` | 32.32 [32.14–32.38] | 33.58 [33.55–33.70] | 7.42 [7.41–7.43] | 3.38 [3.26–3.41] | 3.20 [3.18–3.25] | 60.04 [60.00–60.07] | 68.75 [68.63–68.75] |
| `closures_1` | 7.76 [7.70–7.88] | 8.61 [8.43–8.70] | 6.25 [6.25–6.31] | 4.43 [4.40–4.52] | 4.23 [4.21–4.27] | 56.02 [56.00–56.09] | 64.84 [64.79–65.00] |
| `closures_10` | 23.42 [23.38–23.51] | 27.98 [27.92–28.29] | 10.26 [10.22–10.30] | 13.58 [13.55–13.75] | 13.50 [13.47–13.58] | 62.37 [62.34–62.52] | 71.38 [71.33–71.57] |
| `fib_1` | 6.24 [6.10–6.26] | 6.52 [6.44–6.58] | 5.72 [5.68–5.96] | 3.42 [3.26–3.43] | 3.21 [3.18–3.25] | 54.73 [54.70–54.78] | 63.38 [63.33–63.40] |
| `fib_10` | 6.25 [6.18–6.26] | 6.36 [6.27–6.49] | 5.72 [5.70–5.72] | 3.29 [3.14–3.39] | 3.19 [3.12–3.21] | 54.75 [54.74–54.75] | 63.38 [63.36–63.41] |
| `webpack_1` | 6.39 [6.25–6.50] | 6.83 [6.81–6.83] | 6.35 [6.32–6.38] | 3.50 [3.36–3.52] | 3.32 [3.14–3.38] | 54.82 [54.75–54.87] | 56.91 [56.80–56.94] |
| `webpack_10` | 8.01 [7.93–8.01] | 8.14 [8.09–8.39] | 7.65 [7.38–8.00] | 4.13 [4.00–4.19] | 3.94 [3.65–4.06] | 55.34 [55.32–55.38] | 57.64 [57.57–57.69] |
| `concat_1` | 6.18 [6.00–6.27] | 6.48 [6.45–6.52] | 5.82 [5.72–5.82] | 3.40 [3.23–3.42] | 3.23 [3.03–3.31] | 54.62 [54.62–54.73] | 56.93 [56.68–56.94] |
| `concat_10` | 6.24 [6.07–6.25] | 6.58 [6.52–6.62] | 5.77 [5.42–5.88] | 3.38 [3.21–3.42] | 3.23 [3.09–3.34] | 54.67 [54.66–54.75] | 56.82 [56.82–56.94] |

### React bundle front end

| Engine and work | Instructions, M | Peak RSS, MiB |
|---|---:|---:|
| OLD full parse | 69.67 [69.67–69.68] | 7.15 [7.13–7.59] |
| js2 full parse | 63.49 | 7.16 [6.61–7.20] |
| js2 eager compile | 281.83 [281.83–281.86] | 7.99 [7.93–8.20] |
| QuickJS compile only | 247.32 [247.32–247.37] | 6.79 [6.60–6.84] |

## B. Browser journeys

Both modes use one profiling build at `3c46337e`. The build uses frame pointers and debug symbols.
The source tree of `crates/js` is identical at `48ccf692` and `3c46337e`.
`BARE_JS_EXEC=auto` selects the tiered OLD engine. `BARE_JS_EXEC=new` selects js2.

- The 44 public inputs are the same journeys as #906. The step files have Git tree `36a003426642bb137fb5c4b4e3798cbebfa6e6b7`. The driver uses the URLs in the step comments and runs headless with `BARE_SEED=1`. Replies are live. Page assertions can pass with different traffic and script errors.
- The 27 ramp inputs include `churn_1m`. They run over loopback. The loop and large-object controls use the five extra fixtures from #906. All 32 HTML files match the earlier SHA-256 manifest. The typing runner types 50 characters into `#a1` in the same React page.
- Modes rotate within each case. Each case targets three successful instruction samples and three separate successful memory samples per mode. The counters require at least 99% coverage. Instruction runs use `glibc.malloc.tcache_count=65535`. The memory runs use the default allocator and a fresh binary copy.
- Ready time adds step durations through the first action after `open`. For ramps this is the completion check. For typing it is the reported load time. One-minute load was 0.09–5.14 before the samples. Use the instruction counts for CPU comparisons. Ready times are observations from a shared machine.

Each cell gives **OLD `auto` / js2 `new`**. A bracket gives the min–max spread at the shown precision. Totals require three samples for all metrics in both modes. A win means js2 had the lower value for a paired case.

### Totals

| Group | Paired cases | Instructions, G | Sum of peak RSS, MiB | Sum of ready time, s |
|---|---:|---:|---:|---:|
| Public | 43/44 | 40.55 → 39.59 (0.98×) | 1,900.58 → 1,653.17 (0.87×) | 20.68 → 21.28 (1.03×) |
| Public excluding MDN | 42/43 | 37.67 → 34.61 (0.92×) | 1,765.07 → 1,585.97 (0.90×) | 20.50 → 21.10 (1.03×) |
| Ramps | 26/27 | 39.89 → 31.77 (0.80×) | 1,027.62 → 966.73 (0.94×) | 3.52 → 3.08 (0.87×) |
| Typing | 1/1 | 6.38 → 7.66 (1.20×) | 38.82 → 31.21 (0.80×) | 0.07 → 0.07 (1.01×) |
| Other controls | 5/5 | 2.05 → 1.56 (0.76×) | 105.21 → 104.80 (1.00×) | 0.14 → 0.13 (0.97×) |

| Group | Median CPU ratio / wins | Median RSS ratio / wins | Median ready ratio / wins |
|---|---:|---:|---:|
| Public | 1.01× / 18/43 | 0.98× / 33/43 | 1.01× / 18/43 |
| Public excluding MDN | 1.01× / 18/42 | 0.98× / 32/42 | 1.01× / 17/42 |
| Ramps | 0.99× / 22/26 | 0.98× / 21/26 | 0.99× / 15/26 |
| Typing | 1.20× / 0/1 | 0.80× / 1/1 | 1.01× / 0/1 |
| Other controls | 1.23× / 1/5 | 1.00× / 3/5 | 1.03× / 1/5 |

The similar-traffic subset has 34 complete public pairs. Its instruction total is 28.05 → 25.80 (0.92×) G. The minimum samples have the same request count, bytes within 1%, and equal counts of console errors and page diagnostics.

### Public journeys

| Workload | Instructions, M | Peak RSS, MiB | Ready, ms |
|---|---:|---:|---:|
| `archlinux-org-home` | 81.0 [81.0–81.9] / 77.8 [77.8–78.4] | 23.9 [23.8–24.1] / 23.7 [23.1–24.0] | 734.3 [723.2–734.4] / 728.7 [721.1–739.5] |
| `blog-rust-lang-home` | 155.7 [155.7–156.3] / 156.6 [156.6–180.6] | 25.2 [24.9–25.4] / 24.5 [24.3–24.9] | 93.0 [87.7–93.1] / 92.7 [92.3–100.8] |
| `cnn-lite-front` | 70.9 [70.9–74.0] / 71.9 [71.9–73.1] | 24.4 [24.1–24.4] / 24.3 [23.9–24.5] | 197.0 [191.6–199.4] / 192.3 [190.6–199.3] |
| `craigslist-home` | 375.7 [375.7–2,351.6] / 483.7 [483.7–2,131.7] | 31.0 [30.8–31.0] / 33.9 [33.8–59.8] | 4,641.6 [4,511.6–6,011.3] / 4,514.0 [4,199.4–5,337.2] |
| `crates-io-home` | 85.4 [85.4–90.2] / 93.8 [93.8–99.4] | 26.2 [25.7–26.3] / 25.9 [25.7–26.3] | 375.8 [342.4–898.5] / 360.5 [339.7–375.2] |
| `debian-org-home` | 119.5 [119.5–123.0] / 116.0 [116.0–121.4] | 23.8 [23.5–24.0] / 23.2 [23.0–23.3] | 505.6 [469.6–618.6] / 497.7 [491.0–556.2] |
| `devto-home` | 4,974.1 [4,974.1–6,489.8] / 6,110.4 [6,110.4–6,310.7] | 95.3 [49.5–98.7] / 113.9 [112.2–116.4] | 143.9 [143.5–151.1] / 143.5 [143.5–145.2] |
| `docs-rs-home` | 69.7 [69.7–69.9] / 68.3 [68.3–71.0] | 24.4 [24.4–24.6] / 24.1 [23.6–24.6] | 103.9 [103.1–108.2] / 103.7 [103.7–113.0] |
| `example-learn-more` | 134.6 [134.6–135.4] / 149.4 [149.4–149.5] | 25.4 [25.3–25.6] / 26.3 [26.1–26.4] | 87.9 [87.4–91.7] / 105.2 [92.3–112.0] |
| `example-page` | 27.5 [27.5–27.6] / 26.5 | 22.8 [22.2–22.9] / 22.3 [22.1–22.3] | 87.8 [87.2–91.9] / 96.1 [83.7–109.1] |
| `github-docs-home` | 2,032.5 [2,032.5–2,072.2] / 1,761.5 [1,761.5–2,482.5] | 85.7 [73.4–86.0] / 74.6 [74.3–77.0] | 91.3 [87.2–99.4] / 92.1 [92.0–92.3] |
| `go-dev-home` | 1,930.1 [1,930.1–3,922.2] / 1,247.3 [1,247.3–1,473.9] | 56.1 [53.5–56.6] / 42.7 [42.6–43.1] | 381.7 [372.8–842.3] / 383.0 [378.4–393.3] |
| `hn-comments-back` | 188.4 [188.4–189.3] / 185.9 [185.9–186.0] | 24.3 [24.1–24.3] / 23.6 [23.6–23.8] | 701.2 [691.8–814.0] / 690.0 [674.9–1,046.4] |
| `hn-front` | 403.7 [403.7–403.9] / 383.6 [383.6–402.8] | 24.6 [24.5–24.8] / 24.6 [24.4–24.6] | 684.7 [681.3–694.5] / 790.3 [684.2–920.6] |
| `hn-new-past` | 188.6 [188.6–205.9] / 186.2 [186.2–204.4] | 23.4 [23.3–23.9] / 22.7 [22.4–23.0] | 925.5 [904.3–928.7] / 1,207.0 [958.6–2,200.0] |
| `httpbin-cookies` | 37.0 [37.0–37.1] / 33.6 [33.6–33.8] | 22.2 [21.8–22.3] / 21.6 [21.5–22.1] | 939.0 [897.7–1,038.1] / 946.6 [925.9–995.9] |
| `httpbin-form-fill` | 46.3 [46.3–46.6] / 45.2 | 23.0 [22.7–23.3] / 22.3 [22.0–22.4] | 679.5 [679.0–726.4] / 700.9 [689.5–714.9] |
| `httpbin-form-submit` | 58.5 [58.5–58.7] / 64.2 [64.2–64.8] | 22.9 [22.5–23.1] / 22.7 [22.6–23.0] | 702.8 [684.6–736.5] / 714.5 [710.9–777.9] |
| `kernel-org-home` | 33.3 [33.3–33.9] / 31.1 [31.1–31.2] | 22.3 [22.3–22.5] / 21.2 [21.1–21.4] | 166.5 [165.4–169.0] / 170.4 [168.6–170.4] |
| `lobsters-front` | 140.1 [140.1–142.3] / 135.1 [135.1–137.9] | 24.5 [24.4–24.5] / 24.8 [24.0–25.0] | 809.6 [807.9–815.4] / 869.5 [811.3–1,174.5] |
| `mdn-reference` | 2,880.3 [2,880.3–5,772.3] / 4,982.1 [4,982.1–4,988.3] | 135.5 [133.5–135.8] / 67.2 [67.2–67.3] | 178.2 [170.1–178.4] / 175.3 [169.8–179.5] |
| `nginx-org-home` | 39.3 [39.3–39.6] / 37.7 [37.7–38.0] | 22.6 [22.3–22.6] / 22.0 [21.9–22.3] | 783.2 [709.2–816.5] / 809.9 [776.0–812.9] |
| `nodejs-org-home` | 993.7 [993.7–1,416.3] / 1,253.6 [1,253.6–1,491.2] | 53.5 [46.6–54.6] / 49.7 [48.1–49.9] | 110.4 [109.5–118.2] / 106.5 [106.4–113.8] |
| `npr-text-story` | 33.8 [33.8–34.6] / 30.5 [30.5–34.5] | 22.7 [22.5–22.8] / 22.2 [22.1–22.5] | 192.2 [191.3–196.8] / 193.4 [189.2–202.8] |
| `paulgraham-home` | 58.4 [58.4–69.0] / 65.1 [65.1–68.2] | 23.7 [23.4–24.0] / 23.2 [23.0–23.4] | 752.6 [740.7–762.4] / 775.3 [744.8–795.3] |
| `python-docs-search-results` | 1,785.0 [1,785.0–2,458.9] / 1,728.7 [1,728.7–2,293.2] | 91.3 [90.3–91.8] / 93.6 [93.2–93.7] | 87.9 [87.3–88.6] / 96.1 [87.5–99.7] |
| `python-docs-search` | 1,104.3 [1,104.3–1,106.5] / 1,578.8 [1,578.8–1,581.3] | 76.3 [75.7–76.4] / 81.7 [81.6–82.8] | 92.0 [87.0–95.5] / 95.4 [91.7–95.8] |
| `python-docs-tutorial` | 253.8 [253.8–254.6] / 255.9 [255.9–259.1] | 24.8 [24.8–25.3] / 24.7 [24.6–24.7] | 152.8 [151.5–154.8] / 151.8 [147.8–167.9] |
| `python-org-docs-link` | 1,222.3 [1,222.3–2,678.6] / 1,461.4 [1,461.4–2,040.0] | 38.8 [38.3–38.8] / 38.8 [38.1–39.0] | 325.0 [204.2–507.3] / 466.1 [190.7–503.4] |
| `python-org-search` | 1,245.8 [1,245.8–1,490.7] / 1,493.5 [1,493.5–1,500.0] | 39.5 [39.2–39.7] / 39.0 [39.0–39.3] | 218.9 [216.5–508.7] / 228.2 [202.6–291.9] |
| `react-docs` | 2,857.2 [2,857.2–4,355.0] / 881.3 [881.3–906.0] | 85.0 [71.5–85.5] / 56.5 [54.0–56.9] | 146.0 [142.1–150.5] / 166.4 [138.6–170.2] |
| `rust-book-chapters` | 476.1 [476.1–589.5] / 603.6 [603.6–717.0] | 30.9 [30.7–31.3] / 29.7 [29.6–30.2] | 215.8 [174.4–221.9] / 273.0 [272.9–281.3] |
| `rust-book-search` | 2,369.4 [2,369.4–2,371.9] / 4,261.3 [4,261.3–4,271.0] | 106.3 [105.8–106.4] / 100.8 [100.3–100.9] | 209.1 [180.1–235.2] / 188.7 [177.8–189.2] |
| `rust-book-sidebar` | 297.3 [297.3–304.2] / 342.4 [342.4–353.6] | 29.6 [28.5–29.6] / 29.2 [28.8–29.2] | 246.2 [188.3–303.6] / 196.7 [192.7–301.5] |
| `rust-lang-org-home` | 87.0 [87.0–91.7] / 99.7 | 25.5 [25.3–25.6] / 26.1 [25.9–26.3] | 91.4 [87.5–91.4] / 91.4 [87.5–91.5] |
| `sqlite-org-home` | 131.1 [131.1–152.1] / 145.3 [145.3–149.1] | 25.8 [25.8–26.0] / 24.1 [23.4–25.9] | 1,224.0 [1,220.3–1,238.8] / 1,206.9 [1,187.4–1,222.7] |
| `substack-home` | 6,633.7 [6,633.7–7,548.0] / 1,676.5 [1,676.5–1,756.2] | 189.9 [186.1–190.5] / 61.6 [61.2–61.9] | 911.6 [791.3–1,625.3] / 960.6 [924.7–986.1] |
| `w3c-home-links` | 717.3 [717.3–720.8] / 896.3 [896.3–3,378.5] | 30.1 [29.7–30.4] / 31.9 | 74.6 [70.5–87.4] / 75.5 [71.2–86.8] |
| `wikipedia-article` | 1,777.5 [1,777.5–1,921.3] / 1,810.3 [1,810.3–1,930.0] | 42.4 [41.4–42.7] / 40.9 [40.8–41.0] | 367.0 [359.7–385.4] / 401.0 [398.0–408.8] |
| `wikipedia-main-random` | — (0/3) / — (0/3) | — (0/3) / — (0/3) | — (0/3) / — (0/3) |
| `wikipedia-portal` | 514.4 [514.4–515.1] / 538.1 [538.1–545.5] | 34.4 [34.1–34.9] / 33.2 [33.2–33.3] | 222.0 [218.8–224.6] / 242.1 [215.0–276.1] |
| `wikipedia-search` | 1,869.8 [1,869.8–1,964.1] / 1,949.3 [1,949.3–1,991.9] | 42.8 [42.5–43.1] / 40.4 [39.9–41.0] | 374.7 [350.0–463.5] / 368.6 [362.5–399.0] |
| `wikipedia-toc-languages` | 1,238.9 [1,238.9–1,598.0] / 1,253.3 [1,253.3–1,402.6] | 39.7 [39.7–40.7] / 38.1 [37.5–39.2] | 433.0 [383.7–467.5] / 392.2 [356.9–413.5] |
| `wiktionary-portal` | 813.4 [813.4–832.5] / 820.0 [820.0–822.7] | 38.2 [37.9–38.6] / 35.6 [35.3–35.7] | 218.5 [215.3–236.5] / 215.3 [209.8–218.1] |

### Load-test ramps

| Workload | Instructions, M | Peak RSS, MiB | Ready, ms |
|---|---:|---:|---:|
| `churn_1m` | — (0/3) / — (0/3) | — (0/3) / — (0/3) | — (0/3) / — (0/3) |
| `compm_22000` | 405.3 [405.3–405.4] / 403.3 [403.3–403.6] | 37.7 [37.5–38.2] / 37.4 [37.3–38.3] | 36.7 [36.7–36.8] / 37.2 [36.8–37.6] |
| `compm_24000` | 445.3 [445.3–451.2] / 443.2 [443.2–443.3] | 38.7 [38.6–39.6] / 38.2 [38.1–38.7] | 40.3 [40.0–40.6] / 39.6 [39.6–40.0] |
| `compm_26000` | 482.9 [482.9–483.0] / 480.8 [480.8–486.9] | 40.7 [40.2–41.3] / 39.8 [39.6–40.0] | 43.4 [42.6–44.4] / 42.6 [42.5–42.9] |
| `compm_30000` | 552.7 [552.7–558.8] / 550.6 [550.6–550.7] | 43.2 [42.9–43.6] / 42.8 [42.4–42.9] | 48.8 [48.1–49.1] / 48.8 [48.6–51.0] |
| `compm_34000` | 623.6 [623.6–624.4] / 622.3 [622.3–627.9] | 47.2 [46.7–48.4] / 46.7 [46.0–47.2] | 53.8 [53.6–54.2] / 54.8 [53.8–54.8] |
| `compm_38000` | 693.2 [693.2–701.4] / 691.1 [691.1–692.4] | 49.7 [49.7–50.0] / 48.6 [48.5–48.9] | 60.2 [59.2–60.7] / 59.5 [59.1–60.4] |
| `compm_40000` | 728.3 / 726.2 [726.2–734.3] | 50.8 [50.6–51.1] / 50.1 [49.9–50.2] | 62.2 [61.8–62.3] / 62.3 [61.9–63.4] |
| `compm_43000` | 781.2 [781.2–788.6] / 779.0 [779.0–780.5] | 52.7 [51.9–55.1] / 53.6 [53.2–54.1] | 67.1 [66.4–68.4] / 66.3 [65.8–67.8] |
| `ctor_100k` | 904.7 / 811.4 | 21.1 [21.1–21.4] / 20.7 [20.5–21.0] | 78.7 [78.1–79.4] / 72.2 [70.6–72.9] |
| `ctor_10k` | 96.1 / 85.5 | 21.2 [20.6–21.8] / 20.7 | 13.1 [12.9–13.4] / 12.1 [12.0–12.1] |
| `ctor_1k` | 15.2 / 13.0 | 21.4 [20.5–21.9] / 20.3 [20.3–20.9] | 6.3 [6.2–6.6] / 6.4 [6.2–6.5] |
| `ctor_1m` | 8,990.3 / 8,069.9 | 21.1 [21.0–21.5] / 20.2 [20.1–21.2] | 749.6 [731.3–752.5] / 686.5 [648.7–687.5] |
| `ctor_read_100k` | 1,897.6 / 1,881.6 [1,881.6–1,881.9] | 64.9 [64.5–65.0] / 32.0 [31.7–32.5] | 208.5 [207.5–209.0] / 198.0 [193.5–198.7] |
| `ctor_read_10k` | 191.6 / 194.0 | 25.9 [25.6–26.2] / 23.4 [23.1–23.9] | 24.0 [23.9–24.2] / 24.1 [23.6–24.8] |
| `ctor_read_1k` | 24.8 / 23.9 | 22.0 [21.6–22.0] / 21.2 [21.1–21.4] | 7.6 [7.2–8.0] / 7.5 [7.5–7.6] |
| `loop_100000` | 95.5 / 33.1 | 20.7 [20.1–21.2] / 20.0 [19.9–20.8] | 11.7 [11.3–12.0] / 8.1 [8.0–8.4] |
| `loop_1000000` | 902.8 / 297.7 | 20.5 [20.1–20.9] / 20.1 [19.8–20.2] | 64.7 [64.6–67.8] / 28.8 [28.1–29.9] |
| `loop_10000000` | 8,975.8 / 2,943.7 | 20.5 [20.1–20.9] / 20.4 [20.2–20.5] | 595.8 [593.0–599.0] / 259.4 [247.0–263.3] |
| `readall_fwd` | 1,489.7 [1,489.7–1,493.6] / 1,556.3 | 78.9 [78.6–79.0] / 71.8 [71.7–72.2] | 188.0 [187.1–190.2] / 203.8 [203.4–208.6] |
| `readall_rev` | 1,459.7 [1,459.7–1,459.8] / 1,502.8 [1,502.8–1,503.6] | 78.8 [78.4–79.0] / 72.4 [72.0–74.0] | 188.9 [188.3–189.0] / 201.9 [201.0–202.5] |
| `readall_spans` | 1,262.3 [1,262.3–1,266.7] / 1,318.2 [1,318.2–1,318.5] | 73.6 [73.4–73.9] / 68.2 [68.1–68.5] | 161.9 [159.4–162.3] / 170.8 [170.7–174.0] |
| `rest_13` | 1,079.6 / 1,078.2 [1,078.2–1,078.3] | 52.6 [52.4–52.9] / 52.6 [52.4–53.0] | 136.2 [130.9–137.4] / 137.5 [132.1–138.2] |
| `style_100k` | 2,159.6 [2,159.6–2,160.5] / 2,011.4 [2,011.4–2,012.0] | 32.2 [31.5–32.3] / 33.0 [32.7–33.1] | 187.4 [187.2–207.8] / 177.5 [177.2–178.5] |
| `style_10k` | 220.1 / 203.4 [203.4–203.5] | 21.7 [21.6–22.2] / 22.7 [22.5–22.7] | 23.7 [23.3–23.9] / 23.4 [23.2–23.9] |
| `style_1k` | 27.5 / 24.7 | 21.5 [21.1–21.9] / 21.1 [21.0–21.1] | 7.3 [7.3–7.4] / 7.3 [7.1–7.4] |
| `style_250k` | 5,383.5 [5,383.5–5,385.8] / 5,020.4 [5,020.4–5,021.4] | 48.3 [47.5–48.8] / 48.5 [48.4–48.8] | 454.2 [450.5–459.7] / 441.3 [438.0–450.7] |

### Typing, local loop and large object controls

| Workload | Instructions, M | Peak RSS, MiB | Ready, ms |
|---|---:|---:|---:|
| `typing` | 6,375.6 [6,375.6–6,375.7] / 7,659.5 [7,659.5–7,679.5] | 38.8 [38.6–39.1] / 31.2 [31.0–31.3] | 67.0 [67.0–68.0] / 68.0 [67.0–68.0] |
| `loop_local_10000000` | 1,985.9 / 1,463.8 | 20.6 [20.3–20.7] / 20.1 [19.8–20.4] | 113.0 [112.8–113.1] / 106.2 [104.3–106.5] |
| `object_1000` | 8.7 / 9.0 | 20.6 [20.2–21.0] / 20.3 [20.2–20.9] | 6.0 [5.7–6.5] / 6.0 [6.0–6.1] |
| `object_2000` | 11.8 / 14.4 | 20.8 [20.3–21.0] / 20.7 [20.6–20.9] | 6.1 [5.9–6.1] / 6.3 [6.2–6.7] |
| `object_4000` | 18.0 / 24.3 [24.3–24.4] | 21.4 [21.1–21.4] / 21.6 [21.2–21.6] | 6.1 [6.1–6.6] / 7.1 [7.1–7.3] |
| `object_8000` | 30.5 / 43.8 [43.8–44.0] | 22.0 [21.5–22.5] / 22.2 [22.1–22.5] | 7.3 [7.3–7.6] / 8.2 [8.0–8.5] |

### MDN request and console check

| Mode | Requests across passing runs | Requests at min CPU | Instructions, M | Peak RSS, MiB | Ready, ms |
|---|---:|---:|---:|---:|---:|
| auto | 68–75 | 68 | 2,880.3 [2,880.3–5,772.3] | 135.5 [133.5–135.8] | 178.2 [170.1–178.4] |
| new | 73–74 | 73 | 4,982.1 [4,982.1–4,988.3] | 67.2 [67.2–67.3] | 175.3 [169.8–179.5] |

All 12 runs passed the original assertions. The js2 samples made 73–74 requests and had empty consoles. Every js2 sample fetched the Array compatibility JSON with HTTP 200. OLD logged two Glean pings in each run. Five OLD samples logged two telemetry connection failures. No sample logged a `ReferenceError` or an unhandled rejection. The missing-`Uint8Array` startup cutoff from #907 did not recur. [#907 MDN diagnosis](2026-11-22-mdn-js2-check.md)

The minimum OLD instruction sample made 68 requests. It missed the compatibility JSON and four SVGs present in the minimum js2 sample. The totals retain that minimum under the #906 rule. The two 75-request OLD instruction samples took 5,764.1 M and 5,772.3 M instructions. The lower of those matches the minimum js2 sample on 73 responses by URL, status and byte count. The two extra OLD requests were failed telemetry POSTs with zero bytes. This closer pair took 5,764.1 M versus 4,982.1 M instructions, or 0.86×. The live replies give evidence that the application ran past the prior cutoff. Exact execution parity remains unverified.

### Five journeys requested for follow-up

Each cell gives OLD `auto` → js2. The public table gives spreads for the rerun.

| Journey | #906 instructions, M | Rerun instructions, M | Rerun peak RSS, MiB | Rerun ready, ms |
|---|---:|---:|---:|---:|
| `rust-book-search` | 2,371.6 → 4,257.2 | 2,369.4 → 4,261.3 | 106.3 → 100.8 | 209.1 → 188.7 |
| `devto-home` | 5,721.9 → 7,304.2 | 4,974.1 → 6,110.4 | 95.3 → 113.9 | 143.9 → 143.5 |
| `python-docs-search-results` | 1,617.5 → 2,781.2 | 1,785.0 → 1,728.7 | 91.3 → 93.6 | 87.9 → 96.1 |
| `python-org-search` | 1,251.4 → 1,609.2 | 1,245.8 → 1,493.5 | 39.5 → 39.0 | 218.9 → 228.2 |
| `rust-lang-org-home` | 87.1 → 107.1 | 87.0 → 99.7 | 25.5 → 26.1 | 91.4 → 91.4 |

On the minimum samples, `rust-book-search`, `devto-home`, `python-org-search` and `rust-lang-org-home` had the same request counts and total bytes in both modes. Each still used more instructions on js2. The minimum samples for `python-docs-search-results` had 57 requests on OLD and 48 on js2. The CPU reversal on that row includes a traffic change.

### Incomplete browser rows

| Journey | Mode | CPU / RSS / ready samples | Failure |
|---|---:|---:|---:|
| `wikipedia-main-random` | auto | 0/0/0 | the script gave false: document.querySelectorAll('#mp-tfa p a').length > 0 (time limit 40000 ms) |
| `wikipedia-main-random` | new | 0/0/0 | the script gave false: document.querySelectorAll('#mp-tfa p a').length > 0 (time limit 40000 ms) (network failures: https://intake-analytics.wikimedia.org:443/v1/events?hasty=true: connect refused: Connection refused (os error 111); https://intake-analytics.wikimedia.org:443/v1/events?hasty=true: connect refused: Connection refused (os error 111)) |
| `churn_1m` | auto | 0/0/0 | the script gave false: window.__done === 1 (the page stopped: Limit("Host(\"dom-nodes\")")) (the page is idle: no task is queued, so nothing can change it) |
| `churn_1m` | new | 0/0/0 | the script gave false: window.__done === 1 (the page is idle: no task is queued, so nothing can change it) |

GitHub Docs returned HTTP 429 during the first sweep. One recheck supplied the last instruction and memory samples for each mode. Wikipedia main-page and churn failures remain outside the paired totals.

## #906 vs now

Browser rows use cases with complete pairs in both reports. The count is in parentheses.

| js2 metric | #906 | Rerun | Rerun / #906 |
|---|---:|---:|---:|
| Standalone instructions, G | 3.51 | 2.64 | 0.75× |
| Standalone sum of peak RSS, MiB | 124.72 | 105.51 | 0.85× |
| Empty instructions, M | 0.72 | 0.72 | 0.99× |
| Empty peak RSS, MiB | 4.93 | 5.60 | 1.14× |
| React parse instructions, M | 63.50 | 63.49 | 1.00× |
| React parse peak RSS, MiB | 6.46 | 7.16 | 1.11× |
| React eager compile instructions, M | 323.21 | 281.83 | 0.87× |
| React eager compile peak RSS, MiB | 7.43 | 7.99 | 1.08× |
| Public instructions, G (42) | 34.85 | 37.83 | 1.09× |
| Public sum of peak RSS, MiB (42) | 1,500.35 | 1,578.52 | 1.05× |
| Public sum of ready time, s (42) | 21.33 | 21.18 | 0.99× |
| Public excluding MDN instructions, G (41) | 34.44 | 32.85 | 0.95× |
| Public excluding MDN sum of peak RSS, MiB (41) | 1,470.32 | 1,511.32 | 1.03× |
| Public excluding MDN sum of ready time, s (41) | 20.63 | 21.01 | 1.02× |
| Ramps instructions, G (26) | 32.09 | 31.77 | 0.99× |
| Ramps sum of peak RSS, MiB (26) | 1,020.18 | 966.73 | 0.95× |
| Ramps sum of ready time, s (26) | 2.98 | 3.08 | 1.03× |
| Typing instructions, G (1) | 7.29 | 7.66 | 1.05× |
| Typing sum of peak RSS, MiB (1) | 31.70 | 31.21 | 0.98× |
| Typing sum of ready time, s (1) | 0.07 | 0.07 | 1.01× |
| Other controls instructions, G (5) | 1.52 | 1.56 | 1.03× |
| Other controls sum of peak RSS, MiB (5) | 101.54 | 104.80 | 1.03× |
| Other controls sum of ready time, s (5) | 0.12 | 0.13 | 1.08× |

## What changed

The frozen main includes indexed atoms (#908), short integer concatenation (#909), the adaptive GC trigger (#911), array push and key reuse (#915, #917, #921), compile metadata (#916, #918), global cache miss handling (#922), typed arrays (#863), dynamic scope and Proxy (#889), and string normalization (#901). The rerun measures the combined tree. It gives no per-PR attribution.

## Evidence and limits

Raw samples, logs, fixture hashes and `summary.json` are under `/data/bare-tmp/r22/bench/` on `archMachine`. The JSON stores each browser sample's one-minute load average, all MDN console records, and the values and spreads used in the tables.

Untested work includes Windows, windowed runs, idle-machine latency, a full compatibility suite, a QuickJS browser build, and a new recorded-response MDN replay.
