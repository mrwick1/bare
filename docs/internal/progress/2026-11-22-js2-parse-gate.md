# 2026-11-22 Select the speculative parse worker by engine

The session's engine choice reaches the preload scanner before it starts downloads. The loader creates an OLD parse worker for OLD pages. js2 compiles scripts when they run. Its loader has no OLD worker or parse slots.

A live page retains the scanner's worker choice across turns. A child page forks the network client and keeps the parent's engine type. New loaders for direct pages and resume use that browser type.

## Checks

The new loopback test failed on main because js2 queued an OLD parse. It passes with the gate. All 11 streaming tests pass, including the check that OLD consumes a tree from the worker. `cargo check --locked -p bare-js2 -p bare` passes.

Test262, the full workspace suite, Windows, Miri, and a separate `tree` benchmark were outside this run. This change is in the browser crate.

## Measurements

Compare main `a17eab43` with candidate `682b1417` on archMachine, AMD Ryzen 9 5900HX, Rust 1.96.0, perf 7.3.1. Build both with `CARGO_BUILD_JOBS=3 CARGO_PROFILE_RELEASE_DEBUG=true cargo build --release --locked -p bare-drive`.

Run the public step files with `BARE_SEED=1 LC_ALL=C`. Set `BARE_JS_EXEC=auto` for OLD and `BARE_JS_EXEC=new` for js2. Replay DEV and Python.org with the copied #927 tapes and a virtual clock. Use live requests for Rust-lang.org and Rust Book. Rotate the build and engine order.

Count `instructions:u` with the glibc tcache set to 65,535. Report the minimum and range of three passing runs. Measure peak RSS separately with the wait4 helper and the default allocator settings. Report the median and range of three passing runs. Reply counts match on all four journeys in both modes. One main js2 Rust Book run received 870,647 bytes. The other Rust Book instruction runs received 870,650 bytes.

| Journey | Engine | Replies | Main instructions (M) | Gate instructions (M) | Main RSS (MiB) | Gate RSS (MiB) |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| DEV tape | OLD | 16 | 3196.44 [3196.44–3197.57] | 3197.06 [3197.06–3198.06] | 99.48 [99.22–100.47] | 97.49 [94.73–100.10] |
| DEV tape | js2 | 16 | 3327.13 [3327.13–3327.64] | 3288.38 [3288.38–3289.03] | 94.00 [92.77–97.12] | 92.25 [91.88–99.40] |
| Python.org search tape | OLD | 17 | 1163.34 [1163.34–1163.50] | 1163.31 [1163.31–1163.39] | 34.99 [34.85–35.81] | 35.38 [34.70–35.91] |
| Python.org search tape | js2 | 17 | 1372.26 [1372.26–1372.34] | 1234.05 [1234.05–1234.31] | 34.95 [34.89–35.16] | 33.16 [32.69–33.27] |
| Rust-lang.org live | OLD | 14 | 85.22 [85.22–85.51] | 85.12 [85.12–85.92] | 22.43 [22.21–22.84] | 22.48 [22.44–22.71] |
| Rust-lang.org live | js2 | 14 | 96.82 [96.82–98.43] | 86.92 [86.92–87.32] | 23.09 [22.76–23.33] | 21.74 [21.70–22.23] |
| Rust Book search live | OLD | 21 | 2333.86 [2333.86–2353.63] | 2337.09 [2337.09–2352.92] | 103.96 [103.75–103.97] | 104.10 [103.96–104.35] |
| Rust Book search live | js2 | 21 | 4188.36 [4188.36–4189.99] | 4160.67 [4160.67–4171.26] | 98.45 [97.84–98.53] | 96.66 [96.15–96.71] |

## OLD time to ready

Measure elapsed time from `open` to the first passing assertion that `document.readyState === 'complete'`. Use three passing runs in each build. The host had a one-minute load average of 19.77 on 16 logical CPUs at the start of these runs.

| Journey | Main ready (ms) | Gate ready (ms) | Replies, main / gate |
| --- | ---: | ---: | --- |
| DEV tape | 344.3 [344.0–348.5] | 378.8 [369.2–485.0] | 30 (3 runs) / 30 (3 runs) |
| Python.org search tape | 79.0 [75.4–102.1] | 79.7 [75.9–86.2] | 14 (3 runs) / 14 (3 runs) |
| Rust-lang.org live | 123.5 [123.2–129.1] | 126.9 [121.1–127.4] | 8 (3 runs) / 8 (3 runs) |
| Rust Book search live | 311.1 [292.2–315.0] | 304.2 [286.1–318.2] | 20 (3 runs) / 20 (3 runs) |

The agent stopped before it opened a PR. The owner's session opened it: the OLD code path changes only by one `Option` check, and the OLD instruction ranges overlap on all four journeys. On the DEV tape, each OLD gate run took longer than the slowest main run. All six runs reported 30 replies and 1,400,276 bytes. The timing ranges overlapped on the earlier `5540846b` baseline. The final result crosses the stop threshold. The cause is unverified under shared host load. An idle-host rerun was outside this check. Item 7 of [Next work](../next-work.md) (time to ready on an idle machine) covers it.

## Cost matrix

Run `cost_matrix.sh -a -J -j 3 -g '^examples/(loader_load |session_load |preload_load script )'`. Use the minimum of three instruction counts per build and allocator. All 14 rows pass. The `loader_load modules 1` increase reaches +0.736% on jemalloc and +0.075% on glibc. The debt rule requires a row to exceed +0.5% under both allocators.

| Cost row | Glibc main → gate | Change | Jemalloc main → gate | Change |
| --- | ---: | ---: | ---: | ---: |
| `loader_load blocking 1` | 8,382,769 → 8,393,510 | +0.128% | 15,464,896 → 15,462,720 | -0.014% |
| `loader_load blocking 10` | 28,480,639 → 28,482,729 | +0.007% | 36,298,253 → 36,318,725 | +0.056% |
| `loader_load dynamic 1` | 11,747,888 → 11,768,668 | +0.177% | 18,993,301 → 19,011,576 | +0.096% |
| `loader_load dynamic 10` | 62,886,831 → 62,890,431 | +0.006% | 72,884,896 → 72,900,686 | +0.022% |
| `loader_load mixed 1` | 10,263,091 → 10,248,102 | -0.146% | 17,454,908 → 17,375,526 | -0.455% |
| `loader_load mixed 10` | 46,192,149 → 46,078,435 | -0.246% | 54,688,259 → 54,763,857 | +0.138% |
| `loader_load modules 1` | 9,258,238 → 9,265,142 | +0.075% | 16,945,072 → 17,069,735 | +0.736% |
| `loader_load modules 10` | 26,332,575 → 26,336,645 | +0.015% | 35,291,841 → 35,286,291 | -0.016% |
| `preload_load script 1` | 6,179,783 → 6,180,480 | +0.011% | 13,010,333 → 13,014,241 | +0.030% |
| `preload_load script 10` | 16,050,132 → 16,082,368 | +0.201% | 23,583,352 → 23,530,573 | -0.224% |
| `session_load chain 1` | 5,198,170 → 5,198,556 | +0.007% | 12,201,073 → 12,101,953 | -0.812% |
| `session_load chain 10` | 37,868,724 → 37,880,486 | +0.031% | 46,480,166 → 46,583,060 | +0.221% |
| `session_load heavy 1` | 41,490,385 → 41,494,292 | +0.009% | 47,230,242 → 47,324,913 | +0.200% |
| `session_load heavy 10` | 380,982,575 → 380,987,955 | +0.001% | 397,230,178 → 397,388,741 | +0.040% |
