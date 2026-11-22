# Reduce js2 instruction cost on public pages

## Decision

First, stop sending classic scripts to the OLD parse worker in js2 sessions. The local switch saved 39 M on the DEV tape, 140 M on the Python tape, 24 M on live Rust Book, and 10 M on live Rust-lang.org. Keep the worker for OLD.

The largest remaining Rust Book cost is enumeration. js2 spends 1,268 M instructions in `for...in` setup and OLD spends 14 M in its list path. Two prototype guard probes gave no savings. The numeric-loop control removed most of the extra work. Start the enumeration implementation by passing internal keys and attributes through the snapshot builder.

## Inputs and measurements

The browser source was frozen at `e3817f63d72c3ed5477fc54b2d638e990577a0b6`. The browser and engine crates match the source at `3c46337e` in the [decision rerun](progress/2026-11-22-engine-benchmark-rerun.md). Both modes ran in the same release `bare-drive` binary with symbols and frame pointers. The host was archMachine, AMD Ryzen 9 5900HX, Rust 1.96.0, perf 7.3.1, and Callgrind 3.25.1.

```sh
CARGO_BUILD_JOBS=6 CARGO_TARGET_DIR=/data/bare-tmp/r22/pages/target \
  CARGO_PROFILE_RELEASE_DEBUG=true CARGO_PROFILE_RELEASE_STRIP=false \
  RUSTFLAGS='-C force-frame-pointers=yes' \
  cargo build --locked --release -p bare-drive -p bare --bin bare-drive --bin bare
```

Use the URLs in the [public step files](../tools/drive/public). Run `bare-drive --quiet --var URL=<url> <steps>` with `BARE_SEED=1`, `LC_ALL=C`, and `GLIBC_TUNABLES=glibc.malloc.tcache_count=65535`. Set `BARE_JS_EXEC=auto` for OLD and `BARE_JS_EXEC=new` for js2. Rotate the mode between trials. Read the JSON report for assertion results, replies, and page diagnostics.

Each cell gives the minimum of three passing `perf stat -e instructions:u` runs and the min–max range, in M instructions. Counter coverage was 100% in all 24 runs.

| Live journey | OLD auto | js2 new | js2 / OLD |
|---|---:|---:|---:|
| Rust Book search | 2,376.2 [2,376.2–2,384.4] | 4,270.5 [4,270.5–4,286.3] | 1.80× |
| DEV home | 4,641.0 [4,641.0–4,928.1] | 5,346.6 [5,346.6–5,989.5] | 1.15× |
| Python.org search | 1,248.7 [1,248.7–1,322.5] | 1,501.1 [1,501.1–1,513.5] | 1.20× |
| Rust-lang.org home | 87.1 [87.1–87.7] | 99.7 [99.7–100.0] | 1.14× |

- Rust Book reported 21 replies and 870,650 bytes in every trial. js2 reported nine `Unsupported("clone_out")` diagnostics during input. OLD reported no corresponding diagnostics. Both engines passed the search-result assertion.
- DEV reported one reply and 47,600 bytes in every trial. The report records replies the page took before the final assertion. A separate recording saved 66 replies, including background work. The earlier decision rerun had 47,536 bytes and a 1.23× ratio.
- Python.org reported 17–19 replies on OLD and 18–19 on js2. With the same recorded replies and a virtual clock, the minima were 1,192.1 and 1,442.1 M. All six replay runs had 17 replies and 332,686 bytes.
- Rust-lang.org reported 14 replies and 42,114 bytes in every native trial. The tape missed the live redirect sequence for `/learn`. Use the live profiles for this row.

### Cost split

Units are M instructions. Callgrind uses whole-process `Ir` with `--cache-sim=no --smc-check=all-non-file --separate-callers=1`. Assign exclusive instructions by the symbol or source line. Use the immediate caller for unclassified code. Keep unresolved allocator, hash, and runtime work in Other. The boundary row includes code in `eng*.rs` and `api*.rs`, including checks that stay in the engine. The GC row uses the same exclusive attribution as the other rows.

The first OLD Rust Book profile hit the 30 s driver limit. The retry used a 120 s limit and both engines passed. The longer limit also applied to the other Callgrind runs. A focused run used `--collect-atstart=no --toggle-collect=bare_js2::bi_object::*::for_in_keys` and counted 1,269.16 M in enumeration setup. DEV's first live Callgrind pair took different replies and finished at 741 and 823 M `Ir`. Both values were far below their native counts. Use the matched DEV tape and the sample split from native DEV runs.

| Subsystem | Rust Book live `Ir` OLD → js2 | DEV live sampled OLD → js2 | DEV tape `Ir` OLD → js2 | Python tape `Ir` OLD → js2 | Rust-lang.org live `Ir` OLD → js2 |
|---|---:|---:|---:|---:|---:|
| Network, TLS, and URL | 95.32 → 98.48 | 485.51 → 502.51 | 272.49 → 270.18 | 35.02 → 35.37 | 24.53 → 24.13 |
| HTML parse | 2.89 → 3.12 | 21.00 → 20.50 | 19.64 → 19.65 | 9.07 → 9.06 | 2.86 → 2.63 |
| Style | 52.40 → 45.60 | 920.52 → 892.02 | 550.26 → 548.54 | 309.26 → 307.06 | 14.81 → 15.07 |
| Layout and text | 83.90 → 83.90 | 168.50 → 262.00 | 107.45 → 107.45 | 321.90 → 321.90 | 7.06 → 7.06 |
| Paint | 0.66 → 0.66 | 0.00 → 0.00 | 0.08 → 0.08 | 0.29 → 0.29 | 0.09 → 0.08 |
| JS parse and compile | 259.58 → 344.42 | 835.02 → 2,051.54 | 740.95 → 1,160.76 | 185.84 → 445.69 | 8.37 → 24.29 |
| JS execution | 1,623.90 → 2,940.51 | 1,098.02 → 553.01 | 576.25 → 197.39 | 174.52 → 130.95 | 14.70 → 9.77 |
| Engine-host boundary | 0.92 → 62.99 | 1.00 → 45.00 | 0.92 → 20.78 | 0.93 → 21.08 | 0.50 → 1.28 |
| GC | 74.32 → 12.49 | 0.00 → 7.00 | 0.00 → 4.16 | 0.00 → 1.26 | 0.00 → 1.19 |
| DOM and browser | 41.01 → 33.51 | 937.02 → 852.52 | 236.49 → 237.85 | 75.99 → 76.81 | 4.20 → 4.35 |
| Images | 0.15 → 0.14 | 126.00 → 259.50 | 412.30 → 412.30 | 13.51 → 13.51 | 0.12 → 0.12 |
| Driver and other | 195.05 → 675.45 | 42.50 → 54.00 | 463.41 → 545.71 | 123.51 → 134.98 | 18.64 → 17.31 |
| **Total** | **2,430.10 → 4,301.27** | **4,635.08 → 5,499.10** | **3,380.25 → 3,524.84** | **1,249.85 → 1,497.97** | **95.88 → 107.29** |

For live DEV, run `perf record -e instructions:u -c 500009 --call-graph fp --user-callchains`. Attribute each sample to the closest classified frame. Give GC priority if the stack has a collector frame. The table uses the lowest sampled total of each mode. The reports for that pair had 47,669 bytes. Sample counts were 9,270–12,056 for OLD and 10,998–12,042 for js2. The compile estimates were 832–839 M and 2,052–2,058 M across the three profiles. The exact counts remain the `perf stat` measurements above. Skid and partial stacks limit the accuracy of individual symbols.

The DEV tape and Python tape use `BARE_NET_REPLAY=<dir>` and `BARE_CLOCK=virtual`. DEV has 16 replies and 214,521 bytes in both modes. The three native replay counts spanned 3,269.9–3,270.5 M and 3,411.9–3,412.3 M. Python's equivalent `Ir` delta was 248 M. Its live `Ir` delta was 246 M. The Rust-lang.org live profile reported 15 replies on OLD and 14 on js2. A background reply reached OLD before the journey ended.

Rust Book inclusive GC cost was 122.47 M for OLD and 39.67 M for js2. Some collector descendants appear in other rows. Small reported paint counts apply to the headless driver. They give no estimate for a windowed browser.

## Rust Book

The index response expands to a 4.71 MB script. It calls `JSON.parse` to load the elasticlunr index. The search page uses `for...in` during search and teaser generation. It splits document bodies into sentences and words. It calls the Porter stemmer on the words. Stemmer operations include repeated `RegExp.test` on the same words. Teaser rendering calls `substring` on the document body.

### Enumerate ordinary objects without walking clean prototypes

[`Realm::for_in_keys`](../crates/js2/src/bi_object.rs) takes a snapshot. It visits each object in the chain, including the receiver. It makes a vector of own string keys, converts each key back to an internal key, inserts it in a SipHash set, then retrieves the descriptor to check enumerability. It snapshots the result in a JS array. At each step it checks if the key is still present and enumerable. For a list of `K` candidate keys, setup uses expected O(K) hash and descriptor operations and O(K) scratch memory. Numeric names can also incur sorting and conversion.

| Direct call path | OLD M / calls | js2 M / calls |
|---|---:|---:|
| Enumeration snapshot | 13.91 / 19,154 | 1,268.22 / 19,303 |
| Snapshot → host-aware own descriptor | — | 250.22 / 977,383 |

OLD certifies prototype tails that have no enumerable keys. It reads own keys with their attributes and delays the shadow set until a prototype supplies keys. It also has a cursor for eligible receiver shapes. The function count excludes OLD opens that take the shape cursor. See [OLD enumeration](../crates/js/src/eval.rs) and [OLD heap](../crates/js/src/heap.rs). js2 called `get_own_h` during enumeration of ordinary JS objects. Its entire direct `host_call` path, including callees, took 66 M over 1,286 calls. The large enumeration cost does not require a DOM call.

**Local experiment.** The first engine probe reused the array-push prototype proof. Its three live js2 runs were 4,272.0–4,276.2 M versus 4,264.2–4,271.3 M on the base. A second probe checked each prototype in the chain. It ran at 4,286.7–4,297.7 M live and also gave no savings with the tape. The focused profile showed one fast-path call in 19,303 opens. It still spent 263 M in `key_of` and 249 M in `get_own_h` under enumeration. Check which objects use the slow path before adding a prototype-only shortcut.

A separate replay replaced the three `for...in` loops over dense arrays in `makeTeaser` with numeric loops. The original engine binaries ran both tapes. The same final search markup had 14,840 code units and digest `-983816011` in all 12 runs. The change affects only the local copy of one response and its compressed length.

| Replay with result digest | OLD native M | js2 native M |
|---|---:|---:|
| Original | 7,573.1 [7,573.1–7,573.2] | 17,854.0 [17,854.0–17,866.0] |
| Numeric loops | 7,361.4 [7,361.4–7,361.5] | 10,301.1 [10,301.1–10,301.5] |

The virtual clock lets the index arrive during typing. This runs more searches than the live journey. Use the A/B result to identify the redundant work. Use the live profile to estimate the gain for the original journey. The numeric loops also avoid string-to-index conversion on element access. The 7.55 G saving therefore includes more than snapshot setup.

| Design | Work and storage | Expected gain and risks |
|---|---|---|
| Guard a snapshot of own enumerable keys. | For an ordinary receiver, prove that its prototypes have no enumerable string keys. Reuse the existing own-key order and per-key deletion check. Record a proof by prototype epoch and check host flags and element state at use. A hit uses O(E) scratch and an O(E) key array for E own keys, with existing numeric sorting. It needs no shadow set. | Both probes gave about zero gain. Measure guard eligibility before estimating more. Check prototype mutations without shape changes, numeric and accessor descriptors, non-enumerable shadows, proxies, and open iterators across GC. |
| Pass internal keys and descriptor attributes through enumeration. | Read contiguous shape entries and the indexed element record directly. Accumulate ordered keys and attributes. Make the shadow set only when a prototype contributes candidates. Convert numeric keys when they are yielded. Expected O(K) work and O(K) scratch in the general case, with less formatting and hashing. | Hypothesis: 0.8–1.2 G on Rust Book, up to 35 M on Python, and low single-digit M on DEV and Rust-lang.org. It touches more object kinds. Preserve trap order, indexed property ordering, and shadowing. |
| Use a shape cursor and shared enum keys. | Store the ordered enumerable names in the shape table. A cursor keeps the receiver and position, with a shape and prototype guard. For packed elements, emit numeric keys from a length snapshot. Open in O(1) on a valid cache hit. Use O(E) storage per cached shape. | Hypothesis: up to 1.27 G on Rust Book plus part of the iteration cost. Other measured pages have much smaller enumeration scopes. Mutation or a reused shape id must not expose new keys or keep deleted keys. Pin or trace the snapshot. |

V8 uses cached descriptor keys with an empty-prototype check. It also has a holder for packed elements and named keys. This is a reference for the guard and cursor designs. [V8 key collection](https://chromium.googlesource.com/v8/v8.git/+/459b9aeff9f63fdb632c43f50e725e0de15d1462/src/objects/keys.cc) · [V8 for-in entry](https://chromium.googlesource.com/v8/v8.git/+/main/src/runtime/runtime-forin.cc)

### Avoid repeated regexp matches

Both engines ran 165,966 calls to the builtin `RegExp.test`. OLD spent 403.35 M inclusive and js2 spent 807.18 M. Total calls into the match loop, including other regexp operations, were 64,711 and 175,041. OLD memoizes test results by compiled pattern ID, subject handle with a generation, and start index. A hit recharges the recorded steps. js2 caches 32 compiled programs by source cell and flags. Its builtin test still calls the matcher for each subject. The match loop itself used 322.48 M self on OLD and 724.38 M on js2.

| Design | Work and storage | Expected gain and risks |
|---|---|---|
| Bounded test-result table. | After the existing `lastIndex` read and conversion and the builtin-exec guard, probe by program, subject content or identity, and start. Save end or no match and fuel cost. Check the input on a content-hash hit. Charge saved fuel and keep `lastIndex` writes. Use fixed-size entries, allocate on demand, and clear at GC or use generation-checked weak references. Expected O(1) probe for identity keys. | Hypothesis: 150–400 M on Rust Book if hit rate approaches OLD. Fresh js2 strings can reduce identity hits. Use an explicit byte cap, measure retained subjects, and test fuel boundaries and user-defined `exec`. No material gain assumed for the other three pages. |
| Specialize the repeatedly run match programs. | Use a match-only compiled form for eligible patterns. Reuse the `Blob` image and apply the bounded linear tier from the [regexp design](design-js2-regexp.md). Store the transition table in a capped per-program block. Fall back for backreferences and complex captures. | Hypothesis: 180–370 M on Rust Book if eligible patterns account for one quarter to one half of the 744 M inclusive core cost. Count eligible pattern work first. Preserve bounds, match spans, and budget accounting. No material gain assumed for the other three pages. |

V8 shares regexp data by source and flags. WebKit also has a match-only compile path. These are separate design choices from memoizing a subject and start position. [V8 regexp data cache](https://chromium.googlesource.com/v8/v8.git/+/main/src/objects/compilation-cache-table.cc) · [WebKit RegExp](https://github.com/WebKit/WebKit/blob/main/Source/JavaScriptCore/runtime/RegExp.cpp)

### Stop copying full strings and avoid temporary JSON keys

The two JSON parses took 446.63 M in OLD and 788.66 M in js2. The index dominates the input. In the matching recorded script, the index contains 352,801 properties and 526 distinct key texts. 105,197 keys are decimal. js2 allocates a UTF-16 vector for each parsed key, makes a JS string, classifies it with `key_of`, and calls the generic define path. An indexed key on a plain object becomes a decimal atom again. It also collects the entire input into a UTF-16 vector. OLD interns from a slice or reusable scratch, caches repeated keys, and handles short exact integers in the scanner. See [js2 JSON](../crates/js2/src/bi_json.rs) and [OLD JSON](../crates/js/src/json.rs).

[js2 substring](../crates/js2/src/bi_string.rs) calls `Realm::units` before it applies the requested bounds. In the Rust Book profile, 1,770 calls took 202.76 M, of which 201.76 M was in `units`. Allocation and copying scale with source length. OLD gets the length, then copies the selected range.

| Cause and design | Work and storage | Expected gain and risks |
|---|---|---|
| Substring A. Reuse the flat string range helper. | After coercions, flatten once if needed. Use the existing [range helper](../crates/js2/src/regexp_rt.rs) to copy the selected one- or two-byte units. A flat source costs O(output length) time and memory. | Hypothesis: 150–200 M on Rust Book, low single-digit M on the other three. Preserve argument coercion order and keep the source rooted across it. |
| Substring B. Add a range walker for ropes. | Keep a bounded stack of rope nodes and copy only intersecting spans. Use O(depth + output length) memory and time proportional to the visited nodes and copied units. | Hypothesis: up to 200 M plus savings on large unflattened sources in other journeys. There is no measured extra gain over A. Add this only if rope flattening remains visible. |
| JSON A. Parse borrowed unit spans with a reusable key buffer. | Dispatch once on one- or two-byte input. Decode escaped keys into one reusable buffer. Classify decimal keys at scan time. Probe the atom table by key text before allocating. Use O(input length) scanning and O(depth + longest escaped key) scratch, plus the result. | Hypothesis: 120–220 M on Rust Book. No gain assumed on the other three. Check lone surrogates, escapes that change key identity, canonical index bounds, duplicate keys, and `__proto__`. |
| JSON B. Build objects from staged descriptors. | Extend A with a frame of parsed key/value records. Reuse shape transitions for repeated key sequences or build a dictionary table once for new wide objects. Use expected O(K) insertion and O(K) staging for K properties in an open frame. | Hypothesis: 200–340 M on Rust Book, including A. No gain assumed on the other three. Preserve duplicates and numeric ordering, bound scratch, keep values live, and run revivers in the current order. |

V8 builds JSON objects from staged properties and uses map feedback for repeated layouts. WebKit's literal parser dispatches on code-unit width. [V8 JSON parser](https://chromium.googlesource.com/v8/v8/+/50eba5e6c269c71d6e0e758b84fafe8d5c37d210/src/json/json-parser.cc) · [WebKit literal parser](https://chromium.googlesource.com/external/github.com/WebKit/webkit/+/93d3de050525026eea10afe7a83f3003a7e44ed1/Source/JavaScriptCore/runtime/LiteralParser.cpp)

## DEV, Python.org, and Rust-lang.org

### Use the selected engine for speculative script parsing

[`Net::start_with`](../crates/bare/src/browser_loader.rs) submits downloaded classic scripts to `bare_js::parse_ahead` in both modes. OLD can consume the finished tree through its parse cache. js2 compiles the source through `script_code_in` and its own [compiler](../crates/js2/src/script.rs). It has no consumer for the OLD AST. The worker stores a source copy and a tree behind a slot keyed by content and parse kind. A parse costs work and memory proportional to source size. Each thread can start a parse before the page takes the reply.

| Worker cost | OLD | js2 |
|---|---:|---:|
| Rust Book live Callgrind | 26.59 M | 26.58 M |
| DEV native sampled range | 348.5–447.0 M | 440.5–445.5 M |
| DEV tape Callgrind | 38.83 M | 38.83 M |
| Python live Callgrind | 70.48 M | 140.95 M |
| Rust-lang.org live Callgrind | 5.02 M | 10.05 M |

**Local experiment.** A switch on the second probe build disabled OLD submissions. The comparison uses the same binary with the switch off and on. It retains the same js2 code in both runs. Three passing samples per setting give the ranges below.

| Journey | OLD worker active, js2 M | OLD worker skipped, js2 M | Minimum reduction |
|---|---:|---:|---:|
| Rust Book live | 4,286.7–4,297.7 | 4,263.1–4,273.4 | 23.6 M |
| DEV tape | 3,411.4–3,412.5 | 3,372.6–3,373.3 | 38.8 M |
| Python tape | 1,442.3–1,442.5 | 1,301.8–1,302.0 | 140.6 M |
| Rust-lang.org live | 99.58–100.36 | 89.51–89.99 | 10.06 M |

Live DEV varied from 5,585 to 7,969 M with the worker and from 4,895 to 5,122 M without it. The report's first reply was 47,442 bytes in all six runs. Background work remains a confounder. Use the 38.8 M tape result for the confirmed gain and the 440 M sample estimate as a live target to check.

The DEV tape also measured 656.44 M in js2's lazy parse at unit load and 360.48 M in child parse and compile under `Loader::children`. First call lazy compilation does more work. These scopes explain why the compile increase exceeds the OLD worker cost. The bytecode and built-in execution bucket fell by 379 M on the tape. Image work and background CSS work vary on live DEV. Their variation does not explain the repeatable 420 M increase in the tape's compile bucket.

| Design | Work and storage | Expected gain and risks |
|---|---|---|
| Select the worker at session creation. | Keep OLD's worker for OLD. For js2, skip the OLD submission and compile only when the script runs. Use the session engine choice, including retained and forked clients. Use one discriminant and no OLD tree slots in js2. | The switch gives the reductions above. Measure live DEV with matched replies and check time to ready before rollout. |
| Use the same queue for engine-specific artifacts. | Give a js2 worker a portable unit and facts from the existing [portable code cache](../crates/js2/src/code_cache.rs). The realm imports the artifact only when source and mode match. Compile on demand if the queue is full or the page cancels. Use bounded in-flight source and artifact bytes. Preserve OLD's existing slot format. | Same removal of OLD work. Potential additional live latency gain is unverified. Unused js2 units can erase CPU gains on early-stop journeys. Verify syntax error order, ownership, realm binding, and cache eviction. |

After engine selection, profile child compilation by reason. If eager descendants account for the excess, use compact deferred metadata for functions that need `super`, or retain bounded parsed body data for the functions that are called early. The first option shifts compile work to first call. The second retains more parser memory to avoid a second parse. The 360 M DEV tape child scope bounds the direct gain from this part. No estimate for median or the other three pages is verified. See the [parse-once results](progress/2026-11-22-js2-parse-once-metadata.md).

### Avoid building regexp classes twice

In the Python live profile, js2's `Parser::add_class` took 121.33 M self versus 77.40 M in OLD's matcher parser. A separate OLD worker took another 13.14 M in this function on js2. js2 calls `regexp::check` from `new_regexp` on creation, costing 80.55 M in 329 calls. It parses again when the program cache misses, costing 46.27 M in 111 calls to `compile`. `add_class` is inside these scopes. On Rust-lang.org, creation-time checks took 1.69 M. The regexp match loop is a smaller concern on these two pages.

| Design | Work and storage | Expected gain and risks |
|---|---|---|
| Cache a successful check with the program. | Key a bounded table by source and flag bits. A verified cache hit allocates only the fresh wrapper and initializes `lastIndex`. Retain a full syntax check on misses, including any constraints skipped by the early literal checker. | Hypothesis: up to 80 M on Python, 9 M on Rust Book, 2 M on Rust-lang.org, and a small part of the DEV compile bucket. Count content hits as well as pointer hits. Keep cache entries within GC lifetime. |
| Share the first parse with compilation. | Keep a compact validated pattern record until first use, or emit the program immediately for a bounded small pattern. One parse changes O(pattern length) work to one pass instead of two. A deferred record retains tree or image bytes for unused patterns. | Hypothesis: up to 46 M on Python from removing the second pass, with further gains if repeated checks are shared. Rust Book and Rust-lang.org have low single-digit M in compilation. Measure DEV first. Preserve when errors and limits appear, and preserve fresh wrapper state. |

## PR order and checks

1. Gate the OLD speculative parse queue on the selected engine. Count queued, started, used, and discarded parse units by engine. Recheck DEV live against matched tapes, Python navigation, Rust-lang.org navigation, and OLD's worker ready-time benefit.
2. Pass internal keys and attributes through the snapshot builder for ordinary objects. Keep exotic objects on the existing path and retain `ForInNext` checks. Count conversions, descriptor probes, and prototype keys. Compare result links and text on the Rust Book tape. Test mutations, shadowing, sparse indices, and exotic objects. Use the same regression assertion before and after.
3. Replace full-source copying in the substring family. Check one- and two-byte results, empty ranges, surrogate halves, coercions, and large sources.
4. Add regexp setup reuse for Python. Count matches and object creations separately. Keep the test-result cache or matcher tier in a separate PR. Run only the affected regexp and execution cost rows and targeted compatibility cases.
5. Reduce JSON key work. Measure a corpus with repeated keys, wide objects, and non-ASCII keys before adding a staged builder.

After each implementation, rerun the four journeys three times under both modes on a fresh frozen main. Reuse response tapes and include a live check. Before the rollout decision, measure the full public cohort and load-test ramps with the original assertions and traffic checks. Compare instructions, peak RSS, and time to ready. Use an idle machine for latency.

This report follows item 1 in [Next work](next-work.md). The [rerun summary](progress/2026-11-22-engine-benchmark-rerun.md) has a median ratio of 1.0117 across 43 complete public journeys. If only the four measured rows changed by the gains above, the median would still be 1.01× at the report's precision. A median gain depends on the frequency of these paths in the other 39 journeys. Budget zero median gain until those rows are measured. The expected per-journey gains are hypotheses unless the local experiment gives an A/B result. The estimates overlap where a change removes work that a later change would also remove.

Raw logs, the native counter reports, response tapes, and attribution scripts are under `/data/bare-tmp/r22/pages/` on archMachine. Not verified: full-cohort median, load-test ramps after a fix, windowed runs, Windows, idle-machine latency, and full compatibility suites.
