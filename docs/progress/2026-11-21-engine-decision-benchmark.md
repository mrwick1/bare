# 2026-11-21 — Engine decision benchmark

**Keep js2 on the browser path.** It cut standalone CPU work against OLD by 58%. It also used fewer instructions and less peak memory across the paired public journeys. QuickJS did less total work and used less memory on the standalone suite. A browser adapter has its own integration cost and needs its own benchmark.

| Metric | js2 versus OLD | js2 versus QuickJS |
|---|---|---|
| CPU instructions | **js2 wins.** 3.51 G vs 8.40 G standalone. 0.81× on all 44 public journeys and 0.80× on 26 paired local ramps versus `auto`. | **QuickJS wins on the total.** 3.51 G for js2, 3.28 G for the CLI and 2.90 G for the direct launcher. The engines split the 16 workloads equally. |
| Peak RSS | **js2 wins in aggregate.** The standalone ratio has a geometric mean of 0.94×. The sum of per-process peaks is 0.81× on public journeys and 1.00× on local ramps, which is near parity. | **QuickJS wins all 16 standalone cases.** The geometric mean of js2/QuickJS direct peak RSS is 1.91×. |
| Startup | **js2 wins on both metrics.** 0.72 M vs 2.09 M instructions and 4.93 vs 5.43 MiB for the empty process. | **Split.** js2 uses 0.72 M vs 1.05 M instructions for QuickJS direct. QuickJS uses 3.05 MiB vs 4.93 MiB. |
| Time to ready | **Inconclusive on the shared machine.** Public totals were 21.58 → 21.59 s for `auto` and js2. | **Unavailable.** QuickJS has no browser binding here. |

For a JavaScript developer, the rewrite makes most of these scripts cheaper to run than the old interpreter. In this set, QuickJS does less work on long loops and object-heavy code. js2 does less work on exceptions and closures. QuickJS also uses less memory. The page checks can pass even if the engines run different scripts after the page is ready.

## Frozen inputs

| Input | Commit or version | Status |
|---|---|---|
| OLD | `48ccf6928970f52a79a7ed50a8aad4bfa03b84c7` | `crates/js` from `origin/main` at the start of the run |
| js2 | `05b520eb1876997a91b4e43368f33d493071f1e5` | Local scratch merge based on OLD |
| [#867](https://github.com/mrwick1/bare/pull/867) | `b0b298d0612b36762a06c3d209c4520d288e84f3` | Merged first |
| [#889](https://github.com/mrwick1/bare/pull/889) | `a23db4446029ff79dce166fea0b3197298dc9d6a` | Merged second |
| [#896](https://github.com/mrwick1/bare/pull/896) | `5d95f69490e94fc687daecc689acdbbd885b41d9` | Already in #889 |
| [#863](https://github.com/mrwick1/bare/pull/863) | `713f6ce9968262e510b5d9d3b0c976994c38ac2f` | Skipped after conflicts in `bi_object.rs`, `bi_reflect.rs`, `interp_rt.rs`, `lazy.rs` and `object.rs` |
| [#872](https://github.com/mrwick1/bare/pull/872) | `b6e5a5780f9beeec9983cd2721f4fcb4b1398186` | Merged after #863 was skipped |
| [QuickJS](https://bellard.org/quickjs/) | Bellard 2026-06-04 | GCC 16.2.1, `-O2`, `CONFIG_LTO=y` |
| Node | 22.18.0, V8 12.4.254.21-node.27 | Default JIT enabled and `--jitless` |

The scratch merge retains both the Proxy constructor and the BigInt lazy factory. It retains the existing global registrations and removes a stale interpreter comment. The BigInt lookup calls the named-property helper from #889. This resolves a signature mismatch in the merged tree. The compiled scratch build returned `42` for `BigInt(42).toString()` and `6` for a direct eval of a Proxy read. The benchmark PR contains one report file.

The machine was `archMachine`, AMD Ryzen 9 5900HX with 8 cores and 16 threads, 30.7 GiB usable RAM, Linux `7.2.15-zen1-1-zen`, glibc 2.44, Rust 1.96.0 and perf 7.3.1. Builds used locked dependencies, four Cargo jobs and a separate target directory for each tree. The QuickJS source archive had SHA-256 `b376e839b322978313d929fd20663b11ba58b75df5a46c126dd19ea2fa70ad2a`.

## A. Standalone engines

Each case ran in a fresh process three times under `perf stat -e instructions:u` and three more times under the static `wait4` RSS wrapper from the earlier comparison. Counts cover process startup, the workload and shutdown. Each table cell shows the minimum instruction count or median RSS. Brackets give the minimum and maximum when the spread is visible at the displayed precision. [Earlier engine comparison](../research-quickjs-reference.md)

- `eval_load CASE SCALE --source` gave the exact source and independent expected result for each case. Both Rust runners read that source and return its script completion value. QuickJS and Node read a same-directory file that passes those bytes to indirect `eval` and prints the result. The QuickJS direct control reads the same raw file as Rust and calls `JS_Eval` as a global script. It uses the same LTO objects as the CLI build. The empty case evaluates `0`. Every instruction run passed the checksum. Each RSS process returned success.
- **OLD matched** uses `Exec::Auto`, the default system allocator and no forced collection at exit. **OLD eval_load** is the original example in `auto` mode, with allocation counters, a final collection and a post-collection check for closures. The matched runner controls for this instrumentation. The static C RSS wrapper and the QuickJS archive came from the recovered harness at `/data/claude-tmp/qjs-base/`. The js2 runner uses the script completion API. Both are release builds. QuickJS and Node include their CLI and realm startup costs.
- The bundle is the 314,192-byte React typing fixture `app.js` from the previous journey run. Its SHA-256 is `1992c72031ad2bad490a784172a2e9400b822cb0eb4d0e539f4a021af7c07400`. The Rust parse rows use the full parser and a default allocator. The js2 compile row eagerly compiles function bodies with `script::compile`. The QuickJS compile row uses the same LTO engine objects as `qjs`, with `JS_EVAL_FLAG_COMPILE_ONLY` and a small C launcher. Every bundle row stops before evaluation. OLD compiles eligible code as it runs, so the OLD row measures a full parse only.

Across the 16 nonempty cases, js2 used 3.51 G instructions in total. OLD matched used 8.40 G and OLD `eval_load` used 8.69 G. js2 beat OLD matched on 14 of 16 rows. The geometric mean of the js2/OLD matched ratios was 0.47×. Both webpack rows took more instructions than OLD matched. The geometric mean of peak RSS ratios was 0.94× against OLD matched. The `objects_10` row rose from 8.98 to 21.83 MiB.

The `qjs` CLI used 3.28 G instructions. The engines split the 16 rows equally. QuickJS led on loops, calls, object writes and webpack. js2 led on exceptions, closures, recursion and concatenation. The QuickJS total was 7% lower, while the geometric mean of the js2/QuickJS ratios was 0.95×. In the direct-launcher control QuickJS used 2.90 G instructions, 18% fewer than js2. The engines still split the rows equally. The largest change from the CLI was `throws_10`, from 874 M to 536 M. Hypothesis: the indirect eval path changes the work needed to create an error stack. Both QuickJS builds used less peak RSS than js2 on all 16 rows. The geometric mean of the js2/QuickJS peak RSS ratios was 1.76× for the CLI and 1.91× for the direct runner.

The empty process took 0.72 M instructions with js2, 2.09 M with OLD matched and 1.23 M with QuickJS. The direct QuickJS runner took 1.05 M instructions and 3.05 MiB of peak RSS. The QuickJS CLI took 3.33 MiB, js2 took 4.93 MiB, and OLD took 5.43 MiB. The empty row measures process startup work and memory. It gives no idle startup time.

js2 used 9% fewer instructions than OLD for the full parse of this React bundle. QuickJS used 23% fewer instructions and 12% less peak RSS than js2 for the eager compile comparison. This comparison covers one bundle. The browser can defer compilation of function bodies.

### Instructions, millions

| Case | OLD matched | OLD eval_load | js2 | qjs CLI | QuickJS direct | Node jitless | Node JIT |
|---|---:|---:|---:|---:|---:|---:|---:|
| `empty` | 2.09 | — | 0.72 | 1.23 | 1.05 | 126.84 [126.84–127.07] | 94.64 [94.64–94.92] |
| `loop_1` | 227.01 | 230.59 | 87.60 | 64.69 | 64.51 | 218.47 [218.47–218.79] | 120.00 [120.00–121.02] |
| `loop_10` | 2,341.56 | 2,373.04 | 868.80 | 635.29 [635.29–635.30] | 635.12 | 1,043.28 [1,043.28–1,043.39] | 184.06 [184.06–186.74] |
| `calls_1` | 159.88 | 162.17 | 69.75 | 60.62 | 60.44 | 195.78 [195.78–195.99] | 116.21 [116.21–119.08] |
| `calls_10` | 1,584.79 | 1,603.28 | 689.85 | 594.33 | 594.15 | 815.29 [815.29–815.42] | 138.07 [138.07–148.63] |
| `objects_1` | 195.11 [195.11–195.12] | 198.48 | 105.47 [105.47–105.62] | 53.38 | 52.98 | 174.16 [174.16–174.60] | 146.70 [146.70–149.44] |
| `objects_10` | 1,909.49 [1,909.49–1,909.66] | 1,935.55 | 1,023.58 [1,023.58–1,024.13] | 509.04 | 507.64 | 554.98 [554.98–555.83] | 332.62 [332.62–338.94] |
| `throws_1` | 128.11 | 139.72 | 31.49 | 88.45 [88.45–88.52] | 55.10 [55.10–55.13] | 291.67 [291.67–291.94] | 296.06 [296.06–297.20] |
| `throws_10` | 1,279.11 | 1,395.34 | 307.61 | 874.13 [874.13–875.40] | 535.82 [535.82–536.35] | 1,763.45 [1,763.45–1,763.75] | 2,171.64 [2,171.64–2,172.05] |
| `closures_1` | 43.14 | 49.05 | 21.36 | 26.06 [26.06–26.07] | 25.92 [25.92–25.97] | 141.27 [141.27–141.49] | 110.81 [110.81–111.33] |
| `closures_10` | 412.56 | 467.33 | 207.27 | 279.40 [279.40–279.60] | 277.48 [277.48–277.68] | 276.46 [276.46–276.61] | 198.89 [198.89–201.97] |
| `fib_1` | 7.10 | 7.66 | 3.83 | 4.37 | 4.19 | 129.71 [129.71–129.93] | 98.96 [98.96–99.12] |
| `fib_10` | 56.88 | 58.24 [58.24–58.25] | 34.25 | 35.55 | 35.36 [35.36–35.37] | 157.32 [157.32–157.41] | 110.28 [110.28–110.57] |
| `webpack_1` | 6.70 | 7.53 | 6.85 | 5.41 | 4.99 | 131.81 [131.81–132.00] | 99.46 [99.46–99.53] |
| `webpack_10` | 47.20 [47.20–47.21] | 50.88 | 52.75 [52.75–52.77] | 40.78 | 38.21 [38.21–38.23] | 168.71 [168.71–169.18] | 138.34 [138.34–138.79] |
| `concat_1` | 2.29 | 2.79 | 0.86 | 1.34 | 1.16 | 127.04 [127.04–127.35] | 94.74 [94.74–94.89] |
| `concat_10` | 3.40 [3.40–3.41] | 4.00 | 1.36 | 2.15 | 1.97 | 127.50 [127.50–127.64] | 94.92 [94.92–95.21] |

### Peak RSS, MiB

| Case | OLD matched | OLD eval_load | js2 | qjs CLI | QuickJS direct | Node jitless | Node JIT |
|---|---:|---:|---:|---:|---:|---:|---:|
| `empty` | 5.43 [5.34–5.46] | — | 4.93 [4.91–5.03] | 3.33 [3.25–3.46] | 3.05 [2.94–3.09] | 43.73 [43.42–43.75] | 46.09 [45.84–46.30] |
| `loop_1` | 5.38 [5.30–5.50] | 5.74 [5.74–5.86] | 5.43 [5.30–5.55] | 3.32 [3.18–3.41] | 3.12 [2.94–3.14] | 43.89 [43.82–43.89] | 51.69 [51.49–51.71] |
| `loop_10` | 5.57 [5.49–5.57] | 5.89 [5.61–5.89] | 5.30 [5.25–5.39] | 3.24 [3.19–3.38] | 2.99 [2.97–2.99] | 43.77 [43.64–43.89] | 51.71 [51.55–51.71] |
| `calls_1` | 5.60 [5.59–5.71] | 5.91 [5.83–5.98] | 5.38 [5.30–5.40] | 3.35 [3.32–3.35] | 2.99 [2.93–3.09] | 43.80 [43.72–43.84] | 51.82 [51.81–51.87] |
| `calls_10` | 5.60 [5.58–5.61] | 5.89 [5.78–5.92] | 5.34 [5.28–5.46] | 3.36 [3.29–3.43] | 2.98 [2.93–3.16] | 43.76 [43.56–43.89] | 51.93 [51.90–51.95] |
| `objects_1` | 6.19 [6.02–6.24] | 6.58 [6.42–6.70] | 8.71 [8.63–8.75] | 3.74 [3.66–3.87] | 3.35 [3.34–3.46] | 45.02 [44.96–45.11] | 53.41 [53.18–53.59] |
| `objects_10` | 8.98 [8.91–9.06] | 12.06 [11.73–12.08] | 21.83 [21.65–21.99] | 6.56 [6.53–6.57] | 6.22 [6.22–6.31] | 50.83 [50.65–50.91] | 59.64 [59.38–59.73] |
| `throws_1` | 7.97 [7.89–7.98] | 8.21 [8.17–8.34] | 5.95 [5.95–6.02] | 3.36 [3.33–3.39] | 3.14 [3.12–3.19] | 45.71 [45.62–45.73] | 53.94 [53.91–53.94] |
| `throws_10` | 31.66 [31.66–31.72] | 32.89 [32.78–33.02] | 12.38 [12.17–12.50] | 3.39 [3.38–3.52] | 3.07 [3.01–3.14] | 49.27 [49.26–49.27] | 57.34 [57.34–57.46] |
| `closures_1` | 7.36 [7.20–7.38] | 7.99 [7.78–8.11] | 6.35 [6.14–6.42] | 4.52 [4.50–4.61] | 4.10 [4.07–4.13] | 45.22 [45.05–45.26] | 53.13 [52.75–53.18] |
| `closures_10` | 22.55 [22.45–22.82] | 27.39 [27.37–27.50] | 14.06 [13.88–14.11] | 13.53 [13.41–13.81] | 13.37 [13.24–13.42] | 51.36 [51.36–51.62] | 60.03 [59.84–60.06] |
| `fib_1` | 5.50 [5.37–5.62] | 5.86 [5.84–5.88] | 5.29 [5.22–5.36] | 3.36 [3.34–3.41] | 3.12 [3.04–3.14] | 43.75 [43.65–43.77] | 51.71 |
| `fib_10` | 5.56 [5.41–5.64] | 5.86 [5.45–5.88] | 5.23 [5.19–5.28] | 3.35 [3.30–3.37] | 3.06 [2.92–3.09] | 43.73 [43.61–43.75] | 51.68 [51.54–51.71] |
| `webpack_1` | 5.78 [5.71–5.85] | 6.11 [6.09–6.16] | 5.78 [5.57–5.87] | 3.45 [3.16–3.60] | 3.30 [2.91–3.32] | 43.77 [43.73–43.77] | 46.31 [46.18–46.34] |
| `webpack_10` | 7.48 [7.28–7.55] | 7.67 [7.66–7.72] | 7.05 [6.78–7.12] | 4.07 [4.00–4.20] | 3.86 [3.63–3.94] | 44.37 [44.29–44.43] | 46.96 [46.96–47.09] |
| `concat_1` | 5.57 [5.57–5.60] | 5.89 [5.85–5.97] | 5.18 [5.14–5.42] | 3.42 [3.32–3.42] | 3.08 [2.97–3.09] | 43.77 [43.48–43.86] | 46.19 [46.09–46.34] |
| `concat_10` | 5.39 [5.33–5.64] | 5.78 [5.64–5.91] | 5.46 [5.38–5.54] | 3.46 [3.37–3.48] | 3.07 [2.99–3.15] | 43.72 [43.56–43.86] | 46.25 [46.14–46.34] |

### React bundle front end

| Engine and work | Instructions, M | Peak RSS, MiB |
|---|---:|---:|
| OLD full parse | 69.68 [69.68–69.69] | 6.52 [6.52–6.70] |
| js2 full parse | 63.50 [63.50–63.51] | 6.46 [6.43–6.48] |
| js2 eager compile | 323.21 [323.21–323.28] | 7.43 [7.39–7.64] |
| QuickJS compile only | 247.32 [247.32–247.37] | 6.53 [6.47–6.56] |

## B. Browser journeys

Both engines ran in one profiling build at the js2 scratch commit. `crates/js` is identical to the OLD checkout. The build used frame pointers and debug symbols. `BARE_JS_EXEC=new` selects js2. `BARE_JS_EXEC=old` falls back to the old tree walker because `old` is not an execution mode in this build. `BARE_JS_EXEC=auto` selects the tiered old engine used in the earlier benchmark. The `auto` comparison controls for that mode difference. [#858 journey benchmark](2026-11-21-js2-vs-old-journeys.md)

- The 44 public inputs are the journeys that passed on both engines in #858. All step files are unchanged from that benchmark. Their Git tree is `36a003426642bb137fb5c4b4e3798cbebfa6e6b7`. The driver uses the URLs in the step comments and runs headless with `BARE_SEED=1`. Replies are live. The same assertion can pass with different fetched assets and script errors.
- Ramps come from the 27 local HTML fixtures in #858, including `churn_1m`. They run over loopback and must set the completion marker. The five controls reuse the earlier function-local loop and large object literals. The typing control types 50 characters into `#a1` on the React fixture. Its launcher requires the literal mode `tree` for the old tree walker.
- Each case uses the first three successful instruction runs and three separate successful RSS and time runs per mode. Modes rotate within each case. Instruction runs use `perf stat -e instructions:u` with at least 99% counter coverage and `glibc.malloc.tcache_count=65535`. The count covers the full process. RSS uses `wait4` with the default allocator and a fresh binary copy for each run.
- Ready time adds step durations through the first action after `open`. For ramps this is the completion check. For typing it is the reported load time. The machine was shared. Treat the wall-time values as observations. Use a controlled idle run for a latency verdict.

Read each cell in the order **`old` tree / `auto` tiered / `new` js2**. Values are minimum instructions, median peak RSS and median ready time. Brackets give the min–max spread where it is visible at the displayed precision. A count below three indicates an incomplete metric. Each total includes only cases with all three metrics complete in both modes of that comparison. The RSS sum adds peaks from separate processes.

### Totals

| Group and baseline | Paired cases | Instructions, G | Sum of peak RSS, MiB | Sum of ready time, s |
|---|---:|---:|---:|---:|
| Public, `auto` → `new` | 43/44 | 43.68 → 36.01 (**0.82×**) | 1,903.50 → 1,533.92 (**0.81×**) | 21.58 → 21.59 (**1.00×**) |
| Public, `old` → `new` | 43/44 | 46.37 → 36.01 (**0.78×**) | 1,887.30 → 1,533.92 (**0.81×**) | 22.21 → 21.59 (**0.97×**) |
| Ramps, `auto` → `new` | 26/27 | 39.89 → 32.09 (**0.80×**) | 1,015.20 → 1,020.18 (**1.00×**) | 3.53 → 2.98 (**0.85×**) |
| Ramps, `old` → `new` | 26/27 | 53.72 → 32.09 (**0.60×**) | 1,016.42 → 1,020.18 (**1.00×**) | 4.75 → 2.98 (**0.63×**) |
| Typing, `auto` → `new` | 1/1 | 6.38 → 7.29 (**1.14×**) | 38.00 → 31.70 (**0.83×**) | 0.07 → 0.07 (**0.99×**) |
| Typing, `old` → `new` | 1/1 | 14.44 → 7.29 (**0.50×**) | 36.51 → 31.70 (**0.87×**) | 0.08 → 0.07 (**0.80×**) |

### Public journeys

| Workload | Instructions, M | Peak RSS, MiB | Ready, ms |
|---|---:|---:|---:|
| `archlinux-org-home` | 80.1 [80.1–81.2] / 80.8 [80.8–81.1] / 77.9 [77.9–78.1] | 23.7 [23.6–23.7] / 24.0 [23.6–24.1] / 22.9 [22.8–23.4] | 723.9 [720.8–729.4] / 727.1 [721.9–739.8] / 727.3 [722.9–753.7] |
| `blog-rust-lang-home` | 155.2 [155.2–155.8] / 155.2 [155.2–170.4] / 154.3 [154.3–155.4] | 25.4 [24.9–25.4] / 24.9 [24.5–25.3] / 24.5 [24.4–24.8] | 92.2 [88.6–92.3] / 91.7 [88.7–92.2] / 92.7 [88.3–93.3] |
| `cnn-lite-front` | 70.5 [70.5–72.1] / 71.0 [71.0–72.0] / 70.5 [70.5–70.7] | 24.0 [23.7–24.0] / 24.4 [23.7–24.5] / 24.3 [24.2–24.4] | 189.3 [188.1–191.4] / 189.6 [181.4–194.6] / 187.7 [187.6–191.5] |
| `craigslist-home` | 374.2 [374.2–2,767.6] / 375.7 [375.7–2,352.6] / 90.4 [90.4–92.2] | 91.0 [90.8–91.1] / 93.6 [30.7–93.7] / 24.3 [23.7–24.4] | 4,208.2 [4,122.9–5,224.9] / 4,202.4 [4,165.6–4,480.2] / 4,483.9 [4,191.3–5,824.0] |
| `crates-io-home` | 500.0 [500.0–820.0] / 84.9 [84.9–89.8] / 98.5 [98.5–98.9] | 44.4 [44.2–44.6] / 25.7 [25.2–26.0] / 25.6 [25.5–25.9] | 961.0 [407.7–983.2] / 379.3 [375.3–963.0] / 365.8 [356.2–368.6] |
| `debian-org-home` | 119.2 [119.2–120.7] / 118.5 [118.5–123.0] / 113.0 [113.0–118.2] | 23.4 [23.3–23.9] / 23.2 [23.0–23.6] / 23.0 [22.8–23.1] | 492.3 [469.5–499.9] / 486.0 [482.1–539.5] / 486.0 [484.7–489.6] |
| `devto-home` | 5,554.8 [5,554.8–6,571.6] / 5,721.9 [5,721.9–6,297.4] / 7,304.2 [7,304.2–8,098.5] | 117.3 [113.5–121.2] / 102.8 [102.6–103.8] / 134.6 [133.8–144.1] | 139.6 [135.5–144.1] / 135.1 [134.7–140.2] / 139.4 [135.4–139.7] |
| `docs-rs-home` | 68.4 [68.4–68.5] / 69.6 [69.6–70.2] / 69.3 [69.3–69.7] | 24.5 [23.9–24.8] / 24.1 [24.0–24.2] / 23.9 [23.8–23.9] | 99.6 [99.4–103.1] / 100.2 [95.1–104.0] / 103.5 [99.4–103.8] |
| `example-learn-more` | 127.4 [127.4–127.5] / 134.6 / 152.5 [152.5–154.1] | 25.2 [25.1–25.3] / 25.3 [25.1–25.6] / 26.0 [25.5–26.2] | 90.8 [88.0–95.7] / 95.1 [94.9–104.1] / 99.5 [96.6–99.8] |
| `example-page` | 27.2 / 27.5 [27.5–27.6] / 26.5 | 22.4 [22.3–22.7] / 22.4 [22.1–22.5] / 21.9 [21.6–22.3] | 99.2 [87.7–104.1] / 103.5 [96.9–103.7] / 96.5 [87.4–100.8] |
| `github-docs-home` | 1,384.7 [1,384.7–2,866.0] / 2,719.2 [2,719.2–2,856.6] / 1,642.9 [1,642.9–2,659.1] | — (2/3) / 87.0 [72.7–87.8] / — (2/3) | — (2/3) / 89.4 [87.1–92.0] / — (2/3) |
| `go-dev-home` | 1,587.4 [1,587.4–1,589.5] / 1,581.4 [1,581.4–2,034.3] / 1,378.6 [1,378.6–4,963.5] | 49.7 [49.5–51.2] / 55.4 [55.2–56.0] / 45.4 [45.4–46.7] | 376.6 [364.5–384.6] / 497.0 [378.6–615.3] / 373.5 [355.6–389.8] |
| `hn-comments-back` | 189.8 [189.8–190.7] / 190.7 [190.7–191.0] / 182.0 [182.0–188.6] | 23.9 [23.8–24.0] / 23.9 [23.5–24.0] / 23.6 [23.3–23.7] | 680.2 [670.6–687.5] / 695.3 [681.6–872.5] / 692.3 [676.9–693.8] |
| `hn-front` | 366.4 [366.4–370.1] / 366.3 [366.3–370.1] / 365.6 [365.6–373.7] | 24.3 [24.0–24.6] / 24.4 [24.3–24.6] / 23.8 [23.7–24.4] | 908.1 [674.9–913.7] / 698.0 [686.5–884.6] / 678.4 [677.1–680.3] |
| `hn-new-past` | 189.9 [189.9–206.0] / 206.0 [206.0–209.9] / 202.7 [202.7–203.1] | 23.1 [22.8–23.4] / 22.9 [22.8–23.0] / 22.3 [22.1–22.8] | 923.1 [919.0–927.3] / 906.4 [902.6–934.9] / 931.9 [926.6–2,169.4] |
| `httpbin-cookies` | 37.0 / 37.1 / 32.9 [32.9–33.0] | 21.6 [21.5–21.6] / 21.6 [21.1–22.0] / 21.0 [20.8–21.0] | 972.9 [933.5–1,465.0] / 950.9 [915.2–966.3] / 949.4 [917.7–960.6] |
| `httpbin-form-fill` | 46.2 / 46.3 / 45.0 | 22.5 [22.3–22.7] / 22.5 [22.5–22.7] / 22.1 [22.0–22.1] | 708.1 [686.7–716.3] / 704.6 [701.3–712.0] / 690.6 [671.8–695.5] |
| `httpbin-form-submit` | 58.1 [58.1–58.3] / 58.9 [58.9–59.0] / 66.0 [66.0–66.6] | 22.8 [22.3–22.8] / 22.4 [22.1–22.6] / 22.5 [22.0–22.6] | 705.8 [699.2–739.1] / 715.7 [703.8–727.1] / 700.4 [672.5–781.7] |
| `kernel-org-home` | 34.5 [34.5–34.6] / 34.6 [34.6–34.7] / 32.1 [32.1–32.2] | 21.5 [21.1–21.6] / 21.8 [21.7–21.8] / 20.8 [20.8–20.9] | 675.3 [667.9–688.5] / 671.5 [668.9–674.3] / 688.0 [676.4–702.1] |
| `lobsters-front` | 141.9 [141.9–145.2] / 138.7 [138.7–143.0] / 136.3 [136.3–138.1] | 23.5 [23.4–24.2] / 23.8 [23.3–23.9] / 23.7 [23.3–23.8] | 812.8 [811.9–819.0] / 793.9 [788.7–814.2] / 793.7 [775.7–827.1] |
| `mdn-reference` | 5,971.9 [5,971.9–6,913.6] / 5,792.5 [5,792.5–5,795.4] / 410.9 [410.9–434.2] | 132.1 [131.6–133.2] / 135.8 [135.8–136.4] / 30.0 [30.0–30.4] | 699.2 [693.5–703.4] / 721.0 [705.5–784.7] / 701.6 [698.9–710.3] |
| `nginx-org-home` | 39.3 [39.3–39.4] / 39.4 [39.4–39.5] / 37.8 [37.8–38.9] | 22.6 [22.5–22.6] / 22.6 [21.9–22.9] / 22.1 [21.4–22.4] | 712.6 [702.1–843.9] / 791.5 [715.8–795.8] / 768.6 [746.0–848.8] |
| `nodejs-org-home` | 910.1 [910.1–1,354.7] / 1,186.3 [1,186.3–1,483.8] / 1,192.0 [1,192.0–1,853.1] | 50.2 [49.5–52.4] / 52.4 [45.6–56.3] / 47.3 [46.5–48.2] | 106.0 [101.7–108.9] / 101.9 [100.8–102.4] / 105.7 [97.4–118.3] |
| `npr-text-story` | 33.0 [33.0–33.2] / 33.0 / 28.9 [28.9–35.2] | 22.1 [22.1–22.6] / 22.3 [22.0–22.4] / 22.1 [21.7–22.2] | 200.8 [192.3–201.1] / 207.1 [191.1–234.0] / 201.5 [193.7–233.7] |
| `paulgraham-home` | 52.4 [52.4–62.2] / 63.2 [63.2–68.9] / 64.2 [64.2–65.5] | 23.0 [23.0–23.1] / 22.6 [22.5–23.6] / 22.6 [22.3–23.1] | 748.3 [747.8–773.9] / 755.1 [753.9–767.5] / 770.3 [764.6–771.8] |
| `python-docs-search-results` | 2,570.3 [2,570.3–2,573.4] / 1,617.5 [1,617.5–2,460.4] / 2,781.2 [2,781.2–2,785.5] | 89.8 [89.3–90.7] / 90.2 [89.1–90.3] / 81.5 [81.3–81.8] | 87.8 [87.4–88.1] / 88.0 [84.0–88.7] / 87.6 [87.5–87.6] |
| `python-docs-search` | 1,217.8 [1,217.8–1,220.7] / 1,104.6 [1,104.6–1,106.9] / 1,485.1 [1,485.1–1,487.9] | 75.1 [74.7–75.4] / 75.1 [74.9–75.3] / 70.1 [69.6–70.2] | 83.6 [83.4–87.8] / 87.4 [83.2–88.2] / 88.0 [87.8–88.3] |
| `python-docs-tutorial` | 245.5 [245.5–250.6] / 254.1 [254.1–257.5] / 269.8 [269.8–271.9] | 24.9 [24.7–25.0] / 24.9 [24.7–25.1] / 24.5 [24.4–24.8] | 146.9 [146.0–149.7] / 143.0 [142.8–150.5] / 148.5 [142.7–151.2] |
| `python-org-docs-link` | 1,178.4 [1,178.4–1,219.7] / 1,248.3 [1,248.3–1,615.4] / 1,567.2 [1,567.2–1,589.3] | 38.2 [37.7–38.2] / 39.1 [38.3–39.2] / 38.2 [38.2–38.5] | 211.6 [195.2–490.8] / 200.7 [194.2–206.5] / 199.3 [193.9–212.1] |
| `python-org-search` | 1,225.9 [1,225.9–1,295.1] / 1,251.4 [1,251.4–1,279.6] / 1,609.2 [1,609.2–1,616.7] | 37.5 [37.2–38.1] / 37.8 [37.3–38.0] / 38.6 [38.2–38.7] | 207.7 [191.5–507.2] / 215.9 [200.0–499.4] / 194.7 [194.2–198.1] |
| `react-docs` | 2,929.6 [2,929.6–3,458.9] / 2,715.1 [2,715.1–3,259.6] / 875.4 [875.4–905.8] | 79.4 [75.1–80.7] / 80.2 [79.9–83.6] / 55.1 [54.5–55.3] | 157.7 [142.4–393.4] / 137.7 [129.7–151.7] / 162.2 [146.4–168.6] |
| `rust-book-chapters` | 435.9 [435.9–482.6] / 526.2 [526.2–588.9] / 604.4 [604.4–813.2] | 29.3 [29.2–29.6] / 30.4 [30.0–30.5] / 29.3 [29.2–29.6] | 212.1 [204.3–252.3] / 301.4 [199.8–301.6] / 204.7 [204.3–211.9] |
| `rust-book-search` | 2,746.8 [2,746.8–2,758.2] / 2,371.6 [2,371.6–2,380.5] / 4,257.2 [4,257.2–4,266.9] | 96.9 [96.5–97.1] / 105.4 [105.4–105.6] / 91.2 [90.8–91.4] | 199.7 [195.4–294.9] / 203.5 [199.8–212.9] / 195.4 [193.2–345.9] |
| `rust-book-sidebar` | 279.5 [279.5–324.3] / 302.6 [302.6–366.8] / 362.8 [362.8–365.1] | 27.7 [27.4–27.7] / 28.4 [28.0–28.4] / 28.5 [28.3–28.5] | 201.1 [199.5–204.7] / 203.9 [192.6–211.2] / 209.6 [200.6–217.5] |
| `rust-lang-org-home` | 83.2 [83.2–83.4] / 87.1 [87.1–87.9] / 107.1 | 24.9 [24.5–25.0] / 25.0 [24.9–25.1] / 25.8 [25.6–26.1] | 83.3 [83.1–87.3] / 82.9 [82.7–87.9] / 87.7 [87.5–87.9] |
| `sqlite-org-home` | 124.9 [124.9–150.9] / 125.5 [125.5–151.3] / 127.3 [127.3–150.4] | 25.7 [23.5–25.7] / 25.9 [25.7–25.9] / 25.5 [25.4–25.8] | 1,223.7 [1,205.5–1,239.5] / 1,199.8 [1,185.1–1,244.6] / 1,217.5 [1,184.4–1,309.2] |
| `substack-home` | 7,837.8 [7,837.8–9,445.7] / 7,417.2 [7,417.2–7,686.4] / 1,685.9 [1,685.9–1,718.3] | 174.7 [166.8–175.9] / 192.3 [191.3–192.8] / 57.0 [56.6–57.3] | 1,021.3 [980.0–1,299.8] / 1,077.9 [945.1–1,912.1] / 1,013.1 [999.7–1,030.6] |
| `w3c-home-links` | 1,306.1 [1,306.1–1,316.5] / 721.6 [721.6–3,141.6] / 384.0 [384.0–385.2] | 29.4 [29.1–29.7] / 29.8 [29.6–30.1] / 30.2 [30.2–30.4] | 83.0 [80.2–87.2] / 83.1 [75.3–87.4] / 84.0 [83.8–87.7] |
| `wikipedia-article` | 1,817.5 [1,817.5–1,925.8] / 1,788.5 [1,788.5–1,906.2] / 1,833.3 [1,833.3–1,849.6] | 41.9 [41.2–42.0] / 42.3 [41.8–42.8] / 39.8 [39.6–40.3] | 410.0 [402.4–582.1] / 371.5 [366.5–2,232.5] / 396.3 [353.5–408.4] |
| `wikipedia-main-random` | 1,137.5 [1,137.5–1,162.0] / 1,124.3 [1,124.3–1,148.4] / 1,157.0 [1,157.0–1,206.9] | 36.3 [36.2–36.6] / 36.5 [36.2–37.8] / 33.6 [33.0–34.5] | 259.5 [249.3–285.2] / 269.4 [267.5–904.1] / 265.6 [259.0–271.3] |
| `wikipedia-portal` | 518.6 [518.6–522.2] / 512.0 [512.0–514.0] / 550.5 [550.5–551.0] | 33.1 [32.8–33.7] / 34.3 [34.1–34.4] / 32.4 [32.2–32.6] | 226.1 [217.9–234.1] / 230.3 [222.3–238.2] / 220.8 [211.0–222.9] |
| `wikipedia-search` | 1,921.1 [1,921.1–1,989.7] / 1,850.5 [1,850.5–1,967.7] / 1,914.7 [1,914.7–2,019.3] | 41.4 [41.1–41.9] / 42.5 [42.4–43.0] / 39.8 [39.6–40.0] | 446.1 [405.8–1,453.1] / 392.2 [361.4–407.1] / 388.8 [385.4–396.9] |
| `wikipedia-toc-languages` | 1,228.7 [1,228.7–1,230.3] / 1,221.1 [1,221.1–1,240.3] / 1,254.9 [1,254.9–1,267.6] | 39.5 [38.6–39.6] / 39.9 [39.9–40.0] / 37.3 [36.8–37.5] | 418.5 [407.8–438.2] / 359.8 [359.7–411.3] / 394.7 [375.6–397.1] |
| `wiktionary-portal` | 833.0 [833.0–836.3] / 827.6 [827.6–831.6] / 852.9 [852.9–855.2] | 37.5 [37.2–37.6] / 38.4 [38.3–38.5] / 35.3 [35.1–35.4] | 206.4 [199.3–226.4] / 209.2 [198.7–220.8] / 205.6 [203.0–220.0] |

### Load-test ramps

| Workload | Instructions, M | Peak RSS, MiB | Ready, ms |
|---|---:|---:|---:|
| `churn_1m` | — (0/3) / — (0/3) / — (0/3) | — (0/3) / — (0/3) / — (0/3) | — (0/3) / — (0/3) / — (0/3) |
| `compm_22000` | 405.2 / 405.3 / 403.0 | 37.8 [37.7–37.9] / 37.8 [37.4–38.6] / 37.5 [37.4–37.6] | 38.4 [38.1–49.8] / 42.6 [39.4–49.9] / 38.2 [37.7–40.4] |
| `compm_24000` | 445.3 / 445.4 / 443.1 | 38.7 [38.3–38.7] / 38.8 [38.7–38.8] / 38.9 [38.4–39.4] | 40.9 [40.5–41.1] / 40.7 [40.6–40.9] / 41.1 [40.6–42.2] |
| `compm_26000` | 482.8 [482.8–482.9] / 482.9 / 480.6 [480.6–480.7] | 40.4 [39.7–41.2] / 40.2 [40.0–40.4] / 40.2 [39.9–40.3] | 44.4 [43.9–45.3] / 44.4 [44.0–55.8] / 44.0 [43.8–45.2] |
| `compm_30000` | 552.6 / 552.7 / 550.4 [550.4–550.6] | 42.8 [42.6–42.9] / 43.1 [42.8–43.2] / 42.9 [42.5–43.1] | 49.6 [49.6–50.4] / 49.5 [49.3–50.1] / 49.2 [49.0–49.5] |
| `compm_34000` | 623.5 [623.5–624.3] / 623.6 / 621.3 | 46.7 [46.7–46.8] / 46.7 [46.5–46.8] / 46.5 [46.2–46.6] | 56.2 [55.3–80.1] / 55.9 [55.7–56.9] / 55.6 [55.5–56.6] |
| `compm_38000` | 693.1 [693.1–693.2] / 693.2 [693.2–694.6] / 691.0 [691.0–692.3] | 49.1 [48.8–49.3] / 49.5 [49.3–49.7] / 48.8 [48.7–49.3] | 61.0 [60.4–61.3] / 61.5 [61.1–62.5] / 61.5 [60.9–61.6] |
| `compm_40000` | 728.2 / 728.3 [728.3–728.4] / 726.0 [726.0–726.1] | 50.5 [50.1–50.5] / 50.2 [50.1–50.5] / 49.8 [49.7–50.5] | 63.8 [63.7–64.6] / 64.1 [63.5–64.3] / 64.3 [63.7–93.7] |
| `compm_43000` | 781.0 / 781.1 [781.1–781.2] / 778.8 | 52.6 [52.3–54.1] / 52.4 [51.6–53.8] / 52.2 [52.1–53.9] | 67.9 [67.7–68.3] / 68.1 [67.8–68.9] / 68.0 [67.4–69.5] |
| `ctor_100k` | 1,061.4 / 904.6 / 812.6 | 20.6 [20.5–20.7] / 20.4 [20.2–20.9] / 20.3 [20.0–20.4] | 100.6 [97.2–100.8] / 78.4 [78.3–81.2] / 68.2 [68.1–68.7] |
| `ctor_10k` | 111.7 / 96.0 / 85.5 | 20.5 [20.3–20.6] / 20.4 [20.3–20.6] / 20.2 [20.0–20.2] | 15.5 [15.3–15.9] / 13.1 [12.6–13.8] / 12.0 [11.8–12.3] |
| `ctor_1k` | 16.7 / 15.2 / 12.8 | 20.6 [20.2–20.8] / 20.6 [20.4–21.0] / 20.0 [19.6–20.1] | 6.6 [6.5–7.0] / 6.2 [6.2–6.6] / 6.4 [6.2–6.6] |
| `ctor_1m` | 10,558.1 [10,558.1–10,558.2] / 8,990.2 / 8,083.7 | 20.6 [20.5–20.9] / 20.6 [20.2–20.6] / 19.9 [19.9–20.0] | 921.8 [918.3–957.0] / 729.5 [726.0–733.6] / 620.3 [611.8–623.3] |
| `ctor_read_100k` | 2,121.2 / 1,897.6 / 1,948.6 [1,948.6–1,949.1] | 63.8 [63.7–64.2] / 64.1 [63.9–64.3] / 67.7 [67.7–68.1] | 233.7 [232.2–239.5] / 204.1 [203.0–205.8] / 225.2 [222.7–232.1] |
| `ctor_read_10k` | 213.8 / 191.6 / 194.4 | 25.5 [25.4–25.6] / 25.4 [25.3–25.7] / 25.4 [25.1–25.5] | 26.6 [26.0–27.1] / 23.9 [23.8–24.1] / 24.3 [24.1–24.4] |
| `ctor_read_1k` | 26.9 / 24.8 / 23.8 | 21.1 [20.6–21.4] / 21.1 [20.9–21.3] / 20.7 [20.5–20.7] | 8.1 [7.8–8.2] / 7.7 [7.7–7.8] / 7.8 [7.6–7.9] |
| `loop_100000` | 195.6 / 95.5 / 36.2 | 20.1 [20.1–20.2] / 19.8 [19.6–20.1] / 19.5 [19.4–19.8] | 19.6 [19.2–19.8] / 11.6 [11.2–12.0] / 7.9 [7.6–8.0] |
| `loop_1000000` | 1,903.8 / 902.8 / 329.6 | 20.1 [19.7–20.4] / 20.5 [19.7–20.5] / 19.5 [19.2–20.0] | 145.1 [144.5–145.3] / 65.4 [64.7–65.5] / 26.0 [25.9–26.2] |
| `loop_10000000` | 18,985.8 / 8,975.8 / 3,263.6 | 20.5 [19.9–20.8] / 20.2 [19.8–20.4] / 19.6 [19.5–19.8] | 1,382.4 [1,381.3–1,388.2] / 601.7 [601.5–1,116.8] / 207.3 [197.2–223.0] |
| `readall_fwd` | 1,559.4 [1,559.4–1,559.7] / 1,489.6 [1,489.6–1,490.1] / 1,530.6 | 78.3 [78.2–78.6] / 78.2 [78.2–78.6] / 71.6 [71.3–71.6] | 204.4 [204.3–213.8] / 196.9 [196.5–217.9] / 206.1 [198.4–211.4] |
| `readall_rev` | 1,524.2 [1,524.2–1,524.6] / 1,460.0 / 1,476.5 | 78.5 [78.1–78.6] / 78.4 [77.8–78.6] / 72.0 [71.7–73.0] | 199.4 [198.5–202.0] / 191.0 [190.8–192.6] / 200.5 [197.4–207.7] |
| `readall_spans` | 1,297.2 / 1,262.5 [1,262.5–1,262.7] / 1,291.4 [1,291.4–1,291.7] | 73.1 [73.1–73.2] / 72.9 [72.7–73.3] / 67.5 [67.4–68.1] | 170.1 [168.1–194.8] / 162.8 [162.1–170.2] / 172.9 [165.8–183.7] |
| `rest_13` | 1,079.5 / 1,079.5 [1,079.5–1,079.6] / 1,078.0 | 52.6 [52.3–52.7] / 52.3 [51.8–52.7] / 52.6 [52.2–52.7] | 136.8 [135.0–137.1] / 134.2 [134.2–135.6] / 132.8 [132.6–132.9] |
| `style_100k` | 2,316.3 / 2,159.6 [2,159.6–2,160.5] / 2,003.9 [2,003.9–2,005.0] | 31.3 [31.0–31.8] / 31.7 [31.1–31.8] / 45.9 [45.5–46.1] | 211.6 [210.2–212.9] / 188.2 [185.9–191.4] / 177.9 [173.5–183.1] |
| `style_10k` | 235.6 / 220.0 [220.0–220.1] / 203.0 | 21.7 [21.3–21.8] / 21.7 [21.6–21.7] / 22.5 [22.5–22.8] | 25.7 [25.4–26.8] / 23.8 [23.6–24.0] / 23.8 [22.8–24.2] |
| `style_1k` | 29.0 / 27.5 / 24.5 | 21.1 [20.5–21.1] / 21.0 [20.7–21.3] / 20.6 [20.4–20.7] | 7.6 [7.2–7.8] / 7.6 [7.0–7.6] / 7.6 [7.1–7.7] |
| `style_250k` | 5,775.4 / 5,383.5 [5,383.5–5,385.8] / 4,993.5 [4,993.5–4,995.2] | 47.6 [47.5–48.0] / 47.3 [46.9–47.6] / 57.7 [57.6–57.7] | 509.2 [505.3–512.5] / 457.7 [453.9–468.2] / 434.5 [429.6–439.7] |

### Typing, local loop and large object controls

| Workload | Instructions, M | Peak RSS, MiB | Ready, ms |
|---|---:|---:|---:|
| `typing` | 14,439.2 / 6,375.7 / 7,288.9 [7,288.9–7,318.3] | 36.5 [36.1–36.8] / 38.0 [38.0–38.1] / 31.7 [31.4–31.8] | 84.0 [84.0–134.0] / 68.0 [67.0–68.0] / 67.0 [67.0–68.0] |
| `loop_local_10000000` | 14,115.8 / 1,985.9 / 1,443.6 | 20.2 [20.1–20.3] / 20.6 [20.1–20.6] / 19.2 [19.1–19.7] | 1,103.0 [1,096.0–1,106.9] / 116.4 [112.7–116.8] / 97.1 [96.0–98.6] |
| `object_1000` | 8.7 / 8.7 / 7.7 | 20.1 [19.8–20.4] / 20.3 [20.2–20.6] / 20.0 [20.0–20.1] | 5.8 [5.5–5.8] / 5.8 [5.6–5.9] / 6.0 [5.7–6.1] |
| `object_2000` | 11.7 / 11.7 / 11.6 | 20.5 [20.4–20.6] / 20.4 [20.3–20.7] / 20.1 [20.1–20.2] | 6.3 [6.1–6.3] / 5.8 [5.6–6.3] / 5.8 [5.8–6.0] |
| `object_4000` | 18.0 / 18.0 / 19.1 | 21.1 [21.0–21.3] / 21.1 [20.8–21.1] / 20.9 [20.6–20.9] | 6.3 [6.1–6.6] / 6.3 [6.2–6.7] / 6.9 [6.8–7.1] |
| `object_8000` | 30.5 / 30.5 / 34.2 | 22.0 [21.9–22.1] / 22.0 [21.9–22.2] / 21.4 [21.3–21.5] | 7.4 [6.9–7.5] / 7.2 [7.0–7.3] / 8.0 [7.8–8.1] |


### Live response sensitivity

The one-minute load before the browser samples ranged from 0.22 to 5.25. The median paired public instruction ratio versus `auto` was 1.00×. js2 used fewer instructions on 21/43 paired journeys. The median paired public RSS ratio was 0.98×. The raw time-to-ready ratios were 1.00× versus `auto` and 0.97× versus the tree walker.

Both engines passed the MDN assertions. The minimum OLD `auto` run made 75 requests and the js2 run made 36. js2 logged a rejected promise. The Substack samples had 142 requests each, within 1% in bytes, and no console errors. Its minimum count fell from 7.42 G to 1.69 G.

The following sensitivity check uses the same minimum samples. The similar-traffic subset requires the same request count, bytes within 1%, and the same counts of console errors and page diagnostics. A replay would be needed to compare identical replies and post-ready work.

| CPU sensitivity | Paired cases | OLD `auto` → js2, G | Ratio |
|---|---:|---:|---:|
| Exclude MDN | 42 | 37.89 → 35.60 | 0.94× |
| Similar traffic and diagnostics | 32 | 26.76 → 23.07 | 0.86× |

### Largest public CPU increases

Ranked by added instructions among the complete pairs. Profiles sample one diagnostic run per engine and journey. They support the hypotheses and show where live work changed.

| Journey | Added M vs `auto` | js2 / `auto` | Observation and hypothesis |
|---|---:|---:|---|
| `rust-book-search` | 1,885.6 | 1.80× | Hypothesis: regexp matching drives part of the increase. The matching routine accounts for 15.8% of the js2 diagnostic profile and 12.6% of OLD's smaller profile. Both minimum runs received the same number of responses and bytes. js2 also emitted `Unsupported("clone_out")` page diagnostics after input. |
| `devto-home` | 1,582.3 | 1.28× | Hypothesis: compiler work and browser animation or style work both matter. The js2 profile showed time in `Fc::finish` and the lexer. Both profiles sampled `browser_motion::read`. The OLD profile was much larger than its minimum run, even though both minimum runs received one 47,816-byte response. |
| `python-docs-search-results` | 1,163.7 | 1.72× | Hypothesis: extra page work accounts for much of the largest difference. The minimum js2 run had 62 requests and 1.80 MB versus 49 requests and 1.23 MB on OLD `auto`. In diagnostics with 62 requests each, the approximate counts were 2.76 G and 2.43 G. Parsing and marking were hot in js2. |
| `python-docs-search` | 380.5 | 1.34× | Hypothesis: the front end still spends more work on data literals. The minimum samples had the same traffic. The js2 diagnostic profile showed parsing, lexing and literal compilation. OLD sampled its direct literal builder. |
| `python-org-search` | 357.8 | 1.29× | Hypothesis: response variation and regexp setup contribute. The minimum js2 run made two more requests than OLD `auto`. With 16 requests each in a diagnostic run, `regexp::Parser::add_class` took a larger share in js2. |

The literal `old` setting gives this ranking.

- `devto-home` added 1,749.4 M (1.31×).
- `rust-book-search` added 1,510.4 M (1.55×).
- `python-org-docs-link` added 388.8 M (1.33×). Hypothesis: regexp setup and script execution contribute. The minimum runs had the same request and byte counts. `regexp::Parser::add_class` was a visible cost in both diagnostic runs.
- `python-org-search` added 383.3 M (1.31×).
- `nodejs-org-home` added 281.8 M (1.31×). Hypothesis: more front-end work drives part of the gap. The js2 diagnostic had hot lexer and parser functions. This row is near parity in the minimum counts against tiered OLD.

### Coverage and controls

All 44 journeys have instruction samples. With GitHub Docs included, the CPU totals are 46.40 → 37.65 G (0.81×) for `auto` and 47.76 → 37.65 G (0.79×) for `old`. GitHub Docs returned HTTP 429 during the sweep and the last retry. OLD tree and js2 each have two successful RSS and ready-time samples there. The complete-pair totals use the other 43 journeys.

The three runs for each mode failed the `churn_1m` completion check. The old engine reached the DOM node limit in both modes. The js2 process reached the check with `window.__done` unset.

The 10M script-global loop went from 8.98 G instructions on `auto` to 3.26 G on js2. The function-local control went from 1.99 G to 1.44 G. The 8,000-key literal cost 34.2 M instructions on js2 versus 30.5 M on `auto`. That js2 control was 1,154.5 M in #858. The builds differ across the two reports. [#858 journey benchmark](2026-11-21-js2-vs-old-journeys.md)

The local ramp RSS totals stayed within 1%. The `style_100k` ramp rose from 31.7 to 45.9 MiB. The React typing control used 14% more instructions and 17% less peak RSS than `auto`. These rows keep the aggregate gains in context.

## C. Estimate for a QuickJS browser adapter

Allow **6–10 engineer-weeks** for release parity with one engineer. Allow **7–12 weeks** if the exact Bellard release requires a Windows port. These are planning estimates. The first milestone is a pinned build that opens a small page, runs a native DOM callback, and can release a detached tree with a listener cycle.

| Work | Estimate | Acceptance check |
|---|---:|---|
| Runtime, values, exceptions, handle scopes, native calls and prototypes | 1–2 weeks | Load a page and run inline scripts, DOM methods and constructors with the correct `this` and `new.target`. |
| DOM lifetime bridge, wrapper identity and exotic properties | 2–3 weeks | Preserve expandos on reachable wrappers. Release unreachable subtrees, listeners and host state under repeated detach and navigation. Verify named and indexed property descriptors and enumeration. |
| Jobs, modules, binary data, clone, interrupts and limits | 2–3 weeks | Match event-loop ordering and rejection reports. Resolve modules via the browser loader. Preserve buffer detach and transfer semantics. Check cancellation, resource limits and structured clone. |
| Build, release and compatibility gates | 1–2 weeks | Build the pinned engine on Linux and Windows. Check browser journeys, lifetime stress cases, code size and cost. |

- **Interface.** `Eng` and `Ops` expose 12 and 103 methods. `Eng::Vm` also implements the 108-method `Engine` trait, which includes default methods. Several operations overlap. Property, call and conversion operations map to QuickJS APIs. The work is in ownership, error translation, virtual DOM properties and browser scheduling. Some host paths still use old-engine types and assumptions. Audit those paths before removing an old engine from the binary. [Browser engine interface](../../crates/bare/src/eng.rs) [Host API](../../crates/js-api/src/lib.rs)
- **Ownership.** bare copies raw IDs and distinguishes persistent roots from edges owned by a wrapper. QuickJS refcounts values and collects cycles. Use owned references for persistent roots and references stored by a QuickJS class. Report each class-owned reference with `gc_mark`. The marks must agree with the retained reference counts. A strong reference in a side table acts as a root unless the cycle collector accounts for it through its owner. Use class finalizers to release references and queue native owner keys for pruning at a safe point. A finalizer cannot run JS. A copied handle must not retain a recycled wrapper or cross a realm unchecked. [QuickJS embedding API](https://www.bellard.org/quickjs/quickjs.html)
- **DOM objects.** `Dom::trace` links wrappers in a detached tree, listeners, expandos and objects such as `style` and `dataset`. Preserve these edges while a wrapper is reachable and preserve one JS identity per native node. A strong wrapper cache can root a whole detached tree. A raw pointer in a cache can outlive its allocation. Use a cache tied to wrapper death with checks for slot reuse. Some owners already exist as native JS objects, such as the global object. The adapter needs a collector-visible attachment for them. A property visible to reflection can change web behavior. Run tests for frame teardown and reentry during property access. Use class exotic methods where they can preserve own keys, descriptors, setters and prototypes. [DOM owner tracing](../../crates/bare/src/browser.rs)
- **Runtime semantics.** Drain jobs at the browser's microtask checkpoints with `JS_ExecutePendingJob` and translate the rejection tracker to browser events. Route module loads through bare. Keep page realms and process limits in the browser. Audit UTF-16 string access, subclassed constructors, external array buffers, deliberately live callback roots and the teardown order. Keep the CLI library and its file and process helpers out of page realms.
- **Binding and Windows.** `rquickjs` wraps QuickJS-NG. Treat it as another engine build and rerun the reference if selected. Its safe values and class tracing help with local lifetimes. The `Copy` raw interface, exotic hooks and loader can still need a sys layer. A pinned low-level binding gives direct access to the tested Bellard release. `hirofa-quickjs-sys` lists MinGW for Bellard and MSVC for NG. bare ships `x86_64-pc-windows-msvc`. Prove that toolchain early with the selected engine revision, including static runtime linkage and C/Rust sanitizer or equivalent boundary tests. Check which C compiler and linker can retain LTO for the browser build. [rquickjs](https://github.com/DelSkayn/rquickjs) [hirofa-quickjs-sys](https://github.com/HiRoFa/quickjs-sys) [bare release build](../../.github/workflows/release.yml)
- **Binary size.** The stripped Linux QuickJS direct-run launcher was 1,038,904 bytes. The stripped `qjs` CLI was 1,211,712 bytes. Use about 1 MiB as the initial size budget for the linked C core, plus the Rust adapter and browser code that the new engine instantiates. Measure the browser binary with each engine enabled alone and together. Shared tables, link-time elimination and third-engine code can change the net size. Windows size remains unmeasured.

## Evidence and limits

The raw samples, scripts, input hashes, launchers and scratch merge bundle are in `/data/bare-tmp/r21/engine-decision/` on `archMachine`. Untested work includes Windows, windowed runs, a full compatibility suite, idle-machine timing and a QuickJS browser build.
