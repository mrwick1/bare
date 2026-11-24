# Browser journeys after the js2 fixes

## Summary

Both engines ran at `ab09c54b`. The baseline is [#923 at `3c46337e`](2026-11-22-engine-benchmark-rerun.md#b-browser-journeys). Each result gives OLD `auto` → js2 `new`. Each ratio is js2 divided by OLD.

| Group | Paired now / #923 | Instructions now, G | Instructions #923, G | Sum of peak RSS now, MiB | Sum of peak RSS #923, MiB |
|---|---:|---:|---:|---:|---:|
| Public | 43/44 / 43/44 | 43.24 → 36.00 (0.83×) | 40.55 → 39.59 (0.98×) | 1,935.61 → 1,563.27 (0.81×) | 1,900.58 → 1,653.17 (0.87×) |
| Public excluding MDN | 42/43 / 42/43 | 40.33 → 31.20 (0.77×) | 37.67 → 34.61 (0.92×) | 1,801.47 → 1,496.12 (0.83×) | 1,765.07 → 1,585.97 (0.90×) |
| Ramps | 26/27 / 26/27 | 39.89 → 31.77 (0.80×) | 39.89 → 31.77 (0.80×) | 1,024.02 → 952.25 (0.93×) | 1,027.62 → 966.73 (0.94×) |
| Typing | 1/1 / 1/1 | 6.38 → 5.68 (0.89×) | 6.38 → 7.66 (1.20×) | 38.51 → 30.10 (0.78×) | 38.82 → 31.21 (0.80×) |
| Other controls | 5/5 / 5/5 | 2.05 → 1.56 (0.76×) | 2.05 → 1.56 (0.76×) | 104.88 → 102.95 (0.98×) | 105.21 → 104.80 (1.00×) |
| All paired rows | 75/77 / 75/77 | 91.56 → 75.00 (0.82×) | 88.87 → 80.57 (0.91×) | 3,103.02 → 2,648.58 (0.85×) | 3,072.23 → 2,755.92 (0.90×) |

| Group | Median CPU ratio / wins now | Median CPU ratio / wins #923 | Median RSS ratio / wins now | Median RSS ratio / wins #923 | Sum of ready now, s | Sum of ready #923, s |
|---|---:|---:|---:|---:|---:|---:|
| Public | 0.99× / 26/43 | 1.01× / 18/43 | 0.95× / 37/43 | 0.98× / 33/43 | 21.63 → 21.69 (1.00×) | 20.68 → 21.28 (1.03×) |
| Public excluding MDN | 0.99× / 26/42 | 1.01× / 18/42 | 0.96× / 36/42 | 0.98× / 32/42 | 20.93 → 20.98 (1.00×) | 20.50 → 21.10 (1.03×) |
| Ramps | 0.99× / 22/26 | 0.99× / 22/26 | 0.96× / 23/26 | 0.98× / 21/26 | 3.51 → 2.98 (0.85×) | 3.52 → 3.08 (0.87×) |
| Typing | 0.89× / 1/1 | 1.20× / 0/1 | 0.78× / 1/1 | 0.80× / 1/1 | 0.07 → 0.05 (0.75×) | 0.07 → 0.07 (1.01×) |
| Other controls | 1.24× / 1/5 | 1.23× / 1/5 | 0.98× / 3/5 | 1.00× / 3/5 | 0.14 → 0.13 (0.95×) | 0.14 → 0.13 (0.97×) |
| All paired rows | 0.99× / 50/75 | 1.00× / 41/75 | 0.96× / 64/75 | 0.98× / 58/75 | 25.35 → 24.85 (0.98×) | 24.41 → 24.55 (1.01×) |

The similar-traffic subset has 32 public pairs and uses 25.83 → 22.21 (0.86×) G instructions. #923 had 34 such pairs and used 28.05 → 25.80 (0.92×) G. The minimum samples in each pair have equal request, console error and page diagnostic counts, with bytes within 1%.

## Method

archMachine ran Linux on an AMD Ryzen 9 5900HX with Rust 1.96.0 and perf 7.3.1. The build used Cargo’s release-derived `profiling` profile with debug symbols and frame pointers, as in #923. Plain Cargo built it with four jobs and the default link order.

```sh
CARGO_TARGET_DIR=/data/bare-tmp/r22/rerun2/target CARGO_BUILD_JOBS=4 \
  RUSTFLAGS='-C force-frame-pointers=yes' \
  cargo build --locked --profile profiling -p bare-drive --bin bare-drive -p bare --example type_load
```

The copied #923 harness ran the 44 public journeys headless with live replies. The step tree was `36a003426642bb137fb5c4b4e3798cbebfa6e6b7`. All 32 loopback HTML fixtures passed the #923 SHA-256 manifest. React used the same bundle and typed 50 characters into `#a1`.

Both modes used the same binary, `BARE_SEED=1` and `LC_ALL=C`. The harness rotated modes within each case. The instruction result is the minimum of three passing `perf stat -e instructions:u` samples with at least 99% counter coverage and `GLIBC_TUNABLES=glibc.malloc.tcache_count=65535`. Peak RSS is the median of three separate passing samples with the default allocator. Each uses `cp --reflink=never` and the `/data/claude-tmp/qjs-base/h/rss` wrapper. A shell restores the output streams and uses `exec` to run the binary copy in the measured process.

Ready time is the median of those RSS samples. It ends at the first action after `open` for public journeys, the completion check for ramps and the reported load time for typing. Use ready times as observations on a shared machine.

One-minute load ranged from 0.21 to 2.36 before samples and from 0.21 to 2.17 after samples. The sweep boundaries use `uptime`.

```text
before public and typing 19:25:48 up 9 days,  8:53,  3 users,  load average: 2.36, 2.44, 1.70
after public and typing 19:46:20 up 9 days,  9:13,  3 users,  load average: 0.38, 0.42, 0.75
before ramps and controls 19:46:20 up 9 days,  9:13,  3 users,  load average: 0.38, 0.42, 0.75
after ramps and controls 19:47:33 up 9 days,  9:15,  3 users,  load average: 1.68, 0.78, 0.85
before recheck 19:47:37 up 9 days,  9:15,  3 users,  load average: 1.54, 0.76, 0.84
after recheck 19:47:40 up 9 days,  9:15,  3 users,  load average: 1.50, 0.77, 0.84
```

Brackets give the min–max spread at the shown precision. Totals use rows with three passing samples per metric in both modes. A win means js2 used less of the metric. The public table uses the original 44 journeys. The ramps and controls use loopback fixtures.

## Public journeys

| Workload | Instructions, M | Peak RSS, MiB | Ready, ms | CPU ratio now | CPU ratio #923 |
|---|---:|---:|---:|---:|---:|
| `archlinux-org-home` | 80.5 [80.5–81.2] / 78.2 [78.2–78.4] | 23.9 [23.3–23.9] / 23.0 [22.9–23.3] | 739.6 [719.3–745.2] / 733.6 [731.2–758.2] | 0.97× | 0.96× |
| `blog-rust-lang-home` | 154.9 [154.9–156.1] / 156.8 [156.8–173.3] | 25.2 [25.2–25.3] / 24.1 [23.9–24.1] | 92.5 [91.7–92.8] / 96.9 [92.8–97.1] | 1.01× | 1.01× |
| `cnn-lite-front` | 70.8 [70.8–72.2] / 72.0 [72.0–72.1] | 24.2 [24.1–24.5] / 23.6 [23.4–23.7] | 190.5 [183.1–198.6] / 185.6 [184.8–198.0] | 1.02× | 1.01× |
| `craigslist-home` | 376.6 [376.6–2,350.3] / 2,025.0 [2,025.0–2,028.8] | 93.5 [93.2–93.6] / 31.8 [31.5–58.1] | 4,404.2 [4,135.4–5,787.4] / 4,448.1 [4,425.6–5,264.2] | 5.38× | 1.29× |
| `crates-io-home` | 84.6 [84.6–89.7] / 94.6 [94.6–99.5] | 25.7 [25.6–25.8] / 26.0 [25.4–26.1] | 366.4 [364.1–913.2] / 361.5 [345.7–375.5] | 1.12× | 1.10× |
| `debian-org-home` | 125.9 [125.9–128.1] / 125.4 [125.4–129.8] | 23.7 [23.6–23.9] / 22.4 [22.3–23.0] | 836.7 [798.5–871.2] / 874.0 [827.2–917.1] | 1.00× | 0.97× |
| `devto-home` | 5,554.7 [5,554.7–6,851.0] / 5,399.5 [5,399.5–6,180.2] | 98.0 [96.7–114.7] / 106.7 [98.0–106.9] | 147.7 [147.6–151.6] / 149.0 [148.4–152.4] | 0.97× | 1.23× |
| `docs-rs-home` | 69.5 [69.5–69.8] / 68.4 [68.4–68.5] | 24.2 [23.8–24.7] / 23.1 [22.8–23.6] | 103.2 [103.0–111.5] / 107.5 [103.6–113.8] | 0.98× | 0.98× |
| `example-learn-more` | 134.2 [134.2–145.1] / 128.0 [128.0–128.2] | 25.4 [25.3–25.6] / 24.4 [24.3–24.6] | 112.9 [108.6–227.4] / 112.3 [109.8–489.7] | 0.95× | 1.11× |
| `example-page` | 27.5 / 26.6 | 22.5 [22.3–22.5] / 21.0 [21.0–21.3] | 111.8 [87.8–116.5] / 100.7 [96.2–112.4] | 0.97× | 0.96× |
| `github-docs-home` | 2,006.7 [2,006.7–2,719.4] / 1,410.0 [1,410.0–1,847.8] | 86.3 [63.2–88.1] / 67.1 [64.9–67.4] | 91.8 [91.7–100.8] / 91.5 [91.3–96.3] | 0.70× | 0.87× |
| `go-dev-home` | 1,782.7 [1,782.7–2,008.8] / 995.6 [995.6–1,186.6] | 56.4 [55.2–56.5] / 41.0 [40.8–41.2] | 559.3 [532.3–561.2] / 515.5 [496.7–535.9] | 0.56× | 0.65× |
| `hn-comments-back` | 257.7 [257.7–257.9] / 254.6 [254.6–255.2] | 24.9 [24.4–25.6] / 24.4 [24.3–24.5] | 694.8 [682.6–701.4] / 684.8 [684.3–688.9] | 0.99× | 0.99× |
| `hn-front` | 787.9 [787.9–793.1] / 790.7 [790.7–791.1] | 26.1 [25.9–27.0] / 25.5 [25.5–25.8] | 685.0 [679.6–696.1] / 684.1 [674.3–697.4] | 1.00× | 0.95× |
| `hn-new-past` | 205.6 [205.6–207.8] / 203.5 [203.5–204.4] | 22.9 [22.9–23.1] / 22.5 [22.0–22.6] | 1,140.5 [973.8–1,203.6] / 924.4 [909.3–925.6] | 0.99× | 0.99× |
| `httpbin-cookies` | 36.8 / 33.6 [33.6–33.7] | 22.0 [21.4–22.1] / 21.5 [21.1–21.6] | 923.1 [902.0–987.5] / 954.8 [932.3–963.8] | 0.91× | 0.91× |
| `httpbin-form-fill` | 46.2 [46.2–46.3] / 45.2 | 22.6 [22.4–22.9] / 21.8 [21.6–22.1] | 680.5 [678.7–686.5] / 719.7 [695.4–724.4] | 0.98× | 0.98× |
| `httpbin-form-submit` | 58.3 [58.3–58.9] / 64.4 [64.4–65.8] | 22.6 [22.5–22.8] / 22.0 [21.8–22.2] | 717.5 [717.0–728.2] / 702.0 [696.2–724.4] | 1.11× | 1.10× |
| `kernel-org-home` | 33.1 [33.1–33.2] / 31.1 [31.1–31.2] | 21.9 [21.8–22.1] / 20.8 [20.4–21.0] | 166.1 [165.8–169.9] / 173.0 [171.2–177.9] | 0.94× | 0.93× |
| `lobsters-front` | 139.2 [139.2–141.2] / 137.7 [137.7–138.0] | 24.4 [23.7–24.4] / 23.5 [23.4–23.6] | 775.9 [759.0–1,070.0] / 797.3 [768.1–808.8] | 0.99× | 0.96× |
| `mdn-reference` | 2,906.8 [2,906.8–5,793.7] / 4,802.6 [4,802.6–4,813.4] | 134.1 [133.8–134.4] / 67.2 [66.9–67.6] | 702.6 [696.4–760.9] / 706.8 [693.4–711.8] | 1.65× | 1.73× |
| `nginx-org-home` | 39.2 [39.2–39.5] / 37.7 [37.7–40.8] | 22.7 [22.1–22.9] / 21.4 [21.2–21.5] | 782.3 [781.8–791.6] / 722.3 [704.8–736.5] | 0.96× | 0.96× |
| `nodejs-org-home` | 1,306.0 [1,306.0–1,478.5] / 852.1 [852.1–916.6] | 56.3 [53.6–57.1] / 40.6 [40.5–40.8] | 113.7 [93.7–126.2] / 110.4 [106.2–118.7] | 0.65× | 1.26× |
| `npr-text-story` | 33.1 [33.1–33.4] / 30.0 [30.0–39.5] | 22.4 [22.2–22.6] / 21.4 [21.4–21.5] | 213.9 [201.3–220.7] / 192.7 [188.3–206.1] | 0.91× | 0.90× |
| `paulgraham-home` | 59.7 [59.7–68.8] / 62.6 [62.6–65.2] | 23.4 [22.5–23.5] / 22.8 [22.8–23.0] | 733.8 [731.8–779.9] / 755.1 [749.2–767.5] | 1.05× | 1.11× |
| `python-docs-search-results` | 1,785.1 [1,785.1–2,456.8] / 1,659.3 [1,659.3–2,191.8] | 87.8 [85.2–91.7] / 87.8 [87.0–88.4] | 98.3 [91.3–100.8] / 91.9 [87.0–92.4] | 0.93× | 0.97× |
| `python-docs-search` | 1,106.0 [1,106.0–1,107.2] / 1,497.8 [1,497.8–1,497.9] | 76.3 [76.1–76.4] / 77.0 [76.8–77.2] | 96.2 [87.6–96.7] / 91.6 [91.2–95.9] | 1.35× | 1.43× |
| `python-docs-tutorial` | 253.9 [253.9–258.0] / 256.6 [256.6–265.0] | 25.1 [25.1–25.2] / 24.0 [23.7–24.4] | 164.4 [154.8–268.0] / 273.6 [158.8–336.3] | 1.01× | 1.01× |
| `python-org-docs-link` | 1,059.9 [1,059.9–1,224.2] / 1,223.5 [1,223.5–1,237.3] | 38.5 [38.2–38.8] / 36.5 [35.8–36.5] | 207.8 [202.9–224.4] / 211.4 [202.8–229.0] | 1.15× | 1.20× |
| `python-org-search` | 1,248.6 [1,248.6–1,532.9] / 1,274.0 [1,274.0–1,277.1] | 38.6 [38.5–39.9] / 36.7 [36.7–37.3] | 216.0 [211.6–224.2] / 210.8 [207.8–212.2] | 1.02× | 1.20× |
| `react-docs` | 5,199.6 [5,199.6–14,496.3] / 596.5 [596.5–639.3] | 88.5 [88.4–89.0] / 49.2 [49.0–50.1] | 163.3 [158.9–164.3] / 169.0 [166.9–183.4] | 0.11× | 0.31× |
| `rust-book-chapters` | 475.5 [475.5–532.4] / 462.4 [462.4–528.7] | 30.7 [30.5–31.0] / 27.5 [27.3–27.7] | 203.0 [200.3–203.6] / 180.8 [179.3–186.4] | 0.97× | 1.27× |
| `rust-book-search` | 2,371.2 [2,371.2–2,379.9] / 1,866.5 [1,866.5–1,876.4] | 106.2 [106.0–106.4] / 86.7 [86.6–87.1] | 179.1 [174.9–193.6] / 199.6 [197.7–200.4] | 0.79× | 1.80× |
| `rust-book-sidebar` | 302.5 [302.5–308.3] / 281.7 [281.7–284.9] | 29.2 [28.1–29.4] / 26.3 [26.3–26.7] | 200.5 [188.2–201.2] / 187.3 [180.3–189.8] | 0.93× | 1.15× |
| `rust-lang-org-home` | 80.3 [80.3–88.0] / 88.9 [88.9–89.1] | 25.0 [24.8–25.6] / 24.3 [23.6–24.7] | 92.0 [91.3–95.1] / 92.2 [91.6–92.4] | 1.11× | 1.15× |
| `sqlite-org-home` | 150.3 [150.3–151.5] / 128.4 [128.4–149.2] | 23.8 [23.3–26.2] / 25.1 [25.0–25.3] | 1,212.1 [1,176.2–1,237.6] / 1,219.7 [1,198.7–1,235.3] | 0.85× | 1.11× |
| `substack-home` | 5,923.7 [5,923.7–5,975.7] / 1,657.5 [1,657.5–1,670.0] | 161.9 [158.1–172.4] / 61.2 [61.0–61.3] | 985.1 [930.5–1,070.7] / 952.6 [935.3–1,488.2] | 0.28× | 0.25× |
| `w3c-home-links` | 723.8 [723.8–730.5] / 907.6 [907.6–912.4] | 30.1 [29.8–30.6] / 30.7 [30.7–30.8] | 79.7 [79.4–91.6] / 87.6 [70.6–115.4] | 1.25× | 1.25× |
| `wikipedia-article` | 1,776.6 [1,776.6–1,796.1] / 1,775.9 [1,775.9–1,789.5] | 42.6 [42.1–42.9] / 38.8 [38.5–38.8] | 388.7 [377.5–399.0] / 402.7 [383.4–404.1] | 1.00× | 1.02× |
| `wikipedia-main-random` | — (0/3) / — (0/3) | — (0/3) / — (0/3) | — (0/3) / — (0/3) | — | — |
| `wikipedia-portal` | 512.5 [512.5–514.2] / 515.6 [515.6–519.3] | 34.0 [33.7–34.1] / 31.6 [31.6–31.7] | 241.6 [226.0–296.9] / 283.6 [233.1–305.0] | 1.01× | 1.05× |
| `wikipedia-search` | 1,851.0 [1,851.0–1,861.8] / 1,862.3 [1,862.3–2,663.6] | 42.8 [42.6–43.1] / 39.3 [38.5–39.3] | 407.3 [388.3–415.7] / 444.5 [386.7–490.3] | 1.01× | 1.04× |
| `wikipedia-toc-languages` | 1,221.8 [1,221.8–1,248.1] / 1,222.5 [1,222.5–1,355.5] | 41.0 [40.2–41.3] / 36.6 [36.3–37.1] | 389.7 [382.7–442.4] / 390.2 [387.4–392.2] | 1.00× | 1.01× |
| `wiktionary-portal` | 816.9 [816.9–818.5] / 795.1 [795.1–813.4] | 38.0 [37.8–38.5] / 34.1 [34.0–34.3] | 220.6 [219.2–223.3] / 295.2 [214.0–1,320.7] | 0.97× | 1.01× |

## Load-test ramps

| Workload | Instructions, M | Peak RSS, MiB | Ready, ms | CPU ratio now | CPU ratio #923 |
|---|---:|---:|---:|---:|---:|
| `churn_1m` | — (0/3) / — (0/3) | — (0/3) / — (0/3) | — (0/3) / — (0/3) | — | — |
| `compm_22000` | 405.2 / 403.4 [403.4–404.8] | 38.8 [38.3–39.4] / 37.0 [36.3–37.2] | 37.1 [37.0–37.6] / 37.5 [37.1–37.7] | 1.00× | 0.99× |
| `compm_24000` | 445.3 [445.3–446.7] / 443.4 | 39.2 [38.8–39.6] / 38.1 [37.8–38.9] | 39.9 [39.6–40.9] / 39.9 [39.7–40.2] | 1.00× | 1.00× |
| `compm_26000` | 482.8 / 480.9 [480.9–482.6] | 40.3 [40.2–40.5] / 39.3 [39.2–40.1] | 43.5 [43.2–43.5] / 43.2 [42.8–43.6] | 1.00× | 1.00× |
| `compm_30000` | 552.6 [552.6–559.9] / 550.7 [550.7–550.8] | 43.0 [42.8–45.6] / 42.1 [42.1–44.2] | 48.9 [47.8–49.3] / 48.4 [48.2–50.4] | 1.00× | 1.00× |
| `compm_34000` | 623.5 [623.5–624.1] / 621.7 | 46.6 [46.6–47.2] / 46.3 [45.7–46.6] | 54.6 [54.4–55.1] / 54.8 [54.4–58.1] | 1.00× | 1.00× |
| `compm_38000` | 693.1 [693.1–699.7] / 691.2 [691.2–692.6] | 49.3 [49.2–49.5] / 47.8 [47.7–48.4] | 59.8 [59.3–60.4] / 59.4 [59.4–62.8] | 1.00× | 1.00× |
| `compm_40000` | 728.2 [728.2–728.3] / 726.4 [726.4–734.4] | 50.3 [50.0–50.6] / 49.6 [49.6–50.1] | 63.1 [62.5–63.1] / 62.6 [62.6–63.8] | 1.00× | 1.00× |
| `compm_43000` | 781.0 [781.0–788.4] / 779.2 | 52.2 [51.7–52.4] / 51.3 [51.2–51.5] | 67.5 [66.6–67.6] / 67.2 [66.2–67.4] | 1.00× | 1.00× |
| `ctor_100k` | 904.6 / 811.5 | 20.9 [20.8–21.6] / 20.0 [19.7–20.0] | 77.0 [76.6–78.2] / 69.4 [69.0–70.9] | 0.90× | 0.90× |
| `ctor_10k` | 96.0 / 85.6 [85.6–85.7] | 20.8 [20.8–20.9] / 19.8 [19.7–20.0] | 12.7 [12.5–13.0] / 12.2 [11.9–12.3] | 0.89× | 0.89× |
| `ctor_1k` | 15.1 / 13.0 | 21.0 [20.8–21.1] / 19.8 [19.7–19.8] | 6.4 [6.2–6.7] / 6.1 [5.9–6.3] | 0.86× | 0.85× |
| `ctor_1m` | 8,990.2 / 8,070.0 | 21.0 [20.5–21.4] / 20.0 [19.8–20.0] | 718.1 [717.8–740.5] / 646.7 [634.9–653.0] | 0.90× | 0.90× |
| `ctor_read_100k` | 1,897.5 / 1,881.6 [1,881.6–1,881.9] | 65.0 [64.1–65.2] / 32.0 [31.9–32.1] | 202.1 [201.4–205.9] / 189.7 [189.6–193.1] | 0.99× | 0.99× |
| `ctor_read_10k` | 191.5 / 194.0 | 25.8 [25.3–26.2] / 22.9 [22.6–23.1] | 23.8 [23.4–24.0] / 23.6 [23.2–23.6] | 1.01× | 1.01× |
| `ctor_read_1k` | 24.7 / 24.0 | 21.5 [21.0–22.1] / 20.8 [20.7–21.0] | 7.6 [7.3–7.6] / 7.3 [7.3–7.6] | 0.97× | 0.97× |
| `loop_100000` | 95.4 / 33.1 | 20.6 [20.5–20.7] / 19.8 [19.5–19.9] | 11.7 [11.4–11.8] / 8.0 [7.8–8.0] | 0.35× | 0.35× |
| `loop_1000000` | 902.7 / 297.8 | 20.7 [20.4–21.0] / 19.7 [19.4–19.8] | 65.7 [65.3–70.5] / 26.8 [26.4–27.6] | 0.33× | 0.33× |
| `loop_10000000` | 8,975.7 / 2,943.8 | 20.8 [20.4–21.1] / 19.6 [19.5–19.7] | 608.1 [607.2–608.5] / 202.3 [195.7–223.0] | 0.33× | 0.33× |
| `readall_fwd` | 1,489.6 [1,489.6–1,493.1] / 1,556.5 [1,556.5–1,556.8] | 78.3 [78.3–78.5] / 71.7 [71.1–71.9] | 205.6 [200.1–212.6] / 219.9 [211.0–224.8] | 1.04× | 1.04× |
| `readall_rev` | 1,459.6 [1,459.6–1,460.7] / 1,503.0 [1,503.0–1,506.1] | 78.2 [78.2–79.3] / 71.8 [71.7–72.1] | 198.5 [184.4–206.0] / 209.6 [202.6–211.4] | 1.03× | 1.03× |
| `readall_spans` | 1,262.2 [1,262.2–1,265.2] / 1,318.2 [1,318.2–1,318.3] | 73.5 [73.4–73.5] / 67.9 [67.7–68.0] | 158.5 [157.7–158.6] / 169.1 [166.2–175.6] | 1.04× | 1.04× |
| `rest_13` | 1,079.5 / 1,078.3 [1,078.3–1,088.4] | 52.6 [52.5–52.7] / 51.7 [51.4–51.8] | 135.8 [132.7–137.4] / 134.7 [133.9–134.8] | 1.00× | 1.00× |
| `style_100k` | 2,160.9 [2,160.9–2,161.8] / 2,011.1 [2,011.1–2,013.5] | 32.1 [31.8–32.4] / 32.2 [32.0–32.6] | 183.0 [182.0–185.2] / 172.5 [171.6–172.6] | 0.93× | 0.93× |
| `style_10k` | 220.0 [220.0–220.1] / 203.5 [203.5–203.7] | 21.6 [21.3–21.7] / 22.7 [22.4–22.8] | 24.0 [23.5–24.3] / 22.4 [22.1–22.4] | 0.92× | 0.92× |
| `style_1k` | 27.4 / 24.7 | 21.8 [21.0–21.8] / 20.3 [19.8–20.4] | 7.3 [7.2–7.3] / 7.6 [7.1–7.8] | 0.90× | 0.90× |
| `style_250k` | 5,386.9 [5,386.9–5,389.2] / 5,020.9 [5,020.9–5,025.6] | 48.2 [47.6–48.4] / 48.3 [48.2–48.4] | 454.6 [448.8–463.9] / 434.2 [423.1–434.3] | 0.93× | 0.93× |

## React typing and other controls

| Workload | Instructions, M | Peak RSS, MiB | Ready, ms | CPU ratio now | CPU ratio #923 |
|---|---:|---:|---:|---:|---:|
| `typing` | 6,375.4 [6,375.4–6,375.5] / 5,680.7 [5,680.7–5,681.2] | 38.5 [38.5–39.2] / 30.1 [29.8–30.2] | 68.0 [67.0–68.0] / 51.0 | 0.89× | 1.20× |
| `loop_local_10000000` | 1,985.8 / 1,463.8 | 20.6 [20.4–21.0] / 19.4 [19.3–19.5] | 115.0 [114.7–115.7] / 104.9 [104.7–105.3] | 0.74× | 0.74× |
| `object_1000` | 8.6 / 9.1 | 20.2 [20.1–20.3] / 19.7 [19.7–19.9] | 5.4 [5.3–5.8] / 5.7 [5.6–6.1] | 1.05× | 1.04× |
| `object_2000` | 11.7 / 14.4 [14.4–14.5] | 20.7 [20.6–20.8] / 20.9 [20.3–20.9] | 5.7 [5.6–5.7] / 6.1 [6.0–6.4] | 1.24× | 1.23× |
| `object_4000` | 17.9 / 24.3 | 21.3 [21.0–21.6] / 20.9 [20.7–20.9] | 6.1 [6.1–6.3] / 6.8 [6.6–6.9] | 1.36× | 1.35× |
| `object_8000` | 30.4 / 43.9 | 22.0 [22.0–22.9] / 22.1 [22.1–22.7] | 7.0 [6.9–7.0] / 8.3 [8.2–8.3] | 1.44× | 1.44× |

Observed React typing time, median and range in ms from the memory runs. 771 [771–788] → 535 [534–535] on this run, and 788 [771–823] → 686 [684–702] in #923.

## Five highest public instruction ratios

| Journey | CPU ratio now | CPU ratio #923 | OLD minimum sample | js2 minimum sample |
|---|---:|---:|---:|---:|
| `craigslist-home` | 5.38× | 1.29× | 3 replies, 9,021 bytes | 3 replies, 9,021 bytes |
| `mdn-reference` | 1.65× | 1.73× | 70 replies, 189,705 bytes | 73 replies, 195,763 bytes |
| `python-docs-search` | 1.35× | 1.43× | 40 replies, 42,122 bytes | 40 replies, 42,122 bytes |
| `w3c-home-links` | 1.25× | 1.25× | 7 replies, 83,582 bytes | 7 replies, 83,582 bytes |
| `python-org-docs-link` | 1.15× | 1.20× | 18 replies, 331,692 bytes | 16 replies, 331,964 bytes |

Public servers can return different content and load different scripts. Read the reply counts and byte totals with the ratios.

Craigslist OLD samples used 376.6, 2,350.3, 2,350.3 M instructions. The js2 samples used 2,025.0–2,028.8 M. Both minima had the same reply count and byte total. The minimum rule retains the short OLD run. The cause of the spread remains unverified.

### MDN reply check

The minimum OLD sample missed 5 replies that the minimum js2 sample received. It missed the Array compatibility JSON. The lowest OLD sample with the same 73 successful replies as js2 used 5,790.2 M instructions. The js2 sample used 4,802.6 M (0.83×). The OLD sample also had two failed telemetry requests. The aggregate retains the original minimum for each mode.

### Instruction samples for the two highest ratios

One passing profile per mode and journey uses `perf record -e instructions:u -c 500009 --call-graph fp --user-callchains`. The closest classified frame owns the sample. A collector frame takes priority. Sample skid and partial stacks limit the precision. Each cell estimates M instructions.

| Subsystem | `craigslist-home` OLD → js2 | `mdn-reference` OLD → js2 |
|---|---:|---:|
| Network | 119.00 → 100.00 | 51.00 → 54.50 |
| Parse and compile | 299.01 → 323.51 | 491.01 → 442.51 |
| Style | 79.50 → 0.00 | 398.01 → 301.01 |
| Layout | 82.50 → 0.00 | 363.01 → 358.01 |
| Paint | 0.00 → 0.00 | 0.00 → 0.00 |
| JS run | 1,591.53 → 18.50 | 1,072.52 → 574.51 |
| GC | 76.00 → 2.50 | 30.00 → 5.50 |
| Other | 95.50 → 22.50 | 299.01 → 339.51 |
| Total | 2,343.04 → 467.01 | 2,704.55 → 2,075.54 |
| Sample count | 4,686 → 934 | 5,409 → 4,151 |
| Replies | 3 → 3 | 70 → 69 |
| Bytes | 9,021 → 9,021 | 189,705 → 190,944 |

The profile runs sampled less work on js2 for both journeys. Craigslist kept the same reply count and byte total but sampled much less JS execution on js2. MDN received different replies during profiling. These profiles describe those runs. The ranking uses the three instruction-counter runs.

## Failures

| Journey | Mode | CPU / RSS / ready samples | Failure |
|---|---:|---:|---:|
| `wikipedia-main-random` | auto | 0/0/0 | The `#mp-tfa p a` assertion timed out after 40 s. |
| `wikipedia-main-random` | new | 0/0/0 | The `#mp-tfa p a` assertion timed out after 40 s. Wikimedia analytics connections failed. One run also logged a Wikimedia authentication DNS error. |
| `churn_1m` | auto | 0/0/0 | The page hit the `dom-nodes` limit before it set `window.__done`. |
| `churn_1m` | new | 0/0/0 | The page went idle before it set `window.__done`. |

These rows supplied all required samples after retries.

| Journey | Mode | Failed attempts | Failure |
|---|---:|---:|---:|
| `github-docs-home` | auto | 6 | HTTP 429 left the title or `h1` assertion unsatisfied. |
| `github-docs-home` | new | 6 | HTTP 429 left the title assertion unsatisfied. |

Raw JSONL, reports, build logs, fixture hashes, profiles and `summary.json` are under `/data/bare-tmp/r22/rerun2/` on archMachine.

Untested work includes the code-ordered build, idle-machine latency, recorded-response comparisons, Windows, windowed runs, standalone benchmarks, and the compatibility suite.
