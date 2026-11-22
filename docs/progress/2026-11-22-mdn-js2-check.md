# 2026-11-22 — MDN journey on js2

js2 stops during MDN startup because `Uint8Array` is missing. On recorded replies, making `auto` stop at the same lookup accounts for all of the apparent instruction savings and 99.8% of the peak RSS savings. Full-page savings remain unmeasured. Exclude this row from an equal-work engine comparison.

## Request and console evidence

The original minimum samples had 75 requests on `auto` and 36 on js2. All 36 shared replies matched by URL, status and wire byte count. The 39 extra requests were 31 JavaScript chunks, two API replies, four SVGs and two failed telemetry POSTs. [Decision benchmark](2026-11-21-engine-decision-benchmark.md)

On `f29a583d`, both engines passed the original title, heading and URL assertions in [mdn-reference.steps](../../tools/drive/public/mdn-reference.steps). In a fresh live pair `auto` made 76 requests and js2 made 36. All 36 js2 replies matched `auto` by URL, status and wire byte count. The extra replies were 31 JavaScript chunks, five SVGs, `/api/v1/whoami` and the Array compatibility data JSON from `bcd.developer.mozilla.org`, all HTTP 200. The other two requests were refused telemetry POSTs with status 0 in the driver. The old console logged two Glean pings and two connection failures. The js2 console logged `Uncaught (in promise) [object Object]`.

The rejected reason was `ReferenceError: Uint8Array is not defined`. An instrumented replay logged the module calls through the Rspack runtime and read the rejection reason at `unhandledrejection`. The reason had no `stack` property in js2. The observed module call chain was `33779 → 41857 → 70955 → 98218 → 31971`. The trace reached the entry and telemetry initialization in `index.029f7c24d22174b5.js`, then a dependency in `4585.7535b1f199327a48.js`, then the UUID module in `1216.adee8e88502c83c4.js`. Module 31971 allocates `new Uint8Array(16)` when it loads, before the UUID helper can select its `crypto.randomUUID()` fast path. The entry promise rejects before it calls module 31557, which imports the page's custom elements. The compatibility data is one example of application work that is skipped. Browser-specific request differences remain unverified.

The same rejection and missing chunks reproduced with replies recorded from `auto`. The complete replay had zero misses in every measurement run. Live response differences and telemetry connection failures do not account for the JavaScript failure.

## Measurements

All runs used `f29a583d` on archMachine, AMD Ryzen 9 5900HX, Linux 7.2.15 and Rust 1.96.0. The locked build used the profiling profile, frame pointers and four Cargo jobs. The driver ran headless with `BARE_SEED=1`. Each row uses the minimum of three `perf stat -e instructions:u` samples with at least 99% coverage and the median of three separate `wait4` peak RSS samples. Brackets give the range. The instruction samples used `glibc.malloc.tcache_count=65535`. The RSS samples used the default allocator and a fresh copy of the binary. Replays used `BARE_CLOCK=virtual`.

| Input and mode | Requests, all runs | Instructions, M | Peak RSS, MiB |
|---|---:|---:|---:|
| Live, `auto` | 75–79 | 5,788.0 [5,788.0–5,827.4] | 135.6 [133.9–135.8] |
| Live, js2 | 35–36 | 408.7 [408.7–411.4] | 29.8 [29.3–29.9] |
| Recorded, `auto` | 75 | 6,577.8 [6,577.8–6,580.1] | 136.5 [135.8–137.8] |
| Recorded, js2 | 40 | 390.5 [390.5–390.7] | 29.1 [29.1–29.4] |
| Recorded with the same failure, `auto` | 40 | 385.2 [385.2–385.4] | 29.4 [29.2–29.8] |
| Recorded with the same failure, js2 | 40 | 390.3 [390.3–390.6] | 29.2 [28.9–29.3] |

The control adds `delete globalThis.Uint8Array;` immediately before the UUID module's allocation in the recorded response. No repository code changes. Both engines took the same error path. All 12 control samples had identical request multisets by URL, status and wire byte count. `auto` reported `ReferenceError: Uint8Array is not defined` with a script frame. js2 produced the original rejection message. The replay control keeps the original journey assertions and does not wait for extra work after them.

The forced failure saved 6,193 M instructions and 107.0 MiB within `auto`. At the same failure, js2 used 5.2 M more instructions and 0.27 MiB less RSS than `auto`. The control accounts for 100.1% of the apparent instruction savings and 99.8% of the apparent RSS savings. These percentages use the replay with the forced failure in both engines for the remaining difference. They do not predict performance after the page can finish.

## Fix plan

Stop at the missing feature. The existing typed-array PR adds more than 1,400 lines in its two implementation files alone. It was open at the time of the check. [Typed arrays PR #863](https://github.com/mrwick1/bare/pull/863)

1. Land the typed-array implementation. Add or verify a browser regression in its own test file that creates a `Uint8Array` at module initialization and proves the entry continues.
2. Replay the same tape. Verify that the Array compatibility data and custom-element chunks load. Investigate any subsequent rejection before measuring parity.
3. Run both engines three times on the same replies after the requests and diagnostics match. Publish the minimum instructions and median peak RSS. Recompute the browser total with the replacement MDN row.

Raw driver reports, typed request lists, rejection traces, the tape and measurement logs are under `/data/bare-tmp/r22/mdn/` on archMachine. Windows, a reference browser and the unmerged typed-array implementation were not tested.
