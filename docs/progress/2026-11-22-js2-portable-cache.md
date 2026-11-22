# Share js2 compiled units across realms

## Design

The loader parses a source and builds its code and lazy stubs. Each code has a table of atom pointers. Bytecode already indexes this table. Child function references point into the realm’s full code table. Large constant trees also store atom pointers. Lazy stubs keep parse facts and closure metadata. The realm owns the mutable execution state.

A portable unit holds codes with unit-relative child indexes and a table of owned string units. Each code’s atom table and constant trees hold indexes into that string table. On a hit, intern each distinct string into the realm’s source atom table and bind the code tables. Add the realm’s code base to each child index. Keep the source once and leave mutable execution state in the realm. The cost of a hit is linear in source bytes for the hash and comparison, plus the size of the compiled tables for the bind. The bind needs one transient vector of realm atoms. Each realm keeps its bound tables, and the cache keeps the portable tables.

The thread keeps up to 32 MiB of accounted entries and skips sources shorter than 512 bytes. A hash map uses source identity and compile mode as the key. A hit also compares the source bytes. An ordered index tracks recency and removes the oldest entry first for size or realm age. Lookup takes expected constant time before the source comparison, and an update takes logarithmic time in the number of entries. This change stores no files.

- An operand remap at runtime would keep more code shared, but a property or string operand can run many times for each bind. Binding fills the existing operand tables, so the hot loop can stay as it is.
- One atom arena per thread would avoid reinterning, but js2 never sweeps shared atoms. Live and dead documents would retain all of their interned names until the thread exits. It would still need per-realm child indexes and a portable disk format.
- A parse-tree cache would avoid repeated reads of the source and retain the tree. Every realm would still compile. The current lazy parser already drops most function bodies after it checks them.

V8 caches compiled code by source within an isolate and separates context-specific feedback. SpiderMonkey emits a GC-independent Stencil and instantiates it into runtime cells. [V8 code caching](https://v8.dev/blog/improved-code-caching), [SpiderMonkey frontend](https://firefox-source-docs.mozilla.org/js/)

## Implementation

The compile entry calls `Identity::of` once for an eligible source. A cache hit binds the owned string units into that realm and copies the code tables. The loader changes child references to realm indexes when it appends the codes. Binding fills the operand tables before execution. Each realm owns its feedback and GC-managed state.

The seam for the disk cache is `code_cache::compile_on_miss`. The disk reader can construct a validated `PortableUnit` there. `PortableUnit::bind` serves both memory hits and decoded units. The current miss returns the code it compiled in the target realm and stores its portable copy.

The browser calls the cached script and module loaders through its existing js2 API. The standalone lazy loader also uses the cache. The browser’s download path still uses the old engine’s parse-ahead worker. Js2 has no production implementation of the parse-ahead job trait. Compiled lazy bodies stay in the realm that called them. The cache shares their stubs, source, and initial parse facts.

## Measurement

Machine archMachine, AMD Ryzen 9 5900HX, Linux 7.2.15-zen1-1-zen, Rust 1.96.0. Release base `65f23e2f`, change `08edd23a` before the report. Each row has three fresh processes per variant. `perf stat -e instructions:u` reported at least 99% coverage. Instruction ranges are min–max in millions. Peak RSS is median [min–max] in KiB from three separate `wait4` runs. The machine ran other work during the measurements.

The bundle is the 314,192-byte React typing fixture `app.js` (SHA-256 `1992c72031ad2bad490a784172a2e9400b822cb0eb4d0e539f4a021af7c07400`). Run `code_cache_load FILE REALMS [cold]` from the release examples. It compiles and drops each realm in order. `cold` clears the cache before each realm and pays the cost of storing the new entry. The base has no cache. The first realm in a cached process fills it. Its entry charges 444,084 bytes.

| React realms | Base instructions | Forced-cold instructions | Cached instructions | Base RSS | Forced-cold RSS | Cached RSS |
|---|---:|---:|---:|---:|---:|---:|
| 1 | 76.796–76.801 | 76.994–76.996 | 76.992–77.000 | 6172 [6136–6216] | 6104 [6060–6400] | 6328 [6204–6336] |
| 2 | 152.993–152.998 | 153.382–153.392 | 77.482–77.489 | 6136 [6068–6180] | 6112 [6100–6208] | 6060 [6060–6120] |
| 5 | 381.595–381.619 | 382.575–382.596 | 78.958–78.959 | 6084 [6040–6188] | 6224 [6012–6276] | 6240 [6216–6344] |

| Additional React realms | Base per realm | Forced cold per realm | Warm per realm |
|---|---:|---:|---:|
| 1 | 76.192–76.201 | 76.388–76.396 | 0.483–0.493 |
| 4 | 76.200–76.206 | 76.395–76.400 | 0.489–0.491 |

The per-realm estimate subtracts the matching one-realm process count before dividing by the number of later realms.

The design probes used the same bundle and three-run method. The parse-tree probe retains the lazy parse result and compiles fresh code in each realm. The thread-arena probe holds one source atom arena and copies its code tables. Both probes omit cache map bookkeeping. The thread-arena probe covers one source. The memory cost of retaining atoms across unique documents was outside the probe.

| Probe | Five-realm instructions | Additional instructions per realm | Five-realm peak RSS |
|---|---:|---:|---:|
| Parse tree | 79.276–79.281 | 0.563–0.565 | 6052 [6044–6108] |
| Thread atom arena | 78.955–78.964 | 0.480–0.483 | 6128 [6056–6244] |

A separate operand probe ran 10 million dependent lookups against a 1,024-slot table. The direct path used 57.829 M instructions (57,828,819–57,829,180). The second index load raised the count to 80.329 M (80,328,854–80,329,459). Both got the same checksum. This measures the table access pattern. The bind fills the operand tables before JavaScript runs.

### Browser journey

The headless `bare-drive` journey on `BARE_JS_EXEC=new` opened the React typing fixture on loopback and visited four more documents in one session. Each loaded the same `app.js` and had to show `#a1`.

| Build | Passed | Instructions, M | Journey time, ms | Peak RSS, KiB |
|---|---:|---:|---:|---:|
| Base | 3/3 | 2702.522–2704.436 | 1061.7–1331.2 | 37424–37700 |
| Cache | 3/3 | 2398.115–2401.806 | 1177.1–1340.2 | 37656–38616 |

Journey time includes waiting, navigation, and the visibility checks. It is observational on this shared machine.

### Compiled micro sources

Use `interp_load --source` for the same five cases. Prefix a comment of 512 spaces to exercise the cache. These rows measure compilation and setup. They use five realms and estimate the cost of a later realm by subtracting a one-realm run.

| Source (turns) | Base per realm, M | Forced cold per realm, M | Warm per realm, M |
|---|---:|---:|---:|
| loopfn (1000) | 0.307–0.307 | 0.315–0.315 | 0.261–0.262 |
| calls (1000) | 0.325–0.326 | 0.336–0.336 | 0.264–0.265 |
| fib (12) | 0.304–0.304 | 0.312–0.313 | 0.261–0.261 |
| closures (1000) | 0.345–0.346 | 0.356–0.356 | 0.263–0.264 |
| objects (1000) | 0.316–0.316 | 0.324–0.324 | 0.261–0.261 |

### Interpreter cost rows

The hand-assembled examples measure execution. Every run checked its result. Cells show minimum instructions in millions (base → change), followed by percent change. The maximum within-row spread is in the last column. The release profile sets `codegen-units=1` for js2.

| Row | glibc | jemalloc | Max spread, M |
|---|---:|---:|---:|
| loopfn 3000000 | 866.297 → 866.299 (+0.0003%) | 869.704 → 869.706 (+0.0002%) | 0.0010 |
| calls 1000000 | 571.348 → 571.350 (+0.0004%) | 574.723 → 574.725 (+0.0005%) | 0.0305 |
| fib 27 | 248.668 → 248.670 (+0.0008%) | 252.106 → 252.108 (+0.0011%) | 0.0006 |
| closures 1000000 | 1942.669 → 1942.674 (+0.0003%) | 1945.966 → 1945.970 (+0.0002%) | 0.0662 |
| objects 1000000 | 744.193 → 744.195 (+0.0002%) | 747.629 → 747.631 (+0.0003%) | 0.0008 |

## Cost debt

No interpreter row exceeded +0.5%. The mean change across both allocators was +0.0004%.

## Tests

- Linux and Windows VM each passed 164 focused tests. The filters ran 5 cache tests, 124 script tests, and 35 module tests.
- Linux `cargo check --locked -p bare-js2 -p bare` passed.
- The Windows VM ran the static CRT build from `cargo xwin test --locked --no-run --target x86_64-pc-windows-msvc -p bare-js2 --lib`.
- The matched test262 check used `e4fbf577`. The classic selection covers the top files of `language/expressions/super`, `language/statements/class`, and `language/statements/function`. The module selection covers the top files of `language/module-code`.

| test262 selection | Base pass / fail / skip | Change pass / fail / skip | Passes lost |
|---|---:|---:|---:|
| classic | 514 / 81 / 10 | 514 / 81 / 10 | 0 |
| module | 284 / 0 / 0 | 284 / 0 / 0 | 0 |

## Not tested

Full test262, Miri, the full cost matrix, and long-running browser sessions with many unique bundles. The disk reader and cross-realm reuse of compiled lazy bodies are follow-up work.
