# 2026-11-22 — Public journeys after the js2 fixes

## Method

Measure frozen main at `9a187630f696989a5c8825784f22a9b0b4f93a9a` on archMachine. The host has an AMD Ryzen 9 5900HX and 16 logical CPUs. Use Rust 1.96.0 and perf 7.3.1. Build the `bare-drive` package with `cargo build --release --locked -p bare-drive --bin bare-drive`, `CARGO_BUILD_JOBS=4`, and `CARGO_TARGET_DIR=/data/bare-tmp/r22/remeasure/target`.

Use the URLs and assertions in the [public step files](../../tools/drive/public/). Run `bare-drive --quiet --var URL=<url> <steps>` with `BARE_SEED=1`, `LC_ALL=C`, and `GLIBC_TUNABLES=glibc.malloc.tcache_count=65535`. Set `BARE_JS_EXEC=auto` for OLD and `BARE_JS_EXEC=new` for js2. Reverse the mode order on the second pair. Count `perf stat -e instructions:u` on three passing runs per mode and input. All 36 runs passed with 100% counter coverage.

Copy the DEV and Python.org tapes from `/data/bare-tmp/r22/pages/tape/`. Use the read-only copies with `BARE_NET_REPLAY=<dir>` and `BARE_CLOCK=virtual`. Count the replies in the driver report. Measure peak RSS on three separate passing runs per mode and input with the same allocator settings. Use `/data/claude-tmp/qjs-base/h/rss` to collect peak RSS with `wait4`. Use `--out` on these runs to save the assertion reports.

## Live results

Instruction counts are in millions. Each new cell gives the minimum [min–max]. Each ratio uses the two minima. The before values use the rounded live minima from the [decision rerun](2026-11-22-engine-benchmark-rerun.md). The earlier builds and responses can differ from this run.

| Journey | Before OLD → js2 | Before ratio | After OLD | After js2 | After ratio | Replies OLD / js2 |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| `rust-book-search` | 2,369 → 4,261 | 1.80× | 2,332.43 [2,332.43–2,338.04] | 3,030.22 [3,030.22–3,046.96] | 1.30× | 21 / 21 |
| `devto-home` | 4,974 → 6,110 | 1.23× | 2,430.72 [2,430.72–5,174.73] | 5,224.23 [5,224.23–5,846.46] | 2.15× | 1 / 1 |
| `python-org-search` | 1,246 → 1,494 | 1.20× | 1,218.65 [1,218.65–1,259.74] | 1,245.69 [1,245.69–1,247.47] | 1.02× | 15–16 / 15 |
| `rust-lang-org-home` | 87 → 100 | 1.15× | 83.33 [83.33–85.50] | 87.43 [87.43–87.68] | 1.05× | 14 / 14 |

- Rust Book took 870,514 bytes in each run. Every js2 run reported nine `Unsupported("clone_out")` diagnostics during input. Both modes passed the final search assertion.
- DEV reported one reply and 47,009 bytes in each run. One OLD run used 2,430.72 M instructions. The other two used 5,155.17 M and 5,174.73 M. This spread limits the live comparison. The matched tape gives 1.03×.
- The Python.org instruction minima used 15 replies and 332,196 bytes in both modes. One OLD run took one extra reply.
- Rust-lang.org took 42,114 bytes in each instruction run.

## Tape results

| Journey | OLD instructions (M) | js2 instructions (M) | js2 / OLD | Replies OLD / js2 | Bytes per run |
| --- | ---: | ---: | ---: | ---: | ---: |
| `devto-home` | 3,195.59 [3,195.59–3,195.70] | 3,280.63 [3,280.63–3,281.01] | 1.03× | 16 / 16 | 214,521 |
| `python-org-search` | 1,163.38 [1,163.38–1,163.55] | 1,190.17 [1,190.17–1,190.31] | 1.02× | 17 / 17 | 332,686 |

The [#927 tape minima](../design-js2-real-pages.md#inputs-and-measurements) give 1.04× for DEV and 1.21× for Python.org. The tapes fix the response content and use the virtual clock. The live and tape runs can take different replies before the final assertion.

## Peak RSS

The cells give the median [min–max] in KiB. All 36 RSS runs passed. The reply counts match the instruction runs except for the live Python.org and Rust-lang.org rows. All Python.org RSS runs took 15 replies. One js2 Rust-lang.org RSS run took 10 replies. The other five took 14.

| Journey | Input | OLD | js2 |
| --- | --- | ---: | ---: |
| `rust-book-search` | Live | 106,592 [106,540–106,916] | 99,760 [99,696–99,852] |
| `devto-home` | Live | 61,604 [61,048–109,804] | 111,508 [57,892–113,476] |
| `python-org-search` | Live | 40,156 [39,956–40,448] | 38,856 [38,200–38,904] |
| `rust-lang-org-home` | Live | 22,988 [22,924–23,208] | 22,704 [22,356–22,904] |
| `devto-home` | Tape | 105,812 [99,956–106,248] | 105,044 [102,696–106,612] |
| `python-org-search` | Tape | 39,236 [38,972–39,520] | 37,972 [37,556–38,152] |

## Rust Book profile

Build the same commit with `CARGO_PROFILE_RELEASE_DEBUG=true`, `CARGO_PROFILE_RELEASE_STRIP=false`, and `RUSTFLAGS='-C force-frame-pointers=yes'`. Run js2 once on the live Rust Book journey with Callgrind 3.25.1. Use `--cache-sim=no --smc-check=all-non-file --separate-callers=1`. Raise the step timeout from 30 to 120 seconds in a copy of the step file, as in #927. The profile passes with 21 replies and 870,514 bytes.

Use the [#927 subsystem rules](../design-js2-real-pages.md#cost-split). Assign self instructions by source or symbol. Use the immediate caller for code without a match. Keep unresolved runtime work in Driver and other.

| Subsystem | js2 Callgrind Ir (M) |
| --- | ---: |
| Network, TLS, and URL | 98.64 |
| HTML parse | 3.18 |
| Style | 48.59 |
| Layout and text | 77.90 |
| Paint | 0.66 |
| JS parse and compile | 312.07 |
| JS execution | 1,873.07 |
| Engine-host boundary | 62.90 |
| GC | 13.02 |
| DOM and browser | 34.03 |
| Images | 0.15 |
| Driver and other | 623.22 |
| **Total** | **3,147.44** |

The largest remaining path is `for...in` snapshot setup. [The shape guard](../../crates/js2/src/bi_object.rs) sent 19,153 of 19,303 openings to the snapshot path. That path used 1,263.19 M inclusive instructions, or 40.1% of the profile. Seen-set insertion took 463.23 M, including 227.17 M in rehashing. Key conversion took 262.04 M. The host-aware descriptor lookup took 249.09 M, and the own-key list took 229.76 M. These inclusive counts span several subsystem rows.

The #927 profiles measured 1,268.22 M for js2 snapshot setup and 13.91 M for OLD snapshot setup. The new js2 profile measures the cost of this path. The live ratio comes from the native instruction counts.

## Host load and limits

The start load averages were 0.75, 0.46, and 0.33 at 18:36:39 IST. The end load averages were 3.28, 3.21, and 1.68 at 18:42:29 IST.

This run did not measure wall time, per-fix effects, shape-guard rejection reasons, other journeys, or windowed rendering. It did not run a test suite.
