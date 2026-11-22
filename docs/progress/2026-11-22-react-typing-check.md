# 2026-11-22 — React typing instruction check

The OLD tree walker used 14.44 G instructions. Tiered OLD `auto` used 6.38 G in both reports. Compare js2 with `auto` when `auto` is the baseline.

## Recover the earlier runs

| Report | Browser build | OLD `tree`, G | OLD `auto`, G | js2, G | js2 / `auto` |
|---|---|---:|---:|---:|---:|
| [Decision benchmark](2026-11-21-engine-decision-benchmark.md) | `05b520eb` | 14.4392 | 6.3757 | 7.2889 | 1.143× |
| [Benchmark rerun](2026-11-22-engine-benchmark-rerun.md) | `3c46337e` | — | 6.3756 | 7.6595 | 1.201× |

Both browser builds contain the same `crates/js` tree as `48ccf692`. Both use the same `type_load.rs` and `tools/journeys/triage.py`. The recorded browser binaries came from the two builds above. The `48ccf692` checkout supplied the standalone OLD runner.

Both harnesses run this command for instruction samples. `BIN` is the build's `target-new/profiling` directory. `PAGE` is the fixture directory. The runner starts its own loopback server and uses the real clock.

```sh
BARE_SEED=1 LC_ALL=C GLIBC_TUNABLES=glibc.malloc.tcache_count=65535 \
  BARE_JS_EXEC="$MODE" perf stat -x, -e instructions:u -o "$LOG" -- \
  "$BIN/examples/type_load" "$PAGE" '#a1' 50
```

The archived `r21/triage/typing-page` and `r22/bench/browser/typing-page` agree byte for byte on `index.html`, `app.js` and `style.css`. The default path in `triage.py`, `/data/bare-tmp/triage/typing-page`, has the same files. The saved logs do not record the directory override. The `app.js` SHA-256 is `1992c72031ad2bad490a784172a2e9400b822cb0eb4d0e539f4a021af7c07400`. The two saved `measure.py` files differ only in the engine list. The rerun omits `old`. Both map the `old` label to the literal `tree` for typing.

In the decision logs, the first three `old` launches passed the literal `old` and exited with code 2. The table excludes those samples. The three successful `tree` replacements used 14,439,202,929, 14,439,197,860 and 14,439,199,415 instructions. All six historical `auto` samples fall between 6,375,575,858 and 6,375,748,511. The successful samples show 50 characters, four tasks, four rounds and the same React render counts. The `tree` runs show 1,417,935 evaluator steps per key. Both sets of `auto` runs show 82,227. The counts refer to evaluator steps only. The js2 runner prints zero for this OLD counter.

Raw samples are in `/data/bare-tmp/r21/engine-decision/browser/` and `/data/bare-tmp/r22/bench/` on `archMachine`. See `public.jsonl` and `runs/typing.*` under each browser directory. The rerun also stores the typing row in `summary.json`.

## Reproduction

Measure on `archMachine` (AMD Ryzen 9 5900HX, Rust 1.96.0, glibc 2.44, perf 7.3.1). Build `48ccf692` and `e3817f63` with six Cargo jobs and separate targets.

```sh
CARGO_BUILD_JOBS=6 CARGO_TARGET_DIR="$TARGET" RUSTFLAGS='-C force-frame-pointers=yes' \
  cargo build --locked --profile profiling -p bare --example type_load
```

Main at `e3817f63` differs from the rerun's `3c46337e` only in the report file. Run both OLD modes on `48ccf692`. Also run `auto` on main to share the browser build with js2. The stock runner asserts that load and typing settle and that the value has 50 characters. The sample harness checks exit status, counter coverage, task count and React render counts. The number of scheduler rounds can vary with the real clock.

A separate verification launcher adds event listeners before typing and paints after typing. Three runs in each of the four configurations verify the exact value `abcdefghijklmnopqrstuvwxyzabcdefghijklmnopqrstuvwx`, title `1 done`, progress `1 of 62` and summary `Total characters: 50`. They have no listener error reports. All record one `focus`, one `focusin`, 50 each of `keydown`, `keypress`, `beforeinput`, `input` and `keyup`, and zero `change` events. The 900 × 3,200 pixel raster hash is `a26bbbbd8f51c47e` in each configuration. The stock instruction counts exclude these verification additions.

| Build, mode | Sample 1 | Sample 2 | Sample 3 | Min–max, G |
|---|---:|---:|---:|---:|
| `48ccf692`, `auto` | 6,375,912,970 | 6,375,892,741 | 6,375,912,794 | 6.37589–6.37591 |
| `48ccf692`, `tree` | 14,439,234,265 | 14,439,260,465 | 14,439,218,220 | 14.43922–14.43926 |
| `e3817f63`, `auto` | 6,375,517,268 | 6,375,618,128 | 6,375,442,001 | 6.37544–6.37562 |
| `e3817f63`, `new` | 7,674,662,927 | 7,664,450,845 | 7,665,233,805 | 7.66445–7.67466 |

All 12 samples passed with 100% counter coverage and four tasks. The two scheduler round counts were two and four. Each process made 51 renders each of App, Progress, Choice and Summary, plus 3,060 Field and 102 Area renders. The minimum js2 count is **1.202×** either `auto` baseline and **0.531×** the frozen tree walker.

## Instruction profile

Run Callgrind 3.25.1 on the same symbolized main build in `auto` and `new`. Take one profile at 50 keys and one at zero keys in each mode. Zero keys loads the page and reads the output. The first stock Callgrind run hit the 60-second time budget. The profiling launcher raises the page and session time budgets to 240 seconds. It keeps the real clock. Both 50-key profiles exit successfully with the expected value length and render counts. Each reports five tasks under Callgrind. The hardware samples report four. Replace the `perf stat` part of the instruction command with:

```sh
valgrind --tool=callgrind --dump-instr=yes --callgrind-out-file="$PROFILE"
```

Use `type_load_profile` as the example name and set the last argument to 0 or 50.

The table gives millions of exclusive instructions. Use the function's source file to assign its instructions, including inlined Rust helpers. Assign adapter source lines and host API symbols to the boundary row. This row includes the engine's host-aware property paths. The Rust hash row includes self instructions in hashbrown and the standard hash functions.

| Subsystem | OLD `auto`, 50 | js2, 50 | OLD `auto`, 0 | js2, 0 |
|---|---:|---:|---:|---:|
| JS engine | 6,029.614 | 5,889.934 | 361.518 | 419.494 |
| Engine-host boundary and host-aware lookup | 3.068 | 463.201 | 0.519 | 14.938 |
| DOM and browser input | 28.915 | 48.227 | 4.345 | 5.707 |
| Style | 5.006 | 6.333 | 3.485 | 3.522 |
| Layout | 5.524 | 5.524 | 5.523 | 5.523 |
| Rust hashing and hash tables | 28.138 | 833.960 | 17.987 | 41.556 |
| Other, including allocation, I/O and shared code | 287.985 | 490.983 | 43.730 | 54.928 |
| **Total** | **6,388.250** | **7,738.162** | **437.107** | **545.669** |

The profile gap is 1,349.9 M instructions. The gap after subtraction of the zero-key controls is 1,241.4 M. The stock runner does no final raster pass. Paint symbols account for 66 instructions in each profile.

The direct cost assigned to `eng_new.rs` is 1.503 M. The cost assigned to `eng_old.rs` is 0.119 M. Native callback dispatch and its callees use 53.1 M for 30,552 js2 calls and 46.0 M for 30,544 OLD calls. A separate cost is the js2 host-aware property path. For example, `hs_lazy` and its callees use 527.4 M on 658,671 entries, with 11,918 calls to `Host::lazy_member`. Converting a key to text and attempting to insert it into the lazy-ask set happens before the cache-hit return. The graph totals in this paragraph include callees. Some of those instructions appear in the hash, JS or DOM rows above.

## Root cause

js2 rebuilds the enumeration plan on each `for-in` entry. [`for_in_keys`](../../crates/js2/src/bi_object.rs) walks the prototype chain, builds key lists, converts keys, inserts them into a fresh seen set, reads the descriptors again and builds an array for the loop. `ForInNext` converts and checks each candidate again so it can skip a deleted key. This work is in the JavaScript property runtime.

At 50 keys, `for_in_keys` uses **1,747.9 M** inclusive instructions across **72,406** entries. Its direct calls spend 643.6 M on hash insertion and growth, 358.4 M on key lists, 320.0 M on descriptor reads and 296.7 M on key conversion. Each of insertion, descriptor lookup and conversion runs 1,233,061 times in setup. Next-key checks cost another **197.8 M** across 364,183 calls. The zero-key profile has 1,481 setup calls and uses 41.6 M for setup and checks together. The typing control adds about **1,904 M** of js2 enumeration work.

OLD reads the receiver's shape table in place when the prototype tail has no enumerable keys. Its counters show **72,402** shape iterations and four seen sets at 50 keys. At zero keys they show 1,478 shape iterations and three seen sets. The four list-based calls cost 0.04 M at 50 keys. The 50-key `for_in_step` profile costs 38.4 M. The caller also contains inlined shape setup. The older shape design recorded the same total of 72,406 loop entries on this workload. [OLD for-in design](../design-for-in-keys.md)

## Fix options

1. **Read a guarded shape in place.** Start with ordinary shaped objects with no elements or named index keys and a certified empty enumerable prototype tail. Hold the opening shape and cursor in the iterator. Make the opening shape and its atoms live across collection, including when the receiver changes shape. Check the current shape before the next key. On a change, use the remaining opening keys and generic descriptor checks. Keep the current path for proxies, host objects, dictionaries and indexed objects. This gives constant setup after the tail check and a constant-time step per descriptor. It uses constant iterator storage and no key-list allocation on a hit. Hypothesis: this option can remove most of the 1.90 G added typing work if the js2 receiver mix matches the OLD counters.
2. **Cache an immutable key snapshot.** Store one compact list of keys with the shape sequence and prototype mutation guards. Keep the snapshot alive for open loops. Bound the cache and invalidate entries when GC can recycle shape IDs. Use enumeration checks after a guard changes. A hit reuses the list in constant time after guard validation. A miss still walks the chain and builds a list. Storage grows with the cached key count. Hypothesis: reuse can remove most of the 1.71 G setup added by typing. It can also cover repeated chains with enumerable inherited keys.

Start with the guarded shape path. Count eligible receivers, guards that fail and garbage collections with open iterators before choosing the representation. Test key order, non-enumerable shadowing, deletion, prototype changes, nested loops and collection during a loop. Recheck proxies and host-backed objects on the generic path. Run both the zero-key and 50-key samples three times and compare the other browser and js2 cost rows before merge.

This investigation verifies instruction counts and headless output for one fixture. Windows, windowed input, idle-machine latency, a full test suite and instruction counts after a fix remain unverified. The reproduction logs and profiles are in `/data/bare-tmp/r22/react/` on `archMachine`.
