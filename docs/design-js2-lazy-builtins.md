# Design: startup must not grow with the number of built-ins (js2, after #772 and #791)

Status: design, for owner review. No engine code changed. Date: 2026-11-20. Base: `origin/main` at `43b62b05`. Measured tree: the head of #791 (`49af522d`, branch `t3/improve-js2-builtins-test262`).

Words used in this file:

- "hypothesis" marks a number or a cause that no run proved.
- "startup" is the instruction count of the whole process for `script_run x.js` with `var x = 1;`. It is the measure of #772.
- "native" is a built-in function written in Rust (`NativeFn`). "Prelude function" is a built-in written in JavaScript (`prelude.js`).
- "M" and "K" are millions and thousands of user-mode instructions (`instructions:u`).

## 1. Summary

#791 adds JSON and more String, Array, Number and Math methods. Startup goes from 1,787,715 to 1,958,924 (+9.6%). The cost rule allows +0.5%. Every built-in that we add will grow startup in the same way, because js2 builds every built-in at the start of every realm.

What the measure shows (section 3):

- 1.96 M is split as follows: process start 0.47 M (23%), realm 0.32 M (15%), the prelude 1.12 M (55%), the rest 0.14 M (7%).
- The prelude is the largest part by far. Its wrapper function is parsed, compiled and run at every start, and a typical script calls few of its functions. An experiment that removes parts of `prelude.js` shows: helpers 0.60 M, `Array.prototype` 0.39 M, `Array.of`, `Array.from` and `String` 0.12 M.
- The realm costs about 1.5 K for each native (150 natives). Eager install gives 0.32 M now. At the size of QuickJS (about 890 properties) it would be about 1.3 M.
- #791 adds 182 K in the callgrind split: the prelude wrapper +115 K (63%), the realm +32 K (18%), and relocations of one static table in `bare-unicode` +33 K (18%). The last one is not a built-in cost at all.

Recommendation (section 7): three steps, each its own PR, in this order.

0. Remove the relocations of `DECOMPOSE` in `bare-unicode`: about -33 K, no engine change.
1. Lazy built-in tables (design B): the methods, statics and constants of a built-in object are a static table, and a property is made at the first lookup that needs it. Hypothesis: the realm goes from 315 K to about 90 K.
2. A flat prelude (design D2): no wrapper function. Each prelude function is a lazy table entry that makes a stub at first lookup and does not run any script. Hypothesis: -1.1 M for a script that uses no built-in, about -1.0 M for a typical page.

Together (hypothesis): startup about 0.6 M for a script that uses no built-in, and about 0.7 M for a typical page, against 1.96 M now and 1.18 M for QuickJS. Startup then no longer grows with the number of built-ins. It grows with the number that a script uses, at about 1.7 K each.

A startup snapshot (design C) is not recommended: its cost grows with the size of the realm, it needs a large amount of `unsafe` code and a two-stage build, and it gains no more than B and D2 together.

## 2. How this was measured

- Machine: archMachine (AMD Ryzen 9 5900HX), Linux 7.2.15. Build: `cargo build --locked --profile profiling` (release flags with symbols), `CARGO_TARGET_DIR` of its own. Example: `script_run` on a file with `var x = 1;`.
- Totals: `perf stat -e instructions:u` with `GLIBC_TUNABLES=glibc.malloc.tcache_count=65535` (as `tools/js/instr.sh`), minimum of 3 runs. #791 head: 1,960,063 to 1,960,800 (this build) and 1,958,924 (release, the PR). Base: 1,787,276 to 1,789,153.
- Split: `valgrind --tool=callgrind`, 3 runs on each tree: #791 head 2,039,981, 2,039,617 and 2,039,486; base 1,858,367, 1,858,883 and 1,858,387. The spread is 0.03%. The numbers in section 3 come from the first run. Callgrind counts about 4% more than `perf` (cause not checked), so shares are used and the totals come from `perf`.
- Install by built-in: the same build with `#[inline(never)]` on each `install_*` function (a temporary change, reverted), callgrind, 3 runs (2,039,600, 2,039,512, 2,041,351).
- Prelude experiment: three variants of `prelude.js` with the facts made again (`BLESS_PRELUDE_FACTS=1`), `perf stat`, minimum of 3. The variants are not in the PR. Tests fail on them; only the startup is read.
- Allocation: `valgrind --tool=dhat`, 1 run each.
- Rows: the nine programs of `crates/js2/examples/data/` with `script_run --eval FILE 0`, minimum of 3, on the base and on the #791 head. These are the js2 `eval_load` rows at the scale that the files have, and `bundle_like`.
- QuickJS: `qjs` of `/data/bare-tmp/quickjs-2026-06-04` on an empty file, callgrind and `perf stat`.
- Engines: the sources were read from the public repositories (V8 `src/init/bootstrapper.cc`, SpiderMonkey `js/src/vm/GlobalObject.cpp` and `js/src/jsapi.cpp`, JavaScriptCore `runtime/LazyClassStructure.h`, `LazyProperty.h`, `Lookup.h`, `JSObjectInlines.h`, QuickJS `quickjs.c`, Boa `core/engine/src/builtins/mod.rs`) on 2026-11-20, `main` or `master` of that day.
- The machine had other jobs running. Only instruction counts are used.

## 3. Measured split of startup

### 3.1 By part

The #791 head, callgrind, inclusive counts. Total 2,039,981.

| Part                                                          | Instructions | Share | Notes                                                                                                   |
| ------------------------------------------------------------- | -----------: | ----: | ------------------------------------------------------------------------------------------------------- |
| Dynamic loader (`ld.so`)                                      |        362 K | 17.8% | 3,897 relative relocations at about 16 each = 62 K; the rest is the load of libc and the binary         |
| Rust runtime before `main`                                    |        103 K |  5.0% | 89 K is one `sscanf` under `lang_start_internal` (hypothesis: the stack-guard probe of the main thread) |
| `main` of the example                                         |         56 K |  2.7% | read the file, drop the `Vm`, print                                                                     |
| `Realm::new`                                                  |        315 K | 15.5% | section 3.2                                                                                             |
| `Vm::new` without the realm                                   |          9 K |  0.4% | decode, `Asm::finish`, root maps                                                                        |
| `compile_after` (the script and the top level of the prelude) |         62 K |  3.1% |                                                                                                         |
| Prelude wrapper: `Lazy::compile`                              |        989 K | 48.5% | `compile_function` 566 K, `parse_function` 275 K, `Loader::fill` 97 K, other 51 K                       |
| Prelude wrapper: run of its body                              |        135 K |  6.6% | 11 calls of `__def` and `__helper` 81 K; ops and closures 54 K                                          |

The loader, the runtime and `main` (521 K, 26%) are the same for any Rust program that holds this binary. No design here changes them, except the relocations.

### 3.2 The realm

150 natives are made. A native costs about 1.5 K: `intern` of its name 420, `native_cell` 118, `define_attrs` 780 (a new shape for each property, `add_miss` about 380 of it), the `natives` entry about 100.

| Part of `Realm::new`                                                                                                      | Instructions |
| ------------------------------------------------------------------------------------------------------------------------- | -----------: |
| Before the install (heap, shapes, 22 atoms, the 8 prototypes)                                                             |         28 K |
| `install_object` (Object and `Object.prototype`, about 33 natives)                                                              |       58.7 K |
| `install_function`                                                                                                        |        7.9 K |
| `install_error` (7 constructors, 7 prototypes)                                                                            |       36.6 K |
| `install_array`                                                                                                           |       29.9 K |
| `install_string` (14.0 K of it in `install_string_more`)                                                                  |       55.6 K |
| `install_number` (Number, Boolean, Math with 8 constants and about 26 methods, `parseInt` and the other global functions) |       87.9 K |
| `install_json`                                                                                                            |        4.1 K |
| Global names                                                                                                              |        2.6 K |

### 3.3 The prelude, by experiment

`perf stat`, minimum of 3. The variants cut `prelude.js` and keep its wrapper.

| Variant                                                 |   Startup |                                                                   Difference |
| ------------------------------------------------------- | --------: | ---------------------------------------------------------------------------: |
| #791 head                                               | 1,958,889 |                                                                              |
| Without the `def(Array.prototype, ...)` block (6.4 KB)  | 1,571,651 |                                                                       -387 K |
| Helpers only (no `def` at all)                          | 1,454,913 | -117 K more (`Array.of`, `Array.from`, `String.replace`, `replaceAll`: 1 KB) |
| Empty wrapper (`(function (def, helper, set) {})(...)`) |   852,781 |        -602 K more (the helpers: 9 KB, 18 declarations, 10 `helper()` calls) |

So the prelude costs 1,106 K above an empty wrapper. The cost is not by byte: the 1 KB of the second row costs 117 K, because each `def` call and each function expression has a fixed cost (a closure, a stub, a root map). The empty variant, 853 K, is the floor of today's design: the process (0.47 M), the realm (0.32 M) and the rest.

The helpers (`toDesc`, the `helper(n, ...)` registrations, `copyData`, `search`, `pairs`) are slow paths that natives hand work to when they meet an accessor. A script that does not meet an accessor on a built-in never calls them. Today they cost 0.60 M at every start.

### 3.4 What #791 added

Callgrind, base 1,858,367 and #791 head 2,039,981 (+181.6 K; `perf` says +171 K).

| Part                                                                                                                                  |    Base |    #791 |   Change | Cause                                            |
| ------------------------------------------------------------------------------------------------------------------------------------- | ------: | ------: | -------: | ------------------------------------------------ |
| Prelude wrapper (`Lazy::compile` +92.1 K: `parse_function` +35.8 K, `compile_function` +45.8 K, `fill` +4.3 K; `call_native` +20.9 K) | 1,008 K | 1,123 K | +115.4 K | 2.6 KB more prelude, 22 more entries in `def`    |
| `Realm::new`                                                                                                                          |   283 K |   315 K |  +32.3 K | 22 more natives (128 to 150) at 1.5 K            |
| `ld.so`                                                                                                                               |   329 K |   362 K |  +33.3 K | 2,110 more relative relocations (1,787 to 3,897) |

The 2,110 relocations are not from built-ins. `.data.rel.ro` grows by 50 KB, and the new dependency `bare-unicode` has `DECOMPOSE: &[(u32, &str)]` with about 2,080 entries; each `&str` is one relocation. 2,084 entries of 24 bytes is 50 KB. This matches (to verify: a count with `readelf` on a build without the dependency). `bare-unicode` is already linked in `bare-js`, `bare-url` and `bare-layout`, so the browser binary pays these relocations already. The 33 K shows only in the js2 example binary.

### 3.5 The nine rows

Instructions of the whole process, `script_run --eval`, minimum of 3.

| Row           |        Base |        #791 | Change | Startup share of the row (#791) |
| ------------- | ----------: | ----------: | -----: | ------------------------------: |
| `fib`         |   6,140,492 |   6,308,558 | +2.74% |                             31% |
| `webpack`     |   7,652,821 |   7,813,316 | +2.10% |                             25% |
| `concat`      |  17,131,639 |  17,299,366 | +0.98% |                             11% |
| `closures`    |  19,747,839 |  19,916,285 | +0.85% |                             10% |
| `bundle_like` | 129,054,389 | 129,897,408 | +0.65% |                            1.5% |
| `throws`      |  37,115,647 |  37,324,002 | +0.56% |                              5% |
| `loopfn`      |  62,862,910 |  63,032,285 | +0.27% |                              3% |
| `objects`     |  89,972,753 |  90,134,954 | +0.18% |                              2% |
| `calls`       | 112,682,865 | 112,852,422 | +0.15% |                            1.7% |
| `loop`        | 148,438,793 | 148,606,781 | +0.11% |                            1.3% |

`bundle_like` grows by 843 K, five times the startup change. It makes 36 lazy compiles (callgrind: `Lazy::compile` 3.97 M to 4.25 M, +274 K), because the prelude functions that it calls got longer (the species check in `map` and `filter`). Each prelude function that a script calls costs a parse and a compile at its first call (58 K on average, `design-js2-prelude.md` section 3). A native costs none. This is a second reason, apart from startup, to move prelude functions to Rust when #785 allows it.

### 3.6 How many built-ins a script uses

The feature census of 2026-11-18 (`docs/census/2026-11-18.tsv`, old engine, 50 workloads: the staging journey, the typing page and 48 public journeys) lists 252 distinct natives that are called. A workload calls 44 of them on average (the sum of the workload counts, 2,198, divided by 50). 15 natives are used by 25 or more workloads. 89 are used by 10 or more. 55 are used by one only. `Array.prototype.forEach`, `map` and `filter` are used by 25 workloads each (they are prelude functions in js2).

The census counts the old engine's natives, so it includes RegExp, Set and others that js2 does not have yet. It is the best data that exists. A page calls about 44 natives of the 600 or more that js2 will have.

### 3.7 What an engine with no laziness pays

QuickJS (`qjs` 2026-06-04, empty file): 1,178,889 instructions. `JS_NewContext` is 550 K: `JS_SetPropertyFunctionList` 389 K, atoms 271 K (inclusive, overlapping), 888 `add_property` calls. So about 620 instructions for each property, against 1.5 K for js2. QuickJS builds all intrinsics in every context (`JS_NewContext` calls each `JS_AddIntrinsic*`). Its context is 550 K for about 890 properties. At the same count, js2 would pay about 1.3 M for the realm. This is the number to avoid.

Section 6.10 item 18 and section 6.11 of `design-engine-rewrite.md` judged static realm recipes to be low value because a realm of the old engine costs 70 K to 166 K. The js2 realm is 315 K now, and the prelude adds 1.1 M. The premise has changed.

## 4. How other engines do it

| Engine         | What it does                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                      | Source read                                                                       |
| -------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------- |
| V8             | A snapshot of the heap after the bootstrap. A new context is deserialized from the snapshot (`Snapshot::NewContextFromSnapshot` in `Genesis`); the context is made from scratch only when the isolate has no snapshot. The built-in code is embedded in the binary and is not deserialized (`research-v8-reference.md`, "Built-ins and startup"). Lazy init exists only for rare features: `Temporal` is a lazy accessor on `globalThis` (`LazyInitializeGlobalThisTemporal`) and `Bootstrapper::InitializeLazyPartOfContext` fills native-context fields on first use.                                                                                                                           | `bootstrapper.cc` lines 1992-2050, 6905-6925                                      |
| SpiderMonkey   | Lazy standard classes. The global's `resolve` hook (`JS_ResolveStandardClass`) looks the name up in a static table (`standard_class_names`, then `builtin_property_names`, entries `JSStdName` of atom and `JSProtoKey`) and calls `GlobalObject::ensureConstructor`, which makes the constructor, the prototype and all of their methods at once (`resolveConstructor`, with `JSPROP_RESOLVING` so that the define does not call the hook again). `isStandardClassResolved(key)` is one flag for each class. `JS_MayResolveStandardClass` lets callers skip the hook for names that cannot resolve. Self-hosted built-ins (JavaScript) are lazy functions (`instantiateSelfHostedLazyFunction`). | `GlobalObject.cpp` 256-300, 725-745, 891-935; `jsapi.cpp` 924-990                 |
| JavaScriptCore | `LazyProperty` is a pointer with a low tag bit; a read tests the bit and runs the initializer once. `LazyClassStructure` makes the prototype, structure and constructor of a class at the first `get`. Prototypes with many methods use static property tables (`TypeInfo::hasStaticPropertyTable`, `findPropertyHashEntry`) and `reifyStaticProperties` defines the entries of a table, in a `BatchedTransitionOptimizer`. I did not read the per-key path of a lookup.                                                                                                                                                                                                                          | `LazyClassStructure.h`, `LazyProperty.h`, `Lookup.h` 557, `JSObjectInlines.h` 760 |
| QuickJS        | No laziness. `JS_NewContext` runs every `JS_AddIntrinsic*`, and each one calls `JS_SetPropertyFunctionList` over a static `JSCFunctionListEntry` table: one atom, one C function object and one property for each entry. Section 3.7 gives the cost. An embedder can leave intrinsics out of a context (`JS_NewContextRaw`).                                                                                                                                                                                                                                                                                                                                                                      | `quickjs.c`, callgrind                                                            |
| Boa            | No laziness. `Realm::initialize` calls `init` for each built-in (about 80 calls).                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                 | `builtins/mod.rs` 239-330                                                         |

Three points for bare:

- The engines that care about startup do either a snapshot (V8) or a resolve at the first use (SpiderMonkey, JavaScriptCore). SpiderMonkey and JavaScriptCore resolve at the grain of a class, not of a method.
- bare already has the same hooks for `length` and `name` of natives (`F_FNPROPS`, `fn_prep`; 6 sites call `fn_prep` and 3 call `fn_prep_all`) and for the lazy `on*` members of host objects (`Host::lazy_member` with `Lazy::Name`, `Lazy::All`, `Lazy::Symbols`; `design-lazy-properties.md`). A lazy table is the same idea with the same three cases.
- The old engine makes the function `prototype` object lazily (`set_lazy_prototype`) and nothing else.

## 5. Designs

Common facts about js2 that bound all designs:

- A property that a script reads is found in the shape of an object (`Shapes::find`). The inline caches (`Feedback`) store only own hits (shape id and slot). A prototype hit has no cache yet; it walks the chain at every read through `get_prop_g`. So a built-in method read costs a chain walk today, and a lazy test on a miss adds no cost to a hit.
- A property cannot hold a placeholder value. The inline-cache hit path reads the slot with no test, and the S2.9 work counts every extra instruction there.
- A native is a cell with `F_NATIVE` and the index of its `Native` in slot 0. Its `length` and `name` are made at the first use.
- `Vm::call` needs an empty stack, and a native cannot enter script code until #785 is merged. So a lookup, which a native can make, must not run script code.
- Strings have a deterministic content hash (`Chars::hash`, no seed), cached in the atom cell. A static table can be sorted by it.

### A. Lazy global properties

Data. A static table of the global names, `(name, hash, kind)`, sorted by hash. The global object has a flag. The realm keeps a bit for each entry.

Algorithm. A lookup of an own property of the global object that misses, with the flag set, probes the table (binary search on the hash of the atom, then a text compare). On a hit it runs the builder of the entry (it makes the constructor, the prototype or the object with all of its methods), defines the property with the same attributes as an eager install, sets the bit, and retries. The hook sits where `fn_prep` sits, and in `get_global_slow` before the `ReferenceError`.

Cost. Install: nothing for the entries. A miss on the global object costs a probe of about 80 instructions (hypothesis; `typeof x` of an undefined name, a global that a script reads and tests). A hit costs the builder.

Interaction with #774 (global cells, watch tag). Defining a property changes the shape of the global object, so `Shapes::watch_tag` changes and every `GetGlobal` and `SetGlobal` site that holds an address misses once and fills again. This happens today at every top-level `var` and function declaration of every script. A lazy global adds one more such event for each global that a page uses. The cost is bounded by the sites that ran before the event; a lazy global is resolved at its first read, so the sites that have run are few at the time. To verify: count the events on the journeys.

Spec behavior. `hasOwnProperty`, `in`, `delete`, `Object.getOwnPropertyDescriptor`, `defineProperty`, an assignment, a `var` or `function` declaration of the same name, and `Object.getOwnPropertyNames(globalThis)` must all resolve the name first. The first five go through `fn_prep`'s sites. The listing resolves all entries (`Lazy::All`). `typeof JSON` goes through the same miss path as a read, so it resolves. The own-key order of the global object changes, because lazy names come after the eager ones (section 8).

Gain. Only globals that a page never names: here `Math` (88 K with Boolean, Number and the global functions), JSON, 6 of the 7 Error constructors (about 30 K). Most pages name `Object`, `Array`, `String`, `Error`, `Math` and `JSON`. Expected gain with B in place: 25 K or less (hypothesis). Without B: up to 150 K.

Build cost. Small. Risk. Low to medium (the invalidation event, section 8).

### B. Lazy tables for the properties of built-in objects

This is candidate (b) of the request, in its form with no placeholder value. Candidate (b) as written (a pre-built shape with lazy function objects) needs a placeholder in the slot; section B2 says why it is not chosen.

Data. For each built-in object (a prototype, a constructor, `Math`, `JSON`, the global object) one static table of entries `(name, hash, len, kind)`, sorted by hash. `kind` is `Native(NativeFn)`, `Number(f64)`, `Alias(...)` (`Number.parseInt` is the global function) or, in step 2, `Script(fact)`. The text of the names is in one blob and the entry holds an offset, so the table has no relocations except the function pointers. The object has a flag `F_LAZYTAB` and a small id of its table in the realm; the realm keeps a bit set for each table (a `u64` for a table of at most 64 entries, a short `Vec<u64>` for more; `Array.prototype` has about 20 natives and `Math` about 26 in js2 today).

Algorithm.

- Install: make the object with its constructor, set the flag, store the table id. About 300 instructions for each object.
- Lookup of a named own property of an object with the flag (the same sites as `fn_prep`): `shapes.find` first. On a miss, read the cached hash of the atom, test one bit of a 4,096-bit filter of the table (8 instructions, hypothesis; about 1% false positives for 40 entries), then binary search by hash and compare the text. On a hit, make the property (`intern`, `native_cell`, `define_attrs`: the 1.5 K of today), set the bit and retry. When all bits are set, clear the flag.
- Listing of all own names, including non-enumerable ones (`fn_prep_all`'s sites: `getOwnPropertyNames`, `getOwnPropertyDescriptors`, `freeze`, `seal`, `preventExtensions`, `defineProperties` on the object, `assign` from it): make all entries that are left, in table order, and clear the flag. A listing of enumerable names only does not make anything: every built-in property is not enumerable (`HIDDEN`), so `for (k in Array.prototype)` is free.
- `set`, `defineProperty` and `delete` of a name that is in the table: resolve it first, as for a read. Otherwise `Array.prototype.map = f` followed by `delete Array.prototype.map` would show the original again.

Complexity. Install O(objects). A used property O(log N + 1.5 K). A listing O(N). A miss on a flagged object O(1) for 99% of names (the filter) and O(log N) for the rest. Memory: the tables are in the binary (about 24 bytes for each entry; 8 bytes if the name is an offset and the function is an index); the realm holds a few bits for each table.

Expected gain on js2 today: install goes from 28 + 287 = 315 K to about 90 K. The remaining 90 K are the pre-install part (28 K), 14 constructors with their `prototype`, `constructor` and global properties (about 40 K), the properties `name` and `message` of 7 error prototypes (17 K) and the tables (5 K). Gain about 225 K (hypothesis, from the measured 1.5 K for each native and 150 natives less 14 constructors). At the end of the work (600 or more natives) the gain is about 0.9 M. A page that uses 44 natives pays 44 × 1.7 K = 75 K.

Relocations. Each function pointer in a static table is one relocation (16 instructions at start): 150 entries is 2.4 K, 900 entries is 14 K. This is the only cost that grows with the number of built-ins. It is 1% of what an eager install costs for the same entries.

Property order. A property is made when it is first used, so own-key order of a built-in object is the order of first use, and not the order of the specification. See section 8 and question 2.

Descriptors, identity, `Function.prototype.toString`. A property made by the hook has the same attributes (`HIDDEN` = writable and configurable, not enumerable), the same native cell and the same lazy `length` and `name` as an eager one. It is made once, so the identity of `Array.prototype.map` is stable. `toString` of a native is not changed. A test builds a realm eager (a test-only switch) and a realm lazy, resolves all entries and compares the keys, attributes, `length`, `name` and aliases of every object.

Build cost. Medium: the table type, the probe, 9 hook sites, one change for each `install_*` function (the `for (n, len, f) in [...]` loops become tables), and the differential test. Risk. Medium: a missed hook site shows as a missing property. The differential test and test262 find it.

### B2. A pre-built shape with lazy function objects (candidate (b) as written)

Data. For each object, the final shape is made at build time: a table of entries (key atom, slot, attributes). The slots hold a placeholder. The function object is made at the first read.

Algorithm. Install copies the table (12 bytes for each entry, about 10 instructions) and writes the placeholders. A read of a placeholder makes the native cell and stores it.

Cost. Install about 15 instructions for each entry once the atoms are static (below), so 150 natives are 2 K. A use costs 220 (the cell and the `natives` entry), not 1.5 K.

Why it is not chosen:

1. The placeholder. The inline-cache hit reads a slot with no test. An IC fill could refuse a placeholder (it fills after the slow path has resolved it), and the slow readers would test it. But there are many slow readers of `Obj::slot` and `read_loc` (descriptors, copy, JSON, `assign`, the collector), and a placeholder that leaks to a script is an internal value in a script. B has no such value: a property is either there with its final value or not there.
2. The keys are atoms, which are cells of a heap. A static shape table would need static atom cells at fixed offsets and a static hash table of them (relocation-free). That is a second change (static atoms) in `atoms.rs`, which is `unsafe` code under Miri.
3. The spec-visible gain over B is small: B pays 1.5 K for a used property, B2 pays 220. For 44 natives that is 56 K per page (3% of today's startup).

Keep B2 as a later step on top of B (static atoms and pre-built shapes), if the 1.7 K for each used native shows in a profile.

### C. A startup snapshot

Data. At build time, run the realm and prelude set-up, serialize the result (the heap cells, the shape tables, the atom table, the `natives` vector, the codes and facts of the prelude, the root maps) to a blob that is included in the binary. At start, copy it into the realm.

Algorithm. Copy the blob, then relocate: every pointer in a cell, every `Value` that holds a pointer, every atom address in a shape entry and in the atom table gets the new base added. Function pointers are stored as indices. The Rust-side structures (`Vec<Code>`, `HashMap`, `Shapes` tables) are not one arena; each needs its own serializer and its own rebuild, or the realm must be moved to arenas first.

Complexity. O(size of the blob): a copy and one pass for the pointers. The blob is about 0.1 MB of malloc data (DHAT: 96 KB live at the peak with the prelude, 41 KB without) plus the cells of the GC heap (not measured). Hypothesis: 60 K to 150 K at today's size. It grows with the number of built-ins (a 4 times larger realm is about 4 times the cost: 250 K to 600 K), and a lazy table does not.

GC. A cell of the image can point to the young heap after a script runs `Array.prototype.x = {}`. There is no write barrier in js2, so the image is marked like any other cell: its mark cost (hypothesis: about 100 K for 3,000 cells) is paid at every collection. Or the image gets a barrier, which the design of the collector rejects (non-moving, no generations).

Windows and macOS. A mapped image at a fixed address collides with ASLR, so the copy-and-relocate form is needed on all platforms. The build step must run the engine to make the image. `cargo xwin` builds on Linux, so the generator runs on the host for a target that has the same pointer width. A second build stage (a `build.rs` that uses the crate itself, or a checked-in generated file as for the prelude facts) is required. Each change of a `Value` layout, a cell layout or an op changes the image.

Gain. Up to 1.3 M for the realm and the wrapper, if everything is in the image. The same as B plus D2, with 10 times the code and a format that the JIT decision (S5) may change.

Risk. High. Spec risk: none (the realm is the same objects). Engine risk: a format, a verifier, an `unsafe` relocator under Miri, a build stage.

### D. The prelude

The wrapper costs 1.12 M (section 3.1), and it is the largest part. There are three ways.

D0. Run the whole prelude at the first use of any prelude function. The lookup that finds a prelude name in a lazy table (design B) loads and runs the wrapper. Problem: the load runs script code, and a lookup can run inside a native, where `Vm::call` is not allowed (#785 is open). A page that calls `forEach` anywhere pays the whole 1.1 M. The census says 25 of 50 workloads call `forEach`, `map` or `filter`. Expected gain: 1.1 M for the programs that use no prelude function (the `eval_load` rows `loop`, `calls`, `fib`, `closures`, `concat`, `loopfn`, `throws`), nearly 0 for a page.

D1. Split the prelude into units (helpers; `Array`; `String`), each a wrapper of its own, loaded when a name of the unit is first looked up (the `helper()` slots load the helper unit). The same re-entry problem as D0. A page that calls `map` pays the `Array` unit (about 0.55 M by the experiment: 0.39 M and a part of the helpers) and does not pay the helpers (about 0.43 M) and `String` (about 0.06 M). Expected gain for a page: 0.5 M. Needs #785 or a trigger at an op boundary.

D2. A flat prelude with no wrapper. Each prelude function is a top-level function of its own. A name in a lazy table (design B, `kind = Script(fact)`) resolves to a stub made from the facts that exist now (`prelude_facts.rs`): `Lazy::add` makes a stub `Code` of one word, and the first call compiles the function, as today. No script runs at start or at a lookup. A function that the prelude functions share (`obj`, `len`, `int`, `check`, `need`, `bound`, `mergeSort`, ...) is itself a lazy entry. The values that the wrapper captures today (`nativeReplace`, `rawDefine`, `keys`, `getProto`, `setProto`, `getOwn`, `ownNames`, `arrayIteratorProto`) become realm slots.

D2 needs a place for the shared names that is not in a function scope and not in the global object's shape:

- The global object has a limit of 128 properties in the shape tree (`MAX_PROPS`) and goes to dictionary mode past it. About 35 hidden names plus the page's own globals would risk it, and a dictionary global object made `loop` 3.4 times slower once (#763 in `design-js2-superinstructions.md`). Not chosen.
- A scope template: the layout of the wrapper's context as generated data. The compiler of a child function resolves a captured name by the layout of the enclosing scopes, which today comes from compiling the wrapper. It would have to come from data. Option B (a compiled image) of `design-js2-prelude.md` has the same cost. Not chosen.
- A new op, `GetPre bx`, that reads the realm array of prelude slots. The compiler of the prelude unit maps each free name to a slot number (a generated table), and a slot that holds `undefined` is resolved through the table (a stub or an alias) on the slow path. Chosen. The cost of a hit: one load from a realm field and one test, comparable to `GetGlobal` with a cell. A spike must prove it (section 7, step 2).

Gain. Start: the 1.12 M of the wrapper is gone, and a table of generated entries (about 40 entries at 16 bytes) replaces it. A page pays for what it uses: a stub (about 3 K, to verify) for each prelude function that it looks up, and the first-call compile that it pays today. Typical page: about 10 prelude functions, so 30 K. Expected gain: 1.0 M to 1.1 M for any program. Steady state: a prelude function reads its helpers by `GetPre` and no longer by `GetCtx`: to measure on 1,000 calls of `forEach`, `map`, `sort` and `find` (at #772, 1,000 calls cost 8,277 for `forEach`, 23,794 for `sort` and 6,903 for `find`; `design-js2-prelude.md` section 3; not re-measured on #791).

Also: when #785 is merged, a callback method can be a native (`forEach` and the others). Each one that moves leaves the prelude, removes its first-call compile of 58 K, and becomes a table entry of kind `Native`. D2 does not depend on that and does not block it.

Risk. Medium to high: the compiler change (a unit with free names mapped to slots), the new op, the rewrite of `prelude.js` into flat functions, and the generated tables with a freshness test as for the facts.

## 6. Comparison

Gains are for the #791 head (1,958,889 by `perf`) and are hypotheses, except where a measure is named.

| Design                                     | Removes                        |      Startup, script with no built-in | Startup, typical page (44 natives, 10 prelude functions) | Grows with                                                                | Build cost                           | Spec-visible risk                                  |
| ------------------------------------------ | ------------------------------ | ------------------------------------: | -------------------------------------------------------: | ------------------------------------------------------------------------- | ------------------------------------ | -------------------------------------------------- |
| Now (#791)                                 |                                |                                1.96 M |                                                   1.96 M | all built-ins, 1.5 K for each native, and prelude size                    |                                      |                                                    |
| 0. `DECOMPOSE` without pointers            | 2,084 relocations              | 1.93 M (-33 K, measured as the cause) |                                                   1.93 M | nothing                                                                   | hours                                | none                                               |
| A. Lazy globals                            | globals never named            |       -25 K after B; -150 K without B |                                           about the same | globals                                                                   | small                                | order of global names; #774 invalidation event     |
| B. Lazy tables                             | install of unused natives      |                                -225 K |                        -150 K (75 K paid for 44 natives) | used natives (1.7 K each) and 16 bytes of table in the binary             | medium                               | order of own keys of built-ins; a missed hook site |
| B2. Pre-built shape, lazy function objects | install and 1.3 K of each use  |                                -285 K |                                                   -275 K | 220 for each used native                                                  | high (static atoms, `unsafe`)        | a placeholder value in a slot                      |
| C. Snapshot                                | realm and prelude              |                                -1.3 M |                                                   -1.3 M | size of the realm (copy and relocate), and a mark cost at each collection | very high                            | none for the objects; a format that S5 may change  |
| D0. Prelude at first use                   | wrapper                        |                                -1.1 M |                                                  about 0 | whole prelude at first use                                                | small (needs a re-entry rule)        | none                                               |
| D1. Prelude units                          | helpers and a part of the rest |                                -1.1 M |                                                   -0.5 M | unit size                                                                 | medium (needs #785 or an op trigger) | none                                               |
| D2. Flat prelude                           | wrapper                        |                                -1.1 M |                                                   -1.0 M | used prelude functions (3 K for the stub, 58 K at first call)             | high (compiler, `GetPre`, rewrite)   | none; steady state of prelude functions to measure |
| B and D2                                   | install and wrapper            |                  -1.33 M (0.6 M left) |                                     -1.26 M (0.7 M left) | used built-ins                                                            |                                      | as B                                               |

Which removes the most work: C and D2 together with B are equal at about 1.3 M. D2 alone removes 1.1 M, 85% of the possible gain. B removes 0.2 M now and the part that grows (0.9 M at 600 natives).

Which is cheapest to build: step 0 (hours), then A (small, but 25 K), then B. D0 is small to build but gains nothing for a page. D2 is the largest of the three that I recommend.

Spec-visible behavior (property order, descriptors, `toString`, identity): C changes nothing. D0, D1 and D2 change nothing. A and B change only the order of the own keys of built-in objects, and only for listings of non-enumerable names. B2 changes nothing but exposes a placeholder to many readers.

## 7. Recommendation and build plan

Build B and D2, and fix the relocations. Do not build C. Do A only if a measure after B shows more than 25 K left in the global constructors.

Each step is its own PR. Each step runs the same checks:

- Startup: `tools/js/instr.sh script_run x.js` (release, minimum of 3) and a callgrind split with the parts of section 3.1.
- The nine rows of section 3.5: no row above the base of the step by more than 0.5%, and the startup part must drop by the amount below (a row that does not drop is a defect).
- test262: `tools/js/test262_js2.sh -j 8` against `tools/fixtures/js/test262-js2-baseline.txt`. No test that passed may fail. (15,887 pass at the #791 head.)
- `cargo test --locked -p bare-js2 --lib`, with constant collection on the new tests.
- Windows (the VM, as in the engine tests) for any step that touches `Op`, relocations or tables.

| Step | Change                                                                                                                                                                                                   |                   Startup after (hypothesis) | Check that proves it                                                                                                                                                                                                                                                                                                                                                                                                                               |
| ---- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | -------------------------------------------: | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 0    | `DECOMPOSE` in `tools/gen_unicode_tables.py` becomes `(u32, u32 offset, u16 len)` into one `&str` blob. `crates/unicode/src/lib.rs:139` reads the slice. The text of the output is the same.             |                               1.93 M (-33 K) | The count of `R_X86_64_RELATIVE` entries (`readelf -r script_run`) drops from 3,897 by about 2,084; the unicode tests (NFC, IDNA) pass; the browser binary drops by the same. |
| 1    | Lazy tables (B) for the methods, statics and constants of Object, Function, Array, String, Number, Boolean, Math, JSON and the global functions. The constructors and the 7 error prototypes stay eager. |                              1.70 M (-225 K) | Callgrind: `Realm::new` about 90 K. A differential test (an eager realm and a lazy realm, all entries resolved, compare keys, attributes, `length`, `name`, aliases) over every table. Tests for: `hasOwnProperty`, `in`, `delete` then read, `set` then `delete`, `defineProperty`, `freeze` of a lazy object, `Object.getOwnPropertyNames`, the global functions, `Number.parseInt === parseInt`. test262: no loss.                              |
| 2    | Flat prelude (D2): the spike first (the compiler unit, `GetPre`, 3 prelude functions: `forEach`, `sort`, `replace`), then the whole prelude, then the tables of `Script` entries and a freshness test.   | 0.6 M (-1.1 M) for a script with no built-in | Callgrind: no `Lazy::compile` of the wrapper. Steady state of `forEach`, `map`, `sort`, `find` within +0.5% of 1,000 calls. First call of a function within +5 K of today (357 K for `forEach`, which compiles 5 functions). The nine rows: `fib` about 5.0 M, `webpack` about 6.5 M. `bundle_like` below the base. test262: no loss. A test per prelude function that the names, `length`, `name`, attributes and results are the same as before. |

Order. Step 1 comes before step 2, because D2 uses the tables of step 1 for its entries. Step 0 is independent and can go first. The spike of step 2 can run in parallel with step 1.

Expected end state (hypothesis): a script that uses no built-in costs 0.6 M, which is the floor of the process (0.47 M), the script compile (0.06 M) and `main` (0.06 M). Startup then grows by about 1.7 K for each native that the script uses, and by the stub and the first compile for each prelude function that it uses. A new built-in costs nothing at start, except 16 bytes of table and one relocation pair.

### What happens to #791

#791 is +9.6% and the rule allows +0.5%. The plan above recovers all of it and more: step 0 alone recovers 33 K of the 171 K, and steps 1 and 2 recover the rest. Question 1 asks if #791 may merge before them.

## 8. Risks

Spec-visible:

- Own-key order (A and B). A lazy property is made when it is first used, so `Object.getOwnPropertyNames(Array.prototype)` lists used names first and the rest in table order after a listing. ECMA-262 does not fix the order of built-in properties (V8 and SpiderMonkey differ). I did not search test262 for a test that depends on it (to verify: grep `built-ins/*/prototype` tests for `getOwnPropertyNames` and an expected order). The differential test compares the sets and the descriptors, not the order. A prefix rule (make every entry before the used one, in table order) would keep one fixed order and cost about half of the saving. Question 2.
- A name that is not yet made but is in the table must act as if it were there: `hasOwnProperty`, `in`, `delete`, `defineProperty`, `set`, `getOwnPropertyDescriptor`, `Object.keys` (not listed: not enumerable), `freeze`, `seal`, `preventExtensions`, `assign`, spread from the object, `JSON.stringify` of it, `for-in` (not listed), and the host layer's reads. Each is one of the 9 `fn_prep` sites or goes through `own_key_list`. The differential test and a test for each operation cover them. A new site that reads `shapes.find` on a built-in object without the hook is a bug; a comment on `Realm::fn_prep` and a test that greps for `shapes.find` in the lookup functions can guard it. Hypothesis: the list of sites is complete; to verify in step 1 by reading every `shapes.find`.
- `Function.prototype.toString` of a prelude function: unchanged in D2 (the same script function, the same text). Of a native: unchanged (`fn_to_string` returns the same text for all).
- Identity. One cell for each native for the life of the realm (a bit for each entry). `Number.parseInt === parseInt` is an alias entry that must give the cell of the global.
- `length` and `name` of a prelude function (D2) come from the facts at the creation of the stub, as `design-js2-prelude.md` section 10 says. A test pins them.

Engine:

- #774 invalidation (A). A define on the global object changes the watch tag. To count the events on the journeys before A is built.
- The probe on the miss path (B). A page that reads many missing names on built-in prototypes pays the filter test (8 instructions) for each, plus a search for the 1% that pass. To measure on `eval_objects` and `bundle_like` in step 1.
- Re-entry (D0, D1). A lookup inside a native must not run script code until #785. D2 does not run script code at a lookup.
- Realms (all). Each realm has its own bits and cells. A table is static and shared. This helps the question of shared state across realms (`design-js2-prelude.md` section 8, decision 3): a realm that has nothing made costs about 0.1 M.
- GC. A lazily made cell is an ordinary cell and is rooted through the object that holds it, or through `consts` for the natives that the realm keeps. No new root kind.
- `GetPre` (D2). A new op in the interpreter loop. It must not add instructions to other ops. The S2.9 counts are the check.

## 9. Not measured

- Windows and macOS. Wall time. Peak RSS (DHAT gives malloc bytes only: 96 KB live at the peak with the prelude, 41 KB without).
- A prototype of any design. All gains in sections 5 to 7 are hypotheses built from measured unit costs (1.5 K for a native, 1.1 M for the wrapper, 8 instructions for a filter test from reading the code, 3 K for a stub from `design-js2-prelude.md`: not re-measured).
- How many realms a page makes (the browser slice of S2.8).
- Which prelude functions a page calls (the census counts natives of the old engine).
- The share of 2,110 relocations that comes from `DECOMPOSE` (the size matches; the count was not made on a tree without the dependency).
- The mark cost of a snapshot image, the size of the GC heap at start, the per-key path of JavaScriptCore's static tables.
- Whether the `sscanf` of the runtime start is the stack-guard probe (hypothesis).
- The release build at each point of the split: the split uses the `profiling` profile (the same optimization level, with symbols). The total in release for #791 is 1,958,924 (the PR).

## 10. Questions for the owner

1. May #791 merge before steps 0 to 2? It is +9.6% against a +0.5% rule, but the cause is the design that this file replaces, and the next built-in PR will have the same problem. Recommended: yes, with a written waiver, because test262 progress is blocked otherwise and steps 0 and 1 recover its startup cost. Every new built-in PR until step 1 is merged adds the same 1.5 K for each native.
2. Is a changed own-key order of built-in objects acceptable (names used first, the rest in table order after a listing)? Recommended: yes. ECMA-262 does not fix the order, the engines differ, and the differential test compares sets and descriptors. The prefix rule costs half of the gain of step 1.
3. Where do the shared names of the flat prelude live: a new `GetPre` op, hidden keys on the global object, or a generated scope template? Recommended: `GetPre`, after the spike of step 2. The other two risk the 128-property limit of the global shape (a dictionary global was a 3.4 times slowdown, #763) or a format of the compiler's scope data. If the spike fails the gate, fall back to D1 (units) after #785.
4. Should callback built-ins move to Rust after #785 (this removes their first-call compile of 58 K and the prelude)? Recommended: yes, one group for each PR, independent of this plan. Each one that moves becomes a `Native` entry and leaves `prelude.js`. Do not wait for it before step 2: the prelude has 43 functions and many are helpers that stay.
5. Is the gate "no row above +0.5%, and startup must fall by the amount of the table" the right gate for steps 1 and 2? Recommended: yes. Add: steady state of the four prelude rows within +0.5% for step 2.
6. May step 0 change `tools/gen_unicode_tables.py` and `crates/unicode`? It is not js2 code, but the browser binary pays the same relocations. Recommended: yes, as its own small PR. The output must keep the same lookups (a test compares every `DECOMPOSE` entry before and after).
7. Is a snapshot (design C) rejected for good? Recommended: yes for now. Revisit only if, after steps 1 and 2, startup for a typical page is above 1 M, or if the S5 JIT decision fixes the layout of `Code`.
8. Static atoms and pre-built shapes (B2) after step 1? Recommended: not now. Decide with a profile of a typical page after step 2: B2 saves about 1.3 K for each used native (56 K for 44 natives).
9. Does the realm count per page matter for the order of these steps? Recommended: measure it in the browser slice (S2.8) while step 1 is built. If a page makes 5 realms or more, the shared-code decision (`design-js2-prelude.md` section 8) comes before step 2, because D2 pays stubs per realm.
