# 2026-11-21 — js2 and the old engine on browser journeys

Both engines passed triage on 44 public journeys and the React typing control. js2 used 1.06× the public instruction total and 1.00× the median paired peak RSS. The Python documentation search pages add the largest public instruction increases. The slow global loop drives most of the increase on the local ramps.

| Group | Cases | Instructions, G | Sum of peak RSS, MiB | Sum of ready times, s |
|---|---:|---:|---:|---:|
| Live public journeys | 44 | 41.68 → 44.35 (**1.06×**) | 1,811.02 → 1,664.03 (**0.92×**) | 23.38 → 22.89 (**0.98×**) |
| Local ramps | 26 | 48.27 → 75.67 (**1.57×**) | 1,013.04 → 1,034.66 (**1.02×**) | 8.43 → 10.61 (**1.26×**) |
| React typing | 1 | 6.38 → 4.88 (**0.77×**) | 38.11 → 39.06 (**1.03×**) | 0.14 → 0.12 (**0.87×**) |

The public median workload used 27.7 → 26.4 MiB peak RSS. The median paired ready-time ratio was 1.01×. The RSS sum adds peaks from separate processes. The ready-time sum describes the observed runs on this shared machine.

## Method

Both engines ran from the same frozen checkout of [PR #854](https://github.com/mrwick1/bare/pull/854), commit `69c578f29bf195fb05e6471201e9385c2cec45e4`. Old is `BARE_JS_EXEC=auto`, the tiered mode used by `triage.sh`. js2 is `BARE_JS_EXEC=new`.

Machine `archMachine`, AMD Ryzen 9 5900HX (8 cores, 16 threads), 30.7 GiB usable RAM, Linux `7.2.15-zen1-1-zen`, glibc 2.44, Rust 1.96.0 and perf 7.3.1. Before the build, `/data` had 86 GiB free. The profiling build has debug symbols and frame pointers.

```sh
CARGO_BUILD_JOBS=4 CARGO_TARGET_DIR=/data/bare-tmp/r21/meas \
  RUSTFLAGS='-C force-frame-pointers=yes' \
  cargo build --locked --profile profiling -p bare-drive --bin bare-drive \
    -p bare --example type_load --example browser_load -p bare-stress --bin bare-stress
```

- Public inputs are the frozen `tools/drive/public/*.steps` files and the live URLs in their comments. The driver runs headless. `BARE_SEED=1` sets the seed for browser inputs that honor it. The live replies and the work after the last assertion can differ.
- Local inputs use a loopback HTTP server outside the measured process. `compm_*`, `readall_*` and `rest_13` are the historical style fixtures from `/data/claude-tmp/cwpages`. The CSSOM mutation fixtures come from `/data/claude-tmp/css8/pages`. All local driver steps open the page and assert `window.__done === 1`.
- The generated loop fixtures add the integers from 0 up to `N - 1` and set the completion marker if the result is `N * (N - 1) / 2`. The `loop_*` rows use script globals for the index and accumulator. The `loop_local_*` control puts both in a function and checks the returned sum. The typing control loads the existing React page from `/data/bare-tmp/triage/typing-page` and types 50 characters in `#a1`.
- Each result uses three successful cold processes per engine for instruction counts and three more for RSS and ready time. Engine order alternates between rounds. Instruction counts use `perf stat -e instructions:u` and `glibc.malloc.tcache_count=65535`, as in `tools/js/instr.sh`. Counters cover the whole run, including work after the ready point. Peak RSS is Linux `wait4` from a small launcher, with the default allocator and a fresh `cp --reflink=never` copy for each run.
- Ready time uses the driver step durations from the start of the step file through the first action after `open`, as in the ready criterion in `tools/bench/journeys.py`. For ramps this ends at the script completion assertion. For typing it is the load time printed by `type_load`. The machine was shared. The wall-time columns show observations under load.
- Raw runs, decoded perf stacks and the input hashes are under `/data/bare-tmp/r21/meas-results` on `archMachine`.

One-minute load before measured runs ranged from 14.2 to 30.9. GitHub Docs returned HTTP 429 during the sweep. GitHub Docs and Hacker News received replacement runs for failed samples. The tables use the first three successful samples per engine and metric.

## Public journeys and typing

Old triage passed 44/48 public journeys. js2 triage passed 45/48 public journeys. Both failed `curl-se-home`, `duckduckgo-html-search`, and `lobsters-comments-back`. `nextjs-docs` passed on js2 and stopped on the old engine at an unsupported `checkVisibility` case. Triage skipped staging.

Cells show old → js2. Each measurement gives the min–max spread. Square brackets give the median for RSS and ready time. The bold value is js2 divided by old, using the minimum for instructions and the median for RSS and ready time.

| Workload | Instructions, M | Peak RSS, MiB | Ready, ms |
|---|---:|---:|---:|
| `archlinux-org-home` | 79.6–81.0 → 77.4–78.1 (**0.97×**) | 23.4–24.0 [23.9] → 24.0–24.3 [24.1] (**1.01×**) | 734.8–761.8 [751.7] → 736.3–1,808.7 [739.1] (**0.98×**) |
| `blog-rust-lang-home` | 154.7–155.5 → 153.7–154.4 (**0.99×**) | 24.8–24.9 [24.9] → 25.0–25.4 [25.1] (**1.01×**) | 107.0–120.5 [112.5] → 103.4–132.8 [114.2] (**1.02×**) |
| `cnn-lite-front` | 71.0–72.1 → 79.9–81.0 (**1.13×**) | 23.9–24.7 [24.3] → 24.6–25.4 [25.1] (**1.03×**) | 196.9–241.5 [204.4] → 204.0–247.6 [205.6] (**1.01×**) |
| `craigslist-home` | 376.8–379.8 → 81.7–81.9 (**0.22×**) | 25.0–94.0 [30.4] → 23.7–24.2 [24.0] (**0.79×**) | 4,152.2–4,569.5 [4,222.5] → 4,162.8–4,230.4 [4,175.0] (**0.99×**) |
| `crates-io-home` | 85.9–89.7 → 95.1–99.2 (**1.11×**) | 25.6–26.2 [26.1] → 26.1–26.5 [26.3] (**1.01×**) | 365.3–601.6 [560.3] → 461.7–1,074.0 [600.0] (**1.07×**) |
| `debian-org-home` | 117.4–122.1 → 119.8–121.2 (**1.02×**) | 23.1–23.4 [23.4] → 23.8–23.9 [23.8] (**1.02×**) | 499.8–540.4 [520.4] → 484.0–561.0 [490.5] (**0.94×**) |
| `devto-home` | 5,205.5–13,787.2 → 3,155.3–12,934.8 (**0.61×**) | 109.7–121.9 [116.5] → 66.3–74.0 [72.3] (**0.62×**) | 159.3–491.5 [189.0] → 144.5–209.1 [201.6] (**1.07×**) |
| `docs-rs-home` | 69.7–69.9 → 69.1–69.7 (**0.99×**) | 24.1–24.5 [24.3] → 21.7–24.8 [24.4] (**1.00×**) | 110.0–158.3 [118.3] → 109.7–195.9 [126.7] (**1.07×**) |
| `example-learn-more` | 134.7–139.6 → 147.4–147.5 (**1.09×**) | 24.5–25.1 [24.6] → 26.1–26.7 [26.4] (**1.07×**) | 95.4–113.2 [106.3] → 106.2–127.9 [110.8] (**1.04×**) |
| `example-page` | 27.4–27.6 → 26.4–26.5 (**0.96×**) | 20.1–21.8 [20.2] → 22.6–23.2 [22.8] (**1.13×**) | 108.9–157.9 [116.7] → 115.6–124.8 [118.2] (**1.01×**) |
| `github-docs-home` | 2,717.8–2,718.5 → 2,492.5–2,494.5 (**0.92×**) | 66.6–90.5 [89.0] → 72.3–74.9 [74.4] (**0.84×**) | 94.7–141.3 [115.7] → 98.4–133.2 [99.0] (**0.86×**) |
| `go-dev-home` | 885.0–1,695.7 → 1,472.9–3,718.4 (**1.66×**) | 44.8–46.1 [44.8] → 47.8–50.5 [50.1] (**1.12×**) | 498.5–555.7 [528.7] → 530.6–842.0 [555.1] (**1.05×**) |
| `hn-comments-back` | 155.6–155.9 → 135.7–156.5 (**0.87×**) | 23.1–23.9 [23.7] → 23.7–23.9 [23.7] (**1.00×**) | 679.2–690.5 [682.1] → 686.7–693.5 [690.9] (**1.01×**) |
| `hn-front` | 204.6–205.3 → 203.1–204.1 (**0.99×**) | 23.9–24.0 [24.0] → 23.5–23.8 [23.8] (**0.99×**) | 693.1–1,124.4 [916.7] → 696.7–719.7 [703.8] (**0.77×**) |
| `hn-new-past` | 204.9–206.8 → 202.3–204.6 (**0.99×**) | 21.1–23.4 [23.1] → 23.1–23.5 [23.2] (**1.00×**) | 928.7–1,309.9 [1,165.3] → 928.7–964.3 [932.3] (**0.80×**) |
| `httpbin-cookies` | 37.0–38.1 → 32.6–32.8 (**0.88×**) | 21.4–21.8 [21.5] → 21.5–21.9 [21.7] (**1.01×**) | 939.0–974.7 [962.3] → 939.9–971.7 [962.4] (**1.00×**) |
| `httpbin-form-fill` | 46.4–46.4 → 44.9–45.0 (**0.97×**) | 22.3–22.9 [22.4] → 22.9–23.4 [23.0] (**1.03×**) | 713.9–751.7 [721.7] → 721.2–741.3 [730.2] (**1.01×**) |
| `httpbin-form-submit` | 58.0–58.2 → 64.6–65.2 (**1.11×**) | 22.6–23.0 [22.8] → 23.1–23.4 [23.2] (**1.02×**) | 704.8–727.0 [717.2] → 711.6–751.0 [732.0] (**1.02×**) |
| `kernel-org-home` | 33.3–33.5 → 30.6–30.7 (**0.92×**) | 18.9–22.2 [22.1] → 21.8–22.0 [21.9] (**0.99×**) | 182.3–209.2 [186.9] → 173.3–198.8 [192.6] (**1.03×**) |
| `lobsters-front` | 139.1–140.0 → 137.1–139.3 (**0.99×**) | 23.7–24.3 [24.3] → 24.2–24.9 [24.5] (**1.01×**) | 789.6–824.5 [813.9] → 800.2–828.7 [828.0] (**1.02×**) |
| `mdn-reference` | 4,745.9–5,063.7 → 412.0–439.1 (**0.09×**) | 54.4–136.7 [56.0] → 30.4–30.5 [30.4] (**0.54×**) | 708.1–752.1 [712.8] → 700.0–774.2 [747.6] (**1.05×**) |
| `nginx-org-home` | 39.0–39.5 → 37.5–37.9 (**0.96×**) | 21.7–22.6 [22.1] → 22.7–23.1 [23.0] (**1.04×**) | 722.5–862.7 [826.7] → 721.9–817.4 [732.3] (**0.89×**) |
| `nodejs-org-home` | 1,010.8–1,102.7 → 1,368.3–1,609.6 (**1.35×**) | 45.4–46.9 [45.6] → 45.5–47.1 [46.6] (**1.02×**) | 105.5–122.8 [116.7] → 104.3–121.2 [119.1] (**1.02×**) |
| `npr-text-story` | 33.0–37.7 → 28.9–28.9 (**0.87×**) | 22.2–22.6 [22.5] → 22.2–22.8 [22.4] (**1.00×**) | 205.5–233.2 [213.1] → 200.0–292.9 [252.7] (**1.19×**) |
| `paulgraham-home` | 63.8–65.8 → 62.6–68.3 (**0.98×**) | 22.7–23.0 [23.0] → 23.5–23.7 [23.5] (**1.02×**) | 759.7–774.0 [769.4] → 745.2–784.0 [780.4] (**1.01×**) |
| `python-docs-search` | 1,105.1–1,106.8 → 7,259.0–7,266.6 (**6.57×**) | 75.9–77.1 [76.4] → 120.4–122.0 [121.3] (**1.59×**) | 116.5–145.1 [119.0] → 101.6–145.7 [142.9] (**1.20×**) |
| `python-docs-search-results` | 1,730.7–2,372.1 → 8,218.5–8,918.4 (**4.75×**) | 84.7–91.2 [90.5] → 120.9–121.4 [121.0] (**1.34×**) | 99.2–132.5 [102.1] → 104.1–117.8 [114.5] (**1.12×**) |
| `python-docs-tutorial` | 264.1–265.8 → 272.9–285.7 (**1.03×**) | 25.3–25.7 [25.5] → 25.3–25.9 [25.7] (**1.01×**) | 161.6–286.3 [192.0] → 207.8–261.5 [244.8] (**1.28×**) |
| `python-org-docs-link` | 1,205.9–1,590.5 → 1,508.3–1,537.3 (**1.25×**) | 38.3–39.0 [38.4] → 38.9–40.2 [38.9] (**1.01×**) | 374.8–663.7 [421.9] → 347.2–464.3 [382.5] (**0.91×**) |
| `python-org-search` | 1,251.9–1,259.5 → 1,560.0–1,568.3 (**1.25×**) | 36.0–38.8 [38.6] → 38.6–39.3 [39.0] (**1.01×**) | 319.6–434.0 [432.0] → 321.8–414.9 [370.3] (**0.86×**) |
| `react-docs` | 2,007.1–2,197.1 → 893.4–917.9 (**0.45×**) | 74.9–76.8 [74.9] → 53.1–56.4 [56.1] (**0.75×**) | 149.2–172.7 [156.9] → 177.6–200.2 [179.9] (**1.15×**) |
| `rust-book-chapters` | 499.6–582.5 → 577.3–658.0 (**1.16×**) | 29.1–30.9 [30.4] → 27.2–30.4 [29.4] (**0.96×**) | 226.0–394.2 [277.1] → 223.2–320.6 [282.8] (**1.02×**) |
| `rust-book-search` | 2,376.6–2,379.2 → 2,886.6–2,893.2 (**1.21×**) | 105.5–106.6 [105.7] → 90.8–92.1 [91.6] (**0.87×**) | 216.0–305.2 [240.3] → 210.6–330.2 [214.4] (**0.89×**) |
| `rust-book-sidebar` | 302.8–305.3 → 359.9–367.4 (**1.19×**) | 28.8–29.7 [29.3] → 29.0–29.5 [29.0] (**0.99×**) | 229.4–290.1 [246.0] → 212.8–241.7 [237.5] (**0.97×**) |
| `rust-lang-org-home` | 89.1–94.2 → 104.8–107.0 (**1.18×**) | 25.1–25.5 [25.3] → 26.2–26.4 [26.3] (**1.04×**) | 94.9–128.8 [104.1] → 111.4–178.1 [116.2] (**1.12×**) |
| `sqlite-org-home` | 148.6–151.1 → 122.6–147.2 (**0.82×**) | 23.0–25.9 [25.2] → 23.9–26.4 [24.6] (**0.98×**) | 1,204.3–1,226.0 [1,216.4] → 1,187.9–1,260.4 [1,202.7] (**0.99×**) |
| `substack-home` | 5,793.3–5,838.8 → 1,749.6–1,762.7 (**0.30×**) | 156.4–157.9 [157.2] → 57.3–60.8 [60.6] (**0.39×**) | 1,085.5–1,256.1 [1,124.0] → 1,000.0–1,066.1 [1,035.6] (**0.92×**) |
| `w3c-home-links` | 717.8–723.0 → 163.8–170.6 (**0.23×**) | 29.9–30.6 [30.3] → 27.1–27.5 [27.3] (**0.90×**) | 93.4–112.9 [100.6] → 92.8–212.5 [98.0] (**0.97×**) |
| `wikipedia-article` | 1,832.7–1,854.6 → 1,899.1–1,975.9 (**1.04×**) | 42.9–43.3 [43.0] → 40.4–40.9 [40.7] (**0.95×**) | 505.2–635.7 [521.3] → 507.2–714.8 [540.0] (**1.04×**) |
| `wikipedia-main-random` | 1,030.7–1,244.0 → 1,266.0–1,382.8 (**1.23×**) | 37.3–40.0 [38.4] → 34.3–37.2 [35.0] (**0.91×**) | 299.7–325.9 [305.1] → 273.5–365.2 [293.9] (**0.96×**) |
| `wikipedia-portal` | 527.3–550.3 → 560.1–586.4 (**1.06×**) | 34.1–34.9 [34.3] → 32.7–33.7 [33.1] (**0.97×**) | 250.2–336.3 [253.2] → 245.2–300.8 [300.6] (**1.19×**) |
| `wikipedia-search` | 1,963.6–2,113.7 → 2,023.7–3,719.0 (**1.03×**) | 42.3–43.3 [42.6] → 40.6–44.1 [40.8] (**0.96×**) | 468.1–480.5 [471.0] → 414.8–3,209.7 [538.2] (**1.14×**) |
| `wikipedia-toc-languages` | 1,288.9–1,322.6 → 1,324.0–1,388.1 (**1.03×**) | 40.2–41.0 [40.7] → 37.6–38.2 [37.7] (**0.93×**) | 405.3–1,625.8 [515.5] → 402.3–463.7 [427.2] (**0.83×**) |
| `wiktionary-portal` | 844.6–849.1 → 872.4–874.7 (**1.03×**) | 39.0–39.3 [39.1] → 36.3–36.5 [36.3] (**0.93×**) | 211.7–283.6 [233.3] → 223.3–227.8 [227.6] (**0.98×**) |
| `typing` | 6,376.1–6,376.1 → 4,878.4–4,880.1 (**0.77×**) | 37.8–38.2 [38.1] → 38.6–39.1 [39.1] (**1.03×**) | 118.0–205.0 [135.0] → 101.0–155.0 [118.0] (**0.87×**) |

## Local load-test ramps

26 of 27 fixtures reached the completion marker on both engines. `churn_1m` failed in all three rounds on both engines. The old engine reported the DOM node limit. js2 reached the check with the marker unset.

| Workload | Instructions, M | Peak RSS, MiB | Ready, ms |
|---|---:|---:|---:|
| `compm_22000` | 405.3–405.3 → 403.8–403.8 (**1.00×**) | 36.9–37.5 [37.3] → 37.4–37.8 [37.8] (**1.01×**) | 78.8–102.6 [81.8] → 73.4–127.0 [103.5] (**1.27×**) |
| `compm_24000` | 445.3–445.3 → 443.9–444.0 (**1.00×**) | 38.2–38.6 [38.3] → 38.7–39.5 [38.7] (**1.01×**) | 99.9–116.5 [100.6] → 89.6–109.0 [94.5] (**0.94×**) |
| `compm_26000` | 482.8–485.2 → 481.5–481.5 (**1.00×**) | 39.8–41.5 [40.6] → 40.0–40.8 [40.8] (**1.00×**) | 94.9–166.5 [112.0] → 55.5–132.8 [124.1] (**1.11×**) |
| `compm_30000` | 552.6–552.7 → 551.4–551.4 (**1.00×**) | 42.2–43.0 [42.5] → 42.9–43.8 [43.1] (**1.01×**) | 147.0–232.8 [202.2] → 142.2–228.9 [208.8] (**1.03×**) |
| `compm_34000` | 623.6–624.4 → 622.4–623.1 (**1.00×**) | 45.7–47.0 [46.3] → 46.5–48.9 [47.2] (**1.02×**) | 122.0–142.3 [122.6] → 112.5–143.1 [124.1] (**1.01×**) |
| `compm_38000` | 693.1–694.4 → 692.0–693.4 (**1.00×**) | 48.6–48.8 [48.8] → 48.8–49.2 [49.1] (**1.00×**) | 142.5–200.4 [143.6] → 150.5–176.0 [176.0] (**1.23×**) |
| `compm_40000` | 728.3–734.2 → 727.2–737.5 (**1.00×**) | 50.1–50.7 [50.6] → 50.8–51.1 [50.9] (**1.01×**) | 136.2–253.7 [200.1] → 144.6–239.1 [154.0] (**0.77×**) |
| `compm_43000` | 782.5–786.3 → 780.0–784.9 (**1.00×**) | 51.9–52.1 [52.0] → 52.1–52.5 [52.3] (**1.01×**) | 153.6–207.2 [178.7] → 146.1–247.5 [208.2] (**1.17×**) |
| `ctor_100k` | 1,531.2–1,533.5 → 1,660.9–1,664.8 (**1.08×**) | 20.6–21.3 [21.1] → 20.6–20.8 [20.7] (**0.98×**) | 254.2–260.2 [259.0] → 226.9–271.1 [251.6] (**0.97×**) |
| `ctor_10k` | 159.6–159.9 → 171.0–171.6 (**1.07×**) | 20.4–20.5 [20.4] → 20.6–21.2 [20.8] (**1.02×**) | 50.4–77.7 [61.8] → 31.6–75.1 [36.8] (**0.60×**) |
| `ctor_1k` | 21.5–21.5 → 21.3–21.3 (**0.99×**) | 20.7–21.1 [20.9] → 20.4–21.1 [20.7] (**0.99×**) | 8.9–15.5 [12.0] → 9.2–24.9 [15.8] (**1.32×**) |
| `ctor_1m` | 15,019.0–15,033.4 → 16,534.1–16,550.6 (**1.10×**) | 20.3–20.9 [20.7] → 20.6–20.9 [20.7] (**1.00×**) | 2,429.2–3,100.5 [2,472.2] → 2,357.0–3,164.6 [2,946.0] (**1.19×**) |
| `ctor_read_100k` | 2,499.6–2,506.8 → 2,871.3–2,880.7 (**1.15×**) | 62.3–62.8 [62.5] → 67.7–68.2 [68.1] (**1.09×**) | 461.8–665.1 [474.9] → 541.5–830.8 [683.5] (**1.44×**) |
| `ctor_read_10k` | 252.5–252.6 → 291.0–291.5 (**1.15×**) | 25.4–26.1 [25.6] → 26.3–26.8 [26.6] (**1.04×**) | 72.9–103.1 [82.9] → 78.6–90.2 [83.5] (**1.01×**) |
| `ctor_read_1k` | 31.1–31.2 → 33.7–33.8 (**1.08×**) | 20.7–21.3 [21.2] → 21.3–21.9 [21.3] (**1.01×**) | 12.6–22.7 [22.4] → 12.7–95.0 [39.3] (**1.75×**) |
| `loop_100000` | 95.5–95.5 → 314.5–317.5 (**3.29×**) | 20.2–20.6 [20.5] → 20.1–20.6 [20.2] (**0.99×**) | 15.9–18.7 [16.7] → 27.2–64.3 [43.0] (**2.57×**) |
| `loop_1000000` | 902.8–902.8 → 3,113.5–3,143.5 (**3.45×**) | 19.8–20.2 [20.1] → 20.0–20.6 [20.5] (**1.02×**) | 106.6–112.1 [111.6] → 246.2–297.6 [290.4] (**2.60×**) |
| `loop_10000000` | 8,975.8–8,975.8 → 31,103.5–31,403.5 (**3.47×**) | 20.4–20.5 [20.5] → 20.2–20.6 [20.2] (**0.99×**) | 701.5–912.7 [902.8] → 1,847.0–2,822.9 [1,933.7] (**2.14×**) |
| `readall_fwd` | 1,495.1–1,498.0 → 1,733.2–1,737.1 (**1.16×**) | 78.2–78.6 [78.5] → 72.5–72.6 [72.5] (**0.92×**) | 287.3–331.6 [327.0] → 335.6–390.7 [363.1] (**1.11×**) |
| `readall_rev` | 1,464.9–1,468.4 → 1,653.8–1,659.0 (**1.13×**) | 77.6–78.9 [78.2] → 71.7–73.6 [71.8] (**0.92×**) | 287.7–426.9 [288.3] → 335.9–482.2 [357.5] (**1.24×**) |
| `readall_spans` | 1,265.6–1,268.5 → 1,391.2–1,394.8 (**1.10×**) | 71.4–73.4 [72.8] → 67.6–68.1 [67.8] (**0.93×**) | 353.9–525.7 [423.0] → 379.8–433.5 [412.7] (**0.98×**) |
| `rest_13` | 1,079.5–1,079.6 → 1,078.0–1,086.5 (**1.00×**) | 52.0–52.6 [52.2] → 53.0–53.5 [53.1] (**1.02×**) | 320.1–484.6 [432.7] → 321.7–543.4 [528.6] (**1.22×**) |
| `style_100k` | 2,430.5–2,430.5 → 2,497.8–2,498.5 (**1.03×**) | 31.1–31.7 [31.2] → 46.0–47.0 [46.4] (**1.49×**) | 310.4–350.3 [349.2] → 348.9–444.6 [408.1] (**1.17×**) |
| `style_10k` | 247.0–247.2 → 252.1–252.3 (**1.02×**) | 21.3–21.9 [21.9] → 23.3–23.8 [23.6] (**1.08×**) | 50.8–56.0 [54.6] → 58.0–70.1 [58.8] (**1.08×**) |
| `style_1k` | 30.2–30.2 → 29.4–29.4 (**0.97×**) | 20.4–21.1 [21.1] → 21.1–21.9 [21.6] (**1.02×**) | 11.3–28.3 [13.8] → 12.1–12.5 [12.3] (**0.89×**) |
| `style_250k` | 6,051.4–6,064.3 → 6,213.7–6,225.5 (**1.03×**) | 46.9–47.5 [47.3] → 58.2–58.7 [58.2] (**1.23×**) | 651.0–1,410.0 [988.2] → 674.1–1,448.5 [951.2] (**0.96×**) |

## Loop and object-literal controls

The function-local loop repeats the 10M-iteration sum. The object control builds a literal with 1,000 to 8,000 distinct, quoted keys of fixed width and verifies the final value. Both use the same driver steps as the ramps.

| Workload | Instructions, M | Peak RSS, MiB | Ready, ms |
|---|---:|---:|---:|
| `loop_local_10000000` | 1,985.8–1,985.9 → 1,523.6–1,523.6 (**0.77×**) | 20.3–20.6 [20.4] → 20.2–20.6 [20.3] (**1.00×**) | 219.5–607.3 [306.7] → 244.8–353.1 [262.9] (**0.86×**) |
| `object_1000` | 8.7–8.7 → 32.7–32.7 (**3.76×**) | 20.1–20.3 [20.2] → 20.7–21.1 [20.8] (**1.03×**) | 7.1–11.5 [8.1] → 13.9–35.1 [27.2] (**3.36×**) |
| `object_2000` | 11.8–11.8 → 94.4–94.4 (**8.03×**) | 20.7–20.8 [20.7] → 21.0–21.6 [21.4] (**1.03×**) | 7.2–9.3 [8.6] → 33.8–44.2 [35.9] (**4.17×**) |
| `object_4000` | 18.0–18.0 → 316.3–316.6 (**17.62×**) | 21.0–21.4 [21.1] → 20.6–22.0 [21.8] (**1.03×**) | 16.4–72.4 [16.8] → 75.3–156.8 [103.8] (**6.18×**) |
| `object_8000` | 30.5–30.5 → 1,154.5–1,156.4 (**37.88×**) | 22.0–22.5 [22.1] → 23.1–24.3 [23.5] (**1.07×**) | 10.9–24.2 [12.0] → 178.3–223.4 [209.1] (**17.43×**) |

## Instruction profiles

These are the five heaviest measured workloads by median js2 instructions. Each engine has three additional recordings per workload with `perf record -e instructions:u --call-graph fp` and the same allocator settings as the instruction runs. The period ranges from 0.8 to 3.1 M instructions per sample. Each cell is the mean sample share, old → js2. The JS total includes the six JS rows. Paint can round to zero.

The split attributes each sample to the nearest identified subsystem in its stack. Collector frames take precedence. CSS parse is part of style. The JS parse and compile row includes the old direct data-literal decoder. The JS host row includes engine-side dispatch, adapters and host-aware lookup. DOM and Web APIs includes browser-side binding work. Inlined code can fall under its caller. The shares estimate instruction attribution.

| Subsystem | `loop_10000000` | `ctor_1m` | `devto-home` | `python-docs-search-results` | `python-docs-search` |
|---|---:|---:|---:|---:|---:|
| Network | 0.0% → 0.0% | 0.0% → 0.0% | 5.4% → 3.9% | 7.8% → 1.9% | 6.8% → 1.0% |
| HTML parse | 0.0% → 0.0% | 0.0% → 0.0% | 2.2% → 3.3% | 21.2% → 4.7% | 0.9% → 0.2% |
| Style | 0.0% → 0.0% | 39.4% → 34.0% | 22.3% → 35.8% | 2.8% → 0.7% | 3.4% → 0.5% |
| Layout | 0.0% → 0.0% | 0.0% → 0.0% | 3.9% → 6.1% | 0.8% → 0.2% | 1.7% → 0.2% |
| Paint | 0.0% → 0.0% | 0.0% → 0.0% | 0.0% → 0.0% | 0.0% → 0.0% | 0.0% → 0.0% |
| Images | 0.0% → 0.0% | 0.0% → 0.0% | 2.8% → 2.0% | 0.0% → 0.0% | 0.0% → 0.0% |
| DOM + Web APIs | 0.0% → 0.0% | 41.0% → 38.7% | 19.8% → 27.4% | 8.3% → 1.9% | 0.9% → 0.2% |
| JS parse and compile | 0.0% → 0.0% | 0.0% → 0.0% | 12.5% → 13.1% | 17.9% → 76.6% | 35.5% → 88.6% |
| JS interpreter and runtime | 100.0% → 60.9% | 19.4% → 14.3% | 28.1% → 2.5% | 28.6% → 5.0% | 39.2% → 4.5% |
| JS GC | 0.0% → 0.0% | 0.0% → 0.0% | 0.0% → 0.0% | 10.0% → 4.1% | 6.7% → 0.4% |
| JS built-ins | 0.0% → 0.0% | 0.0% → 2.1% | 2.1% → 3.9% | 0.1% → 4.4% | 0.0% → 3.7% |
| JS host calls | 0.0% → 39.1% | 0.0% → 10.7% | 0.0% → 0.9% | 0.0% → 0.3% | 0.0% → 0.4% |
| JS realm setup | 0.0% → 0.0% | 0.0% → 0.0% | 0.1% → 0.0% | 0.1% → 0.0% | 0.2% → 0.0% |
| Driver | 0.0% → 0.0% | 0.0% → 0.0% | 0.1% → 0.1% | 0.6% → 0.0% | 1.0% → 0.0% |
| Other | 0.0% → 0.0% | 0.2% → 0.2% | 0.8% → 1.0% | 1.8% → 0.3% | 3.6% → 0.3% |
| JS total | 100.0% → 100.0% | 19.4% → 27.1% | 42.8% → 20.4% | 56.8% → 90.4% | 81.7% → 97.6% |

| Workload | Old samples per run | js2 samples per run |
|---|---:|---:|
| `loop_10000000` | 9,999 / 9,997 / 9,995 | 10,089 / 10,089 / 10,091 |
| `ctor_1m` | 10,085 / 10,127 / 10,079 | 10,024 / 9,981 / 10,057 |
| `devto-home` | 7,658 / 7,864 / 7,985 | 4,406 / 5,483 / 4,398 |
| `python-docs-search-results` | 3,061 / 2,582 / 2,584 | 10,605 / 9,931 / 9,685 |
| `python-docs-search` | 1,374 / 1,370 / 1,368 | 9,060 / 9,052 / 9,057 |

## Largest measured costs

The ranks use the mean of the estimated instruction counts from the three js2 profiles of each workload. The estimate multiplies the count of samples in a subsystem by the recording period and sums the per-workload means. The large loop and CSSOM ramp have substantial weight in this ranking.

1. **Interpreter and runtime, about 22.3 G.** `Vm::exec`, `get_prop_g`, `get_global_slow` and `set_named` are hot on the global loop. The 10M-iteration loop is 3.47× the old engine with globals, and 0.77× with function locals. **Hypothesis:** the browser global object is in dictionary mode by the time script globals are read. `get_global_slow` and `set_global_slow` fill the global cache only for cacheable shapes. The browser run did not record the shape or cache-hit count.
2. **JS host calls and lookup, about 14.1 G.** `hs_walk` and `hs_plain` are hot on the global loop. `api::host_call` also appears on `ctor_1m`. **Hypothesis:** the browser global repeatedly enters the host-aware property walk for ordinary reads. CSSOM operations add host dispatch and argument conversion. The lookup share includes checks that resolve a property without calling the host.
3. **JS parse and compile, about 13.3 G.** These stages account for 76.6% of js2 samples on `python-docs-search-results` and 88.6% on `python-docs-search`. The hottest addresses in `emit_value` on the results journey resolve to `names.contains(&n)` in `Fc::object`. This is the linear duplicate-key scan. It still runs when the object has too many keys for the short literal form. The old profile shows `data_lit::Build::value`, which reads large JSON-shaped literals directly from source. **Hypothesis:** the duplicate-key scan also drives most of the difference on the second Python journey. Doubling the keys from 4,000 to 8,000 in a local literal raised the js2 count from 316 M to 1,155 M. The old engine went from 18 M to 30 M.
4. **DOM and Web APIs, about 7.6 G.** `browser_cssom::publish_rules`, `chunks` and `items` are hot on `ctor_1m` in both engines. **Hypothesis:** repeatedly publishing the adopted sheet accounts for much of this share. The fixture inserts and deletes one rule per iteration, and the final sheet length stays constant.
5. **Style, about 7.1 G.** `Tokens::next_spanned`, `Parser::declaration_list` and `Parser::rule_list` are hot on `ctor_1m`. **Hypothesis:** the repeated parsing of the same inserted rule accounts for most of this share. The live `devto-home` profiles also have a large style share, under different amounts of JS work.

JS built-ins account for about 1.1 G estimated instructions across the five profiles. GC accounts for about 0.38 G. `Marker::drain` is the largest js2 GC symbol on the Python search results. On that journey GC is a smaller share of a larger instruction total.

## Coverage limits

Public journeys check their steps. They leave room for the engines to run different page scripts or load different resources. For example, on `mdn-reference`, the old-engine instruction runs recorded 75–79 network replies. The js2 runs recorded 36–42 and logged an unhandled promise rejection. The lower total for that journey includes less observed page work. On `devto-home`, the count spans 5.2–13.8 G on old and 3.2–12.9 G on js2. Treat the live totals as a description of this sweep.

The historical large linked-sheet ramps `sheety_8192` and `sheety_16383` have dangling fixture links. The new loop fixtures are a separate check. `bare-stress` exercises individual crates without switching the browser VM. `browser_load` constructs the old browser directly. Both binaries built. The measurements use `bare-drive` and `type_load` for the engine comparison.

This sweep leaves public replay, idle-machine wall time, authenticated staging, windowed runs, Windows, the old tree-only and byte-only modes, and the full test suite untested. This entry adds measurements only.
