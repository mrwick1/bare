# Reduce js2 cost on three public journeys

## Decision

1. Parse large constant literals without making general expression nodes. Keep js2's existing allocation recipe and its runtime semantics. Start with the Python Docs search index.
2. Skip decimal atom work when a shape cannot have a named index. Use the existing non-creating atom probe for shapes that can have one. Start with the profiles for W3C.
3. Move lazy host name conversion after the cache probe. Measure the result on the Python.org documentation journey. Defer general changes to lazy function compilation.

The first two changes address different parts of the JavaScript engine. Measure and review each change in its own PR.

## Inputs and measurements

The source is `862ab0c3308d13ffb550fd597ecb82415d324fc9` on `origin/main`. Its code matches the `ab09c54b` build in [PR #942](https://github.com/mrwick1/bare/pull/942). The host is archMachine with an AMD Ryzen 9 5900HX, Rust 1.96.0, perf 7.3.1, and Callgrind 3.25.1. Both modes use the same release binary.

```sh
CARGO_BUILD_JOBS=3 CARGO_TARGET_DIR=/data/bare-tmp/r22/js2-pages-round2/target \
  CARGO_PROFILE_RELEASE_DEBUG=true CARGO_PROFILE_RELEASE_STRIP=false \
  cargo build --release --locked -p bare-drive -p bare --example type_load --bin bare-drive
```

Use the URLs and assertions in the [public step files](../tools/drive/public). Set `BARE_SEED=1`, `LC_ALL=C`, and `GLIBC_TUNABLES=glibc.malloc.tcache_count=65535`. Set `BARE_JS_EXEC=auto` for OLD and `BARE_JS_EXEC=new` for js2. Run `bare-drive --quiet --var URL=<url> <steps>`. Rotate engines between trials. Record a separate live OLD run per journey with `BARE_NET_RECORD`. Use the same tape in both replay modes with `BARE_NET_REPLAY` and `BARE_CLOCK=virtual`.

The first three rows give PR #942's live counts. The other rows give this run. Each count is the minimum of three passing `perf stat -e instructions:u` samples, with the min–max range in brackets, in millions. All 36 new instruction samples passed with 100% counter coverage. Compare the multiset of reply URL, status and byte count. Replay also requires no tape misses.

| Journey and input | OLD | js2 | js2 / OLD | Traffic |
|---|---:|---:|---:|---|
| Python Docs search, #942 live | 1,106.00 [1,106.00–1,107.20] | 1,497.80 [1,497.80–1,497.90] | 1.35× | Minima match, 40 replies, 42,122 bytes |
| W3C home links, #942 live | 723.80 [723.80–730.50] | 907.60 [907.60–912.40] | 1.25× | Minima match, 7 replies, 83,582 bytes |
| Python.org docs link, #942 live | 1,059.90 [1,059.90–1,224.20] | 1,223.50 [1,223.50–1,237.30] | 1.15× | Minima differ, 18 / 16 replies |
| Python Docs search, live | 1,072.65 [1,072.65–1,075.14] | 1,454.79 [1,454.79–1,457.13] | 1.36× | 40 replies, 42,122 bytes |
| Python Docs search, replay | 1,058.57 [1,058.57–1,058.67] | 1,439.92 [1,439.92–1,441.04] | 1.36× | 40 replies, 957,162 bytes |
| W3C home links, live | 697.78 [697.78–699.64] | 869.61 [869.61–874.02] | 1.25× | 7 replies, 83,582 bytes |
| W3C home links, replay | 690.59 [690.59–690.63] | 848.27 [848.27–849.07] | — | 11 replies, 134,910 bytes. Missing background POST replies. |
| Python.org docs link, live | 1,174.74 [1,174.74–1,212.53] | 1,201.00 [1,201.00–1,214.06] | 1.02× | 16 replies, 331,870 bytes |
| Python.org docs link, replay | 1,141.38 [1,141.38–1,141.41] | 1,168.71 [1,168.71–1,168.81] | 1.02× | 20 replies, 541,271 bytes |

In PR #942, the lowest matching OLD Python.org sample used 1,199.45 M. It received 16 replies and 331,964 bytes. The corresponding js2 minimum was 1,223.52 M, a 1.02× ratio. The 1,059.94 M OLD sample had three zero-status replies for analytics, a stylesheet and jQuery. It also lacked the 272-byte `/authenticated` reply. The matched tape on current main has the Google CDN files in both modes. The 27.33 M replay gap is the target for this journey.

The live report for Python Docs excludes the background search-index reply from its final network list. The replay includes it. Both live profiles execute a large data-literal build. For W3C, the virtual clock reaches requests on the standards page that live runs usually leave pending. Replay misses a background POST in both modes. The URLs, statuses and byte counts in the native replay reports match. Use the live profile and live counter samples for W3C attribution and the acceptance gate.

Peak RSS uses the median and range of three separate `wait4` samples in MiB under the same allocator settings.

| Journey | Live OLD → js2 | Replay OLD → js2 |
|---|---|---|
| Python Docs search | 73.88 [73.86–73.98] → 75.10 [74.75–75.69] | 73.09 [72.89–73.56] → 75.56 [75.28–75.77] |
| W3C home links | 28.53 [28.31–28.60] → 29.67 [29.27–29.93] | 27.97 [27.77–28.05] → 29.69 [29.60–29.70] |
| Python.org docs link | 39.69 [39.53–47.15] → 37.68 [37.11–38.08] | 39.79 [39.47–40.69] → 38.20 [37.88–38.25] |

One W3C js2 live RSS trial has six replies. The other five trials have seven. W3C replay RSS has the same POST limitation as the instruction samples.

## Callgrind split

Use `--tool=callgrind --cache-sim=no --smc-check=all-non-file --separate-callers=1`. For these runs, raise the step-file timeout to 120 seconds. The table gives exclusive `Ir` in millions. Attribute instructions by source or symbol, then by the immediate caller. The engine boundary includes `eng*.rs`, `api*.rs`, and host-aware lookups implemented there. Other includes unresolved allocator, copy, and runtime work. The separate call-edge counts below include callees.

The Python Docs live profiles have 40 replies and 42,122 bytes. The W3C live profiles have five replies and 64,947 bytes and list two fewer script replies than the native runs. The Python.org tape profiles have 20 replies and 541,271 bytes. The live Python.org profiles differ by two replies and by style/layout work, so use its tape pair. The Python Docs tape profile pair differs by one zero-byte 304 response. Its gap is 376.28 M, compared with 368.49 M live. The W3C tape profile pair differs by one 974-byte script and has tape misses.

| Subsystem | Python Docs live OLD → js2 | W3C live OLD → js2 | Python.org tape OLD → js2 |
|---|---:|---:|---:|
| Network | 85.55 → 108.23 | 69.86 → 89.91 | 34.94 → 35.27 |
| HTML parse | 2.63 → 2.80 | 7.64 → 7.65 | 8.63 → 8.65 |
| JS parse and compile | 40.20 → 644.41 | 33.24 → 60.63 | 184.20 → 260.57 |
| Style | 41.48 → 36.12 | 33.21 → 31.77 | 296.77 → 296.37 |
| Layout | 13.26 → 13.26 | 9.95 → 9.95 | 269.21 → 269.21 |
| Paint | 0.17 → 0.17 | 0.14 → 0.14 | 0.22 → 0.22 |
| JS run | 724.18 → 399.71 | 522.12 → 558.75 | 169.31 → 103.65 |
| GC | 40.29 → 31.11 | 0.00 → 4.82 | 0.00 → 2.09 |
| Engine boundary | 0.57 → 21.15 | 0.99 → 57.58 | 0.69 → 18.21 |
| DOM and browser | 12.97 → 12.51 | 11.98 → 14.96 | 67.92 → 68.93 |
| Images | 0.17 → 0.15 | 0.35 → 0.34 | 38.79 → 38.79 |
| Driver | 1.17 → 0.36 | 0.22 → 0.23 | 0.19 → 0.19 |
| Other | 145.65 → 206.82 | 44.91 → 84.29 | 121.70 → 115.24 |
| **Total** | **1,108.30 → 1,476.79** | **734.61 → 921.00** | **1,192.57 → 1,217.39** |

| Journey | Largest js2 built-in source groups in JS run, exclusive M | OLD corresponding work |
|---|---|---|
| Python Docs | Data recipe build 28.78, string 26.62, object 8.81, array 2.63 | OLD's source-based data build and related literal helpers use 169.16 M exclusive. |
| W3C | Object 16.50, buffer views 9.58, array 9.00, BigInt helpers 6.68, string 6.36 | OLD's interpreter dispatch and shared property helpers contain much of this work. OLD's bytecode loop alone uses about 226 M on the tape. |
| Python.org | Object 4.28, regexp and runtime 3.72, array 0.49, string 0.35 | OLD regexp execution uses 2.38 M exclusive. Parser and character-class construction are in the compile row. |

The built-in source grouping includes helpers that run below an opcode. It excludes time attributed to callees in other modules. OLD handles many built-ins inside its evaluator, so this split cannot identify all corresponding functions by source name.

## Python Docs search

The search index calls `Search.setIndex` with a large object literal. js2 builds one general syntax node per value, then [lowers the tree](../crates/js2/src/data_lit.rs) to a prefix allocation recipe. The live compiler enters `Lexer::scan_at` about 1.19 M times across the journey. It enters the primary expression parser about 0.53 M times. Recipe lowering costs 190.50 M in six calls, including about 522,000 recursive visits. Portable cache capture costs 78.50 M across 13 entries. Runtime recipe builds use 74.06 M in two calls.

OLD's [parser](../crates/js/src/parse.rs) checks strict data syntax with a byte scanner and keeps a span. It spends 74.85 M in 32 scanner calls, including rejected candidates. Its [builder](../crates/js/src/data_lit.rs) creates the data directly from the span in 305.25 M. OLD avoids the per-item syntax nodes. Its later build costs more than js2's recipe build.

QuickJS emits bytecode as `js_parse_object_literal` and `js_parse_array_literal` consume tokens. It has no general AST materialization step for these literals. Its `json_parse_value` has a separate path for JSON data. The comparison uses the local QuickJS 2026-06-04 source.

| Design | Work and storage | Expected gain |
|---|---|---|
| Check a data span and lower it directly to the existing recipe. | Use a byte scanner for the JSON subset of JS literals. Keep one AST node with the span, then emit the prefix items from that span. Use two linear byte passes, O(depth) scratch, and the existing O(items) recipe. Keep the normal parser for syntax outside the subset. | Hypothesis: 350–500 M in Python Docs. It removes the token and tree work and keeps the cheaper runtime builder. |
| Build source-backed data at runtime, as OLD does. | Check and retain the span. Scan and allocate again every time execution reaches it. Use O(depth) scratch and retain the source. | Hypothesis: 100–300 M on Python Docs on a single execution. Repeat execution can lose much of the gain. This also changes the runtime builder, so defer it. |
| Emit the recipe during the parser's first pass. | Store compact literal payloads in the program so the compiler consumes them without a second scan. Preserve atom ownership, limits and rollback for rejected candidates. | Hypothesis: another 30–75 M beyond the span path. It adds transient program storage and changes the parser/compiler interface. Defer until measurements justify those changes. |

A fast path must charge the original node and list budgets or fall back. Keep depth limits and JS cover grammar. Reject `__proto__`, including escaped spellings, and shapes that can be valid destructuring patterns. Check strings with escaped code units, numbers with a fraction or exponent, negative zero, duplicate keys, source order, non-JSON syntax fallback, and fresh values on every execution. Keep existing atom binding and GC rules. Check if portable cache capture still matters after the first fix.

## W3C home links

When a chain has a host-filled or lazy object, [`hs_walk`](../crates/js2/src/api_host.rs) checks ordinary own properties while it walks. The indexed branch of `hs_plain` looks for a dense element. If it misses, it decimal-formats and interns the index before it probes the shape. The live js2 profile spends 176.56 M over 196,187 `get_prop_g` calls to `hs_walk`. Of that, 112.96 M is in 269,807 calls to `hs_plain`. Its 176,341 calls to `idx_atom` alone spend 76.70 M. A separate inherited setter path spends 49.74 M in 98,115 calls to `get_own`. That path already uses a non-creating probe. It still formats the index for each applicable shape.

OLD keeps integer property keys in the evaluator and checks dense elements before named access. QuickJS returns in-bounds integer array and typed-array reads directly in `JS_GetPropertyValue`. `JS_ValueToAtom` uses an immediate atom for a small nonnegative integer on the slow path. Neither needs a decimal string for every lookup.

| Design | Work and storage | Expected gain |
|---|---|---|
| Use existing shape metadata to skip a named index probe. | If an immutable shape has no named indices, return the own-property miss before formatting the key. For a dictionary or a shape with named indices, use the current lookup-only atom probe. Keep the host lookup order. O(1) work and O(1) scratch on a guarded miss. | Hypothesis: 45–100 M on W3C across reads and indexed writes. Python.org's corresponding paths are much smaller. |
| Format and intern the index only once per walk. | Resolve the decimal atom once when the first shape needs it. Recheck after a lazy callback that can add a previously absent property. Share a hit across a chain of length L. | Hypothesis: up to 45 M on W3C. A single-object miss still formats the index. |
| Represent small named indices as immediate keys. | Carry integer keys through shape insertion, descriptors and lookup. Format only for reflection or a host name. | Hypothesis: up to the 126 M combined read and setter scopes on W3C. It changes shape storage and other property paths. Defer it. |

Test dense hits, inherited data and accessors, a numeric name added after a miss, dictionary shapes, and the lazy callback on a numeric miss. A miss must leave the atom table unchanged. Preserve host shadowing and ordinary prototype lookup.

## Python.org documentation link

The compile row costs 76.37 M more on js2. It is offset by 65.66 M less JS execution. The tape has 802 calls to js2 `parse_function` under lazy compilation, for 45.22 M inclusive. The initial parse already checked those bodies for syntax. The later parse builds the tree js2 compiles. QuickJS compiles nested bodies during parsing and finishes their bytecode in `js_create_function`. It has no first-call body parse.

| Design | Work and storage | Expected gain |
|---|---|---|
| Retain checked bodies for functions that run early. | Reuse bounded parsed bodies and capture information for selected functions. Keep most bodies lazy. Store O(nodes) additional data for retained bodies. | Hypothesis: 0–45 M on the tape, before paying to retain bodies that never run. Check retention and selection costs. |
| Emit code during the first function-body parse. | Compile each body and hold its bytecode, so a call does no body parse. Use O(code) retained storage and parse each body once. | Removes up to 45 M of separate body parse on the tape. First load compiles unused bodies. Total gain and peak RSS remain unverified. |

Both compiler designs change how the parser hands bodies and captures to the compiler. They need more than a small change. Defer them in this round.

A smaller cost is separable. `hs_lazy` renders the name before it checks whether it already asked the host. The Python.org tape spends 5.29 M in 14,987 calls from `hs_lazy` to `Realm::text`. The W3C live walk spends 36.67 M in 41,351 calls to `hs_lazy`, including hashing and real requests. A repeated miss can return using the existing numeric or atom identity. OLD constructs its lazy property request from its property key. QuickJS has no matching DOM lazy table. Its exotic lookup takes a `JSAtom` and keeps that representation until an operation needs text.

| Design | Work and storage | Expected gain |
|---|---|---|
| Construct the host name only on the first ask. | Probe the existing tuple set using the object address, key kind and key identity. Create text for a callback only after insertion succeeds. A repeat probe uses expected O(1) work and no new string. | Hypothesis: 3–6 M on Python.org and 8–20 M on W3C. Recheck the gap after the change. |
| Let the host identify the names it can supply. | Register a predicate or name set for each lazy owner and skip asks for names it cannot supply. Use O(names) metadata and an O(1) or bounded prefix check. | Hypothesis: 8–15 M on Python.org and 15–35 M on W3C. It changes the JS host API and browser registration. Defer it. |

Check callbacks for names, indices, symbols and enumeration. Keep the first callback, retries after GC, and callback errors. The design does not predict that the small host change alone removes the full Python.org gap.

## Remaining Python Docs work after the first three fixes

The tape still uses 1,106.33 M instructions on js2 after the checked-span, indexed-probe, and host-name changes. The OLD minimum was about 1,058.5 M on the same tape. All measured native replies match. The newer Callgrind sample has one extra zero-byte 304. It still attributes 79.05 M inclusive to portable code capture.

### Keep the warm search-index cache

The decoded search-index script is 3,806,411 bytes. In the isolated `code_cache_load` example, its first load uses 433.95 M Callgrind instructions. A single `PortableUnit::capture` call uses 76.60 M. The entry accounts for 17,722,435 bytes. Two and three realm loads use 474.95 M and 516.98 M. The cache reports one and two hits. Each warm load uses about 41–42 M additional instructions.

OLD's source-span literal has no portable recipe to copy. QuickJS's `__JS_EvalInternal` creates bytecode and evaluates it. Its portable writer is the separate `JS_WriteObject` API.

| Design | Work and storage | Expected gain |
|---|---|---|
| Admit a large recipe on its second load. | Keep one bounded identity marker after a cold miss. Build the portable entry only after a repeat. The first load avoids O(items) cloning. The next load compiles a second time. | Saves up to 76.6 M cold. The second request would use about 393 M extra instructions from the measured compile and bind costs. Keep the current admission policy. |
| Share immutable recipes that contain local atom indices. | Let the realm and cache share the O(items) recipe. Bind a separate O(distinct atoms) table per realm. The runtime allocation op must read strings through that table. | Hypothesis: saves up to 76.6 M on cold loads and retains the warm compile hit. This changes compiler storage and the runtime builder. Defer it. |

### Emit small integers during the checked-span lowering pass

The recorded search-index object contains 341,428 numbers. They are all integers with at most nine decimal digits. The isolated compile enters the general floating-point parser 341,428 times. `FromStr<f64>` and its decimal parser use about 46 M exclusive instructions in the journey profile. The decimal text had already been checked by the span scanner.

OLD's `Build::number` accumulates digits directly when up to 15 digits fit exactly in an `f64`. It applies the sign separately and keeps negative zero. QuickJS checks the syntax in `json_parse_number`, then calls `js_atod` and returns a compact number through `JS_NewFloat64`. js2 can emit its tagged integer directly when the scanner has already checked the digits.

| Design | Work and storage | Expected gain |
|---|---|---|
| Accumulate the integer on the lowering scan. | Use the digit walk already needed to find the token end. For at most nine digits and no fraction or exponent, emit a tagged integer. Make negative zero and other decimal numbers use the existing converter. O(digits) time and O(1) storage. | Hypothesis: 40–50 M on Python Docs. The first validation pass still checks syntax without building a value. |
| Try `i32` conversion after scanning each number. | Accept the same signed integer subset. Delegate every other token to the existing decimal converter. It reads digits twice during lowering. O(digits) time and O(1) storage. | Hypothesis: 25–40 M. The extra digit pass gives simpler code but preserves work that the first design removes. |

Build the first option. Cover negative zero, the fast-path endpoints, fractions, exponents, long integers and long exponent cancellation. Recheck the matching tapes, live samples, peak RSS, the relevant test262 directories, and the parser and interpreter cost rows.

## Verification and limits

For each fix, use a new focused test file. Keep a failed baseline result and a passing result. Run the narrow tests, targeted test262, affected cost rows, five js2 interpreter rows, and the three-run journey and RSS gates. Recheck Rust Book search, DEV home, Python.org search and the React typing fixture. Reuse the tapes and check the live reply lists again.

The raw measurements and attribution scripts are under `/data/bare-tmp/r22/js2-pages-round2/` on archMachine. Full compatibility suites, Windows, windowed paint, idle latency and post-fix costs remain unverified.
