# 2026-11-21 — Cache js2 globals in dictionary mode

## Design

The realm allocates its global as an ordinary object. The browser marks it for lazy member lookup and installs a host-filled object in its prototype chain for window names. An existing own data property wins over both. A global lexical binding wins over the object property.

A global bytecode site's entry stores the address of a value slot and the global object's mutation tag. Its hit checks one tag and reads or writes one slot in O(1). The shape guard used by ordinary property sites serves a different purpose. Those sites can see many objects, and a dictionary shape ID cannot guard attributes or slot positions because its table changes in place.

| Option | Storage and lookup | Trade-off |
| --- | --- | --- |
| Keep the global in tree shapes | Use the existing shape transition table and slot record. Raise the shape limit for the global and add transitions for changes now handled by dictionaries. A hit is O(1). | Each change can allocate shape metadata. General deletes or reconfiguration can require copying O(n) entries. Lazy members and script additions share the transition table. |
| Give globals property cells | Give the global dictionary one stable cell per property. A site checks the cell state and then loads or stores its value in O(1). Deletion or a change to an accessor invalidates the cell. | Adds O(n) per-property storage and new cell lifetime and invalidation rules. A separate script-global record would also need to share its values and descriptors with the window's property operations. [V8 property cells](https://raw.githubusercontent.com/v8/v8/main/src/objects/property-cell.h) · [V8 cell replacement](https://raw.githubusercontent.com/v8/v8/main/src/objects/objects.cc) |
| Reuse the global mutation tag | Keep the dictionary entries and value slots. Permit a global site to cache an own data slot after lookup, with an extra writable check for stores. A hit is O(1) with the existing 16-byte site record. | No additional storage. Any relevant global mutation invalidates all global sites. A site looks up the atom and dictionary entry again on a miss. |

Use the existing tag. Obj::set_shape bumps it even when a dictionary mutation keeps the shape ID. Slot-record moves also bump it, and lexical declarations invalidate it when they can shadow an object property. Deletion can free a slot and a descriptor change can change its meaning, so both must invalidate the entry. Accessors, inherited names and virtual names still follow the existing lookup. A lazy member defined as an own data property can fill an entry on later reads. SpiderMonkey also checks global lexical bindings before object access and guards where shadowing can occur. [SpiderMonkey CacheIR](https://raw.githubusercontent.com/mozilla/gecko-dev/master/js/src/jit/CacheIR.cpp)

## Global access counters

The test uses the host installed by `filled()` and marks the global for lazy lookup. In the test, `Object.defineProperty` changes an attribute of an existing own property and puts the global in dictionary mode. Six sites run repeatedly in the 1,000-iteration sum. The unpatched test found no entries. With dictionary slots enabled, all six sites held the current tag.

The counters count hits in the interpreter, entries installed by `fill_global`, and calls to `hs_walk` with the global as receiver during the script. Both probes used the same instrumented binaries with a switch for the dictionary-slot path. The instruction measurements use clean builds.

| Script | Mode in both runs | Cache fills | Read hits | Write hits | Global host walks | Walks per iteration |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| Host-realm loop, 1,000 iterations | Dictionary, lazy | 0 → 6 | 0 → 3,993 | 0 → 1,996 | 4,001 → 0 | 4.001 → 0 |
| Browser `loop_100000` | Dictionary, lazy | 0 → 6 | 0 → 399,993 | 0 → 199,996 | 400,003 → 0 | 4.00003 → 0 |

The browser trace puts the first dictionary transition in `Dom::install`. js2 installs `self` as non-enumerable. The browser defines it again with `Attrs::ALL`, so `Obj::reconfigure` converts the shape before the page script runs. The global has the lazy-member flag in both probes. The host-filled named object sits on the prototype chain. The old global fast path rejects the dictionary at every access. Reads therefore call `hs_walk` even when the global has the property. Writes go through `set_prop_g` and `set_named`.

## Measurements

The clean before and after builds use the same frozen source, with the two global-cache eligibility checks changed and regression tests added in the after build. The before source is `5f7099ab3041462a78a6cf4f732a3191e8f6c05b` (#854 plus the then-current main). Its 10M global loop reproduces the 31.10 G instruction minimum in the original report. [Original journey comparison](2026-11-21-js2-vs-old-journeys.md)

Machine `archMachine`, AMD Ryzen 9 5900HX, Linux `7.2.15-zen1-1-zen`, Rust 1.96.0, perf 7.3.1. The profiling builds use `-C force-frame-pointers=yes`. Browser runs use `BARE_JS_EXEC=new` and `BARE_SEED=1`. A loopback server serves the original fixtures. The driver checks `window.__done === 1` on each ramp.

Run each case in three fresh processes. Count user-mode instructions with `perf stat -e instructions:u`. Browser instruction runs use `glibc.malloc.tcache_count=65535`. Three additional runs with the default allocator measure ready time through the completion assertion and peak RSS with `wait4`. Compare instruction minima. Tables show the minimum and maximum of three, except peak RSS, which shows medians. The machine was shared during the run.

The interpreter inputs are `loopfn 3000000`, `calls 1000000`, `fib 27`, `closures 1000000`, and `objects 1000000` from `interp_load`. Startup runs `script_run` on `var x = 1;` and reads `x`. Both allocator settings run for each of these six rows. The raw logs are under `/data/bare-tmp/r21/glob-evidence` on `archMachine`.

| Browser workload | Instructions, M (before → after) | Change | Ready, ms (before → after) | Peak RSS, MiB (before → after) |
| --- | ---: | ---: | ---: | ---: |
| `ctor_100k` | 1663.66–1665.24 → 1422.35–1423.08 | -14.51% | 393.3–1339.6 → 349.8–373.1 | 18.0 → 21.0 |
| `ctor_10k` | 170.88–172.42 → 147.23–148.33 | -13.84% | 56.4–133.5 → 54.3–112.4 | 21.0 → 20.7 |
| `ctor_1k` | 21.26–21.42 → 18.88–19.02 | -11.20% | 13.0–27.2 → 17.5–241.9 | 21.2 → 20.9 |
| `ctor_1m` | 16456.63–16637.74 → 14151.52–14191.82 | -14.01% | 4879.1–8696.4 → 2733.2–4329.5 | 20.6 → 20.9 |
| `ctor_read_100k` | 2867.04–2880.27 → 2491.11–2491.56 | -13.11% | 776.3–2886.8 → 771.0–1447.2 | 68.2 → 67.9 |
| `ctor_read_10k` | 291.48–291.93 → 253.00–253.05 | -13.20% | 198.4–258.0 → 109.9–237.7 | 26.8 → 26.3 |
| `ctor_read_1k` | 33.68–33.78 → 29.89–29.89 | -11.27% | 35.5–83.5 → 19.2–33.7 | 21.8 → 21.3 |
| `loop_100000` | 317.51–317.52 → 38.71–38.73 | -87.81% | 68.3–394.9 → 20.3–25.7 | 20.1 → 20.8 |
| `loop_1000000` | 3143.52–3143.53 → 355.51–355.52 | -88.69% | 505.2–1646.4 → 104.4–192.5 | 20.4 → 20.7 |
| `loop_10000000` | 31103.54–31403.54 → 3523.52–3523.52 | -88.67% | 7386.0–10884.8 → 635.3–767.5 | 20.0 → 19.9 |
| `loop_local_10000000` | 1523.56–1523.56 → 1523.56–1523.57 | -0.00% | 368.4–751.5 → 312.7–765.1 | 16.5 → 19.8 |

| js2 workload | Instructions, M, large tcache (before → after) | Change | Instructions, M, default (before → after) | Change |
| --- | ---: | ---: | ---: | ---: |
| `loopfn` | 908.321–908.322 → 908.322–908.323 | +0.00% | 908.321–908.322 → 908.322–908.323 | +0.00% |
| `calls` | 604.382–604.383 → 604.383–604.383 | +0.00% | 604.383–604.384 → 604.384–604.385 | +0.00% |
| `fib` | 269.114–269.114 → 269.114–269.115 | +0.00% | 269.115–269.115 → 269.116–269.117 | +0.00% |
| `closures` | 1980.495–1980.497 → 1980.496–1980.497 | +0.00% | 1980.534–1980.536 → 1980.536–1980.537 | +0.00% |
| `objects` | 721.166–721.167 → 721.168–721.168 | +0.00% | 721.170–721.170 → 721.170–721.171 | +0.00% |
| `startup` | 0.739–0.740 → 0.740–0.740 | +0.01% | 0.737–0.738 → 0.736–0.738 | -0.14% |

The 10M global loop drops from 31.10 G to 3.52 G instructions. That is 0.39× the 8.98 G minimum for the old engine in the original report. The local-variable control keeps the same instruction count. The constructor ramps fall by 11.20% to 14.51%. No interpreter or startup row exceeds the 0.5% cost rule.

## Verification

The host cache regression failed on the baseline. It found zero entries for the six repeated sites. With the change it passed. The host and browser regressions exercise named properties, lazy resolution, strict writes, reconfiguration to an accessor, and deletion.

The final checks include the object kind change from main and the RegExp build fix. The before-and-after instruction counts above isolate the global cache change on the earlier frozen source. [Object kind change](https://github.com/mrwick1/bare/pull/853) · [RegExp build fix](https://github.com/mrwick1/bare/pull/862)

- `cargo test --locked -p bare-js2 --lib api_host`: 18 passed.
- `cargo test --locked -p bare-js2 --lib interp::tests`: 38 passed.
- Global cache and frozen-global tests: 5 passed.
- `cargo test --locked -p bare --lib browser_new`: 4 passed.
- `cargo check --locked -p bare-js2 -p bare`: passed.
- `tools/journeys/triage.sh -e new`: 46 passed, 3 failed, 1 skipped. The failures match the original report: `curl-se-home`, `duckduckgo-html-search`, and `lobsters-comments-back`. Staging was skipped. The driver used source `6ffa3783905ea4497047f6c98f268fcaa6e46f1e`.

Windows, Miri, test262, and the full unit suites were not run locally.
