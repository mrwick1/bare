# Research: keyed access (`GetElem`, `SetElem`) on the page

Date: 2026-11-17. Base: origin/main `4d4b5e5442`. No engine code changed.
Input: lever 5 of `docs/research-lane-profile-after-calls.md` (commit `14f728bd`). The keyed slow paths are 6.8% of the steady page at 50 keys. The key mix was not measured.
Purpose: measure the mix of receivers and keys, the cost per class, and rank the fast paths. This note gives data. It is not a design.

## Summary

- Two classes make 77% of the keyed cost. Both use a string key (an atom) on a plain object in shape mode.
  - `SetElem` that adds a new key: 196,075 ops, 1,064 instructions each, 208.6M (3.10% of the steady page).
  - `GetElem` of an own data property: 474,375 ops, 405 instructions each, 192.1M (2.85%).
- Array index keys (small integers) are 1.2% of the steady page in total. An array fast path alone gains at most about 1% on the page (hypothesis).
- The source is a few `for-in` loops of React and of the bundle helpers: the JSX props copy `for (u in l) u !== "ref" && (c[u] = l[u])`, the rest-props copy, and the React DOM prop diff. The top 10 sites run 69.5% of all keyed ops, the top 20 sites 87.2%.
- A per-site cache keyed by shape and atom does not fit: the key changes at each iteration. Simulated on the page: a one-entry site cache hits 5.8% of the string-key ops, a four-entry site cache 18.0%.
- Recommendation: a global keyed cache keyed by (shape, atom), as the V8 megamorphic stub cache, that reuses the existing `Ic` entries and fill code. Expected 3.5% to 5.5% of the steady page (hypothesis). Then the for-in key cache (lever 2) with slot indices, so that `src[k]` in a for-in body needs no lookup.

## Method

- Machine: AMD Ryzen 9 5900HX, Linux. Other agents ran at the same time. Only instruction counts are used.
- Throwaway counter patch (not committed; reverted after the runs):
  - The `GetElem` and `SetElem` arms call a recorder, then one `#[inline(never)]` wrapper per class (`ek_g_N`, `ek_s_N`, N = receiver class x 8 + key class). Each wrapper calls the normal path (`bc_get_elem`, or `coerce_key` then `put_prop` as in the arm).
  - The callgrind inclusive cost of the call edge into a wrapper is the cost of its class. It does not include the recorder.
  - The recorder classifies the receiver with a heap helper that reads the object record. It keeps per-site counters keyed by (code pointer, pc) and simulates a one-entry and a four-entry (shape, key) cache per site.
- Build: release with `CARGO_PROFILE_RELEASE_DEBUG=line-tables-only CARGO_PROFILE_RELEASE_STRIP=false`, own target dir.
- Page: `type_load /data/bare-tmp/jit-page '#a1' N`, `BARE_JS_EXEC=auto`, N = 0 and 50. The page output did not change: 89,347 evaluator steps per key, the same render counts, `bcstats` as in the brief (`calls_lane=1713027 calls_tree=4720`).
- Callgrind: `GLIBC_TUNABLES=glibc.malloc.tcache_count=65535`, one run each (exact for a run). Steady = 50 keys minus 0 keys. The steady base is 6730.6M (the unpatched callgrind of the profile note).
- Check of the patch: the steady op counts equal the call counts of the unpatched profile (`GetElem` 648,770 against 648,771 `bc_get_elem` calls; `SetElem` 263,492 against 263,492 `put_prop` calls from the loop). The mean cost per `GetElem` is 387 in both builds.
- Caveat: the classifier reads the object's shape with `shape_get`, which fills the global `found` table. That can make the named classes a little cheaper than in the normal build. The mean cost per `GetElem` agrees, so the effect is small (hypothesis).
- Fixtures: the same patch, `eval_load` rows in `byte` mode (scale 10) and some `arrobj_load` cases (scale 1). Counts only, no callgrind.

## 1. Classes

Receiver classes:
- array dense hit: an array, an index key, the element is in `elems` (not a hole);
- array out of bounds: an index at or after `elems.len()` (an append for `SetElem`);
- array hole; array with a named key;
- plain object, own named: shape mode, no `elems`, the key is an own property;
- plain object, named not own: the key is on the prototype chain or absent;
- object with elements or an index key: not an array (the `arguments` object is in this class);
- dictionary-mode object; typed array or view; other exotic (host, proxy, wrapper); string primitive; other primitive.

Key classes: small integer (0 to 2^31 - 1), other number, string that is an index, atom string, string that is not an atom, symbol, object, other.
A short string is always an atom (`heap.rs`, `key_from_string`). A computed short string is an atom too. So "interned constant" and "computed string" are the same class here; the 818 non-atom keys are long strings.
No existing cache serves a keyed op. The only cache on the path is the global `found` table (256 entries, (shape, key) to slot) inside `shape_get`.

## 2. The mix and the cost (page, steady 50 keys minus 0 keys)

| Op | Receiver | Key | Load ops | Steady ops | Steady cost | Per op | Share of steady (6730.6M) |
|---|---|---|---|---|---|---|---|
| SetElem | plain object, named not own (add) | atom string | 6,168 | 196,075 | 208.6M | 1,064 | 3.10% |
| GetElem | plain object, own named | atom string | 10,294 | 474,375 | 192.1M | 405 | 2.85% |
| GetElem | array dense hit | small int | 2,567 | 114,672 | 32.6M | 284 | 0.48% |
| SetElem | array out of bounds (append) | small int | 554 | 19,950 | 21.1M | 1,056 | 0.31% |
| SetElem | array dense hit | small int | 1,000 | 19,650 | 18.2M | 925 | 0.27% |
| SetElem | plain object, own named (store) | atom string | 446 | 24,350 | 16.7M | 686 | 0.25% |
| GetElem | plain object, named not own | atom string | 535 | 18,368 | 10.2M | 553 | 0.15% |
| GetElem | string primitive (`s[i]`) | small int | 766 | 17,300 | 6.8M | 391 | 0.10% |
| GetElem | object with elements (`arguments`-like) | small int | 431 | 20,350 | 5.8M | 284 | 0.09% |
| GetElem | dictionary-mode object | atom string | 4 | 2,411 | 2.8M | 1,161 | 0.04% |
| SetElem | dictionary-mode object | atom string | 52 | 3,100 | 2.2M | 711 | 0.03% |
| GetElem | object, long string key | non-atom string | 18 | 800 | 0.6M | 715 | 0.01% |
| SetElem | other exotic (host) | atom string | 44 | 2 | small | 6,729 | 0.00% |
| SetElem | dictionary-mode object | symbol | 111 | 0 | 0 | | 0.00% |

Totals: steady 517.5M (7.7% of the steady page; the profile note gave 6.8% because it counted only `bc_get_elem` and `put_prop`, without the key coercion of `SetElem`). Load: 15.7M (3.5% of load, 445.97M).
Not seen at all: typed arrays, holes on read, other numbers, string index keys, symbols on read, object keys, proxies.

### 2.1 Where the cost goes in the two large classes (callgrind call edges, 50 keys)

`GetElem`, own named, 405 per op:
- `coerce_primitive` 23 and `to_property_key` 62 (it calls `to_string` on a string, 30, and reads the string record for the digit flag);
- `get_prop` 249: `lookup` 163, of it `shape_get` 69. `lookup` runs `check_key` (the string record again), `obj`, the lazy and exotic tests and `own`;
- the rest is call and return overhead of the layers.

`SetElem`, add of a new key, 1,064 per op:
- `coerce_primitive` 23 and `to_property_key` 65;
- `Heap::set`: `lookup` 212 (it walks the whole chain to `Object.prototype` and misses), then `get_own_property` 116 (the own lookup again), then `define_own_property` 379 (`check_key` again, `find` again, `add_miss` 53 with the transition already cached, `push_slot` 44).
The key is found or missed three times. The transition itself is cheap.

### 2.2 The sites

230 sites at 50 keys. The top sites, with the first keys they saw:

| Site | Op | Ops | Classes | Keys seen | Source (from the bundle) |
|---|---|---|---|---|---|
| 0 | GetElem | 147,369 | own named | value, name, i, type, defaultValue, label | JSX props copy `for (u in l) u !== "ref" && (c[u] = l[u])` |
| 1 | SetElem | 147,369 | add | same | same loop, the store into the new props object |
| 2 | GetElem | 68,561 | own named | forwardRef, createContext, useLayoutEffect, ... | a re-export getter that reads `from[key]` with a captured key (hypothesis: the bundler's copy-props helper) |
| 3, 4 | SetElem, GetElem | 51,438 each | add, own named | asChild, children, type, role, aria-controls, ... | rest-props copy `... t.indexOf(l) < 0 && (n[l] = e[l])` |
| 5, 6, 7 | GetElem | 41,759 each | own named | style, children, ref, type, role, ... | React DOM prop diff loops (`for (v in n) s = n[v], ...`) |
| 8, 9, 12 | GetElem | 29,451, 29,451, 19,200 | array dense hit | 0, 1, 2, ... | array loops |
| 10, 11 | GetElem, SetElem | 20,373 each | object with elements, append | 2, 3, 4, ... then 0, 1, 2, ... | a copy of `arguments` into an array (hypothesis) |
| 13, 14, 15 | GetElem | 18,000 each | own named | id, name, type, className, value, onChange | React DOM prop loops |
| 18 | SetElem | 11,673 | own store or dictionary | `__reactProps$...` (one key) | React's props key on a DOM node |
| 19, 20 | GetElem | 9,033, 8,857 | string primitive | 0, 1 | `s[0]`, `s[1]` |

Concentration: the top 10 sites run 69.5% of the 935,254 keyed ops (load and 50 keys), the top 20 sites 87.2%.
Cache simulation over all string-key ops on shape-mode objects: one-entry site cache 42,748 hits, four-entry site cache 132,353 hits, of about 736,000 ops (5.8% and 18.0%). Only site 18 is monomorphic. The global hit rate of a (shape, key) cache was not measured.

### 2.3 Fixtures (counts only)

| Fixture | GetElem | SetElem |
|---|---|---|
| `eval_load` loopfn, fib, calls, loop (scale 10) | 0 | 0 |
| `eval_load` closures | 50,001 array dense hit | 0 |
| `eval_load` objects | 997 dictionary, atom | 199,871 dictionary, atom; 129 add |
| `eval_load` webpack | 3,500 array dense; 2,996 object with elements | 500 object with elements |
| `arrobj_load` fill, sort, arrays, objects, reverse (scale 1) | array dense hits only (50 to 40,001) | 200 in `objects` (add or dictionary) |
| `arrobj_load` mapfilter, queue | 0 | 0 |

The narrow rows of the brief do not use keyed access, except `closures` (one array read per iteration). The `objects` row writes into an object that went to dictionary mode. Its cost was not measured.

## 3. What a fast path per class would remove

Per-op targets are hypotheses: the cost of the existing inline-cache hit code is about 20 to 60 instructions (profile note, section 1).

| Class | Steady ops | Now per op | Fast path | Target per op (hypothesis) | Removable (hypothesis) | Risk |
|---|---|---|---|---|---|---|
| SetElem add, atom key | 196,075 | 1,064 | global (shape, atom) entry of kind `IC_ADD`: new shape, slot, epoch, prototype | 80 to 150 | 179M to 193M (2.7% to 2.9%) | the chain must have no setter or read-only property for the key; `ic_add` already checks it with `proto_epoch` and `ic_chain` |
| GetElem own, atom key | 474,375 | 405 | global (shape, atom) entry of kind own slot; a string atom key skips `coerce_key` | 40 to 80 | 154M to 173M (2.3% to 2.6%) | low: the shape fixes the slot; dictionary, exotic and lazy objects take the old path |
| GetElem not own and SetElem own store, atom key | 42,718 | 553, 686 | same table, prototype hit and store entries | 50 to 100 | 23M to 25M (0.35%) | as `get_ic` and `set_ic` |
| GetElem array dense, small int | 135,022 (with `arguments`-like) | 284 | `elems[i]` is `Some`: return it | 20 to 40 | 33M (0.5%) | a hole must walk the chain (index on a prototype); a hole falls to the old path |
| SetElem array dense, small int | 19,650 | 925 | `elems[i]` is `Some`: replace it | 30 to 60 | 17M (0.25%) | an element in `elems` is writable by definition (`heap.rs`, `Object::elems`); a frozen array keeps its elements in `dict` (to verify) |
| SetElem append, small int | 19,950 | 1,056 | `i == elems.len()`, array, extensible, `length` writable, no index property on the chain: push and update `length` | 80 to 150 | 18M to 20M (0.3%) | `length` update, a setter or read-only index on `Array.prototype`, non-writable `length` |
| GetElem string, small int | 17,300 | 391 | a table of one-unit strings (V8 keeps one, to verify) | 30 to 60 | 6M (0.1%) | low |

Sum of the named rows: 356M to 391M, 5.3% to 5.8% of the steady page. Sum of the index rows: about 68M to 72M, 1.0% to 1.1%.
The gain shrinks by the miss rate of the global table. A miss costs the old path plus a fill (about 500 per fill for `get_ic` misses, profile note). The miss rate is not measured. Range after misses: 3.5% to 5.5% of the steady page (hypothesis).

### 3.1 Inline in the loop, or out of line

- `GetElem` already calls one out-of-line function (`bc_get_elem`). The fast path can go at the top of that function. The arm does not change, so the loop layout does not change. The call stays (about 10 to 20 instructions).
- `SetElem` inlines `coerce_key` and `put_prop` in the arm. A fast path there changes a hot arm. Better: make the arm call one out-of-line `bc_set_elem` with the fast path first. The arm gets smaller. That is a layout change: measure the five narrow rows before and after.
- An array dense read inline in the arm (no call) saves 10 to 20 more instructions on 135,022 ops: about 2.7M, 0.04%. It is not worth a layout risk on the page. It is worth more on `closures` (50,001 reads in 469.74M, at most 0.2%). Hypothesis.

### 3.2 Make the work unneeded: for-in with slot indices

The large sites are `for (k in src)` bodies that read `src[k]` and write `dst[k]`. The iterator knows the shape of `src` and the order of its keys.
- If the for-in key cache of lever 2 also keeps the slot index of each key, `src[k]` with the current loop key and an unchanged shape is one slot read. No key lookup and no cache probe.
- For `dst[k] = src[k]` into a new object, the writes follow the key order of `src`. The global add entries then hit in order.
- This needs the code generator to mark a `GetElem` whose key register is the loop variable of the enclosing for-in and whose base is the iterated object. It covers sites 0, 4, 5 to 7 and 13 to 15 (hypothesis from the source; the base of each site was not checked).
- Extra gain over the global table: 40 to 80 instructions down to about 10, on about 400,000 reads: 12M to 28M (0.2% to 0.4%). The main value is with lever 2 (`for_in_keys` 288.8M, 4.0%), which the same design removes.

## 4. What other engines do

- V8. Inline caches per site with feedback; a keyed load records name feedback and handles a name that is an array index as an index. In the megamorphic state, `KeyedLoadIC` and `LoadIC` use the stub cache: a global fixed-size hash table keyed by the receiver's map and the property name that holds the handler. Element access uses handlers per elements kind; strings get an indexed-string handler. TurboFan calls the megamorphic keyed load directly and found it worth 2% to 4% on TypeScript benchmarks. For `for-in`, V8 keeps an enum cache on the map with the keys and their field indices (to verify the current name of the optimization that reads `obj[key]` by the enum cache index).
- SpiderMonkey. CacheIR stubs per site. For `for-in`, the property iterator can keep slot indices, so that `obj[key]` in the loop body reads the slot by the iterator index (names such as `LoadSlotByIteratorIndex` and `IteratorHasIndices` are from memory: to verify).
- JavaScriptCore. A per-site `get_by_val` cache with array-mode profiling. Since a rewrite of `for-in`, the enumerator has its own ops for `get_by_val` on the enumerated key (names to verify).
- Ladybird LibJS. A recent change adds per-site caches for keyed access and probes VM-wide caches directly from the interpreter. It tracks shape generations and stops filling a site cache when the keys keep changing. That is the same finding as section 2.2.

Sources:
- [Hidden Classes and Inline Caches in V8](https://chromium.googlesource.com/v8/v8/+/HEAD/docs/runtime/hidden-classes-and-ics.md)
- [v8/src/ic/ic.cc](https://github.com/v8/v8/blob/master/src/ic/ic.cc)
- [V8 stub-cache.h (older version)](https://github.com/chargify/node/blob/master/deps/v8/src/stub-cache.h)
- [[turbofan] Reduce overhead of megamorphic property accesses](https://groups.google.com/g/v8-reviews/c/ePHZikt3A1M)
- [LibJS: Improve interpreter property access caches (Ladybird PR #12560)](https://github.com/LadybirdBrowser/ladybird/pull/12560)

## 5. Ranked recommendation

| Rank | Change | Expected gain, steady 50 keys | Effort | Basis |
|---|---|---|---|---|
| 1 | Global keyed cache, (shape, atom) to an `Ic` entry plus the key, for `GetElem` and `SetElem` with a string atom key on an object; filled by the existing `get_ic` and `set_ic` fills. Out of line: at the top of `bc_get_elem`, and in a new out-of-line `bc_set_elem`. | 3.5% to 5.5% | M | measured class costs; hit rate hypothesis |
| 2 | for-in key cache with slot indices (lever 2 of the profile note, extended) | 4.0% row of lever 2, plus 0.2% to 0.4% | M to L | hypothesis |
| 3 | Array element fast paths: dense read, dense write, append (in the same out-of-line functions) | about 1% | S | measured class costs |
| 4 | One-unit string table for `s[i]` | 0.1% | S | measured class cost |

The global cache can also serve `GetField` sites that miss on many shapes (lever 6). That use was not measured.
Not worth it now: an inline arm for array reads (0.04%), typed-array paths (0 ops on the page), dictionary-mode paths (0.07%).

## 6. Gates and tests for rank 1 and 3

Gates:
- Page, `Auto`, `#a1`, `ic.sh` min of 3: 50 keys and load. Expected: 50 keys down by 3% or more; load not up.
- Narrow rows (`rows.sh`): within +0.5% (the `SetElem` arm changes).
- Cost matrix in `Auto` against main: no row over +0.5% under both allocators.
- Peak RSS with `cp --reflink=never` copies: the table is fixed size (for example 1,024 entries of about 32 bytes: 32 KiB, hypothesis). Within the RSS rule.
- `exec_diff.sh`: `differences: 0`. Unit tests and clippy clean.
- Count the hit rate of the global table with a feature-gated counter before the gain claim.

Tests (each in `Tree`, `Auto` and `Byte`, compared):
- `o[k]` over many shapes and keys in a loop, then a key deleted, then a getter defined for the key on the prototype: the cache must miss after the change.
- An add through `o[k] = v` when `Object.prototype` gets a setter or a read-only property for `k` after the fill (`proto_epoch`).
- A non-extensible, sealed and frozen object; a frozen array; an array with non-writable `length`.
- Holes: `[1, , 3][1]` with `Array.prototype[1]` defined as a value and as a getter.
- An append at `a[a.length]` and a write past the end (a hole in between), checking `length`.
- Keys that are index strings (`"1"`), `"-0"`, `"4294967295"`, a symbol, an object with `toString`, and a long string.
- `arguments` objects and typed arrays (they must take the old path or keep their rules).

## 7. Not tested

- No fast path was built. Every gain in sections 3 and 5 is a hypothesis.
- The hit rate of a global (shape, key) table; the effect of the `found` table fill by the classifier.
- The base object of each site in the source; whether site 2 is the bundler's re-export getter.
- Callgrind cost of the fixtures (counts only); `eval_load objects` (dictionary-mode writes).
- The private React dashboard journey and other pages.
- Narrow rows, cost matrix, RSS, unit tests, `exec_diff.sh`: not run (no kept code).
- Engine sources marked "to verify" in section 4. Some web searches were blocked; those names are from memory.
- Wall time.
