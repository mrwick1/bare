# Design: a keyed property cache for `GetElem` and `SetElem`

Status: proposal for owner review. No engine code is kept. Base: origin/main `37618a8853` (PR #644).
Inputs: `docs/research-keyed-access.md` (the key mix), `docs/design-heap-objects.md` (shapes, `Ic`, `IC_ADD`, epoch), `docs/design-for-in-keys.md` (design B, built in parallel on branch `forin-keys-in-place`).

## Summary

- Two classes cost most of the keyed work on the page: a `GetElem` of an own data property with a string key (475,175 steady ops) and a `SetElem` that adds a new string key (196,075 steady ops). Both run the full generic path. An add finds or misses its key three times: `lookup` (224 instructions), `get_own_property` (104) and `define_own_property` (337 per add).
- The page has few distinct pairs. A trace of the 713,973 cacheable steady ops shows 1,575 distinct (shape, key, read or add) tuples. An unbounded table would hit 99.78%. A table of 4,096 entries, 2-way, hits 97.6%. A table of 1,024 entries, direct-mapped, hits only 85.8%.
- A throwaway experiment of design A (a global table keyed by (shape, key) that reuses the `Ic` entry and its fill code, two direct-mapped tables of 2,048 entries) measured: 50 keys 7,020.64M to 6,804.68M instructions (-3.08%); load 436.88M to 432.57M (-0.99%). The page output and the `bcstats` line did not change. Peak RSS grew by 176 KiB at 50 keys (+0.50%) and 196 KiB at load (+0.73%). That is at or over the gate, so the kept design must use a smaller table.
- Recommendation: design A with a compact 32-byte entry in one shared 2-way table of 2,048 entries (64 KiB), out of line in `bc_get_elem` and a new `bc_set_elem`. Then a single-lookup `Heap::set` (part of design C), which removes two of the three key searches from every add that misses, also outside the lane (`Object.assign`). Then a for-in slot hint on top of the for-in design B, which makes the lookup unneeded at the for-in sites.

## 1. Current design and cost

Code: `crates/js/src/bc_vm.rs` (`bc_get_elem`, the `SET_ELEM` arm), `eval.rs` (`coerce_key`, `get_prop`, `put_prop`, `put_done`), `heap.rs` (`to_property_key`, `key_from_string`, `lookup`, `set`, `get_own_property`, `define_own_property`, `add_data_fast`), `heap_shape.rs` (`Shapes`, `found`, `Ic`, `get_stub`, `set_ic`, `ic_add`, `set_fill`).

### 1.1 Data structures

- **Key.** `PropertyKey` is `Index(u32)`, `String(StrId)` or `Symbol(SymId)`. A string key is an atom: the handle is the content. Every string of at most `ATOM_MAX` (32) units is an atom. The `Str` record has a `digit` flag: set when the string may be an array index or is not an atom. `key_from_string` reads the record once. With `digit` clear, the key is `String(s)` at once.
- **Shape.** A node of a transition tree (`Shape`: `parent`, `count`, `nslots`, `table`, one inline `child`, an optional `children` map). The keys of a chain share one `Table` of `Entry { key, slot, attrs }` records. A table of at most 8 entries is scanned. A longer table builds a lazy `FxHashMap` index (`Table::probe`).
- **`found`.** A global direct-mapped table of 256 `(shape, key, slot, attrs)` answers (`Shapes::found`), probed by `Object::shape_get` inside every `own` call. Index function `recent_at`: `((key.index ^ shape.rotate_left(16)) * 0x9e3779b9) >> 24`.
- **`Ic`.** 24 bytes: `shape` (tagged with `SHAPE_TAG` for every kind but `IC_OWN`), `slot`, `kind`, `depth`, `first` (the receiver's prototype at fill time), `epoch` (`Heap::proto_epoch` at fill time) and `misses`. Kinds: own data, own accessor, store, prototype data or accessor, absent, dictionary, add (`slot` holds the new shape), mega. A debug build also keeps the key and checks every hit against the plain walk (`ic_verify`, `ic_verify_set`).
- **`stubs`.** A global table of 256 `(PropertyKey, Ic)` pairs indexed by `recent_at(shape, key)` (`Heap::get_stub`). Only native reads use it (`get_prop_nat`). A shape sweep resets it. It is design A for reads, in small, already.
- **Epoch.** A fill sets `ic_chain` on every prototype that it walks. A change of the keys, the attributes or the prototype of a flagged object bumps `proto_epoch`. Prototype, absent and add entries stop hitting.

### 1.2 Paths

- `GetElem` (`bc_get_elem`, out of line): `coerce_key` (`coerce_primitive`, then `to_property_key`, which calls `to_string` and `key_from_string`), then `get_prop` → `Heap::get` → `lookup`: `check_key` (the string record again), and for each object on the chain: `obj` (slab read and generation check), the lazy and exotic tests, `own` → `get_prop` → `shape_get` (a `found` probe, else a table scan or probe).
- `SetElem` (inline arm): `coerce_key`, then `put_prop` → `Heap::set`: `check_value`, `exotic_set` when exotic objects exist, `lookup` (walks the whole chain to `Object.prototype` for a new key and misses), `get_own_property` (`check_key` and `own` again), then `define_own_property` → `add_data_fast`: `check_key` again, `find` again, `Shapes::add` (inline `child`, then `add_miss`), `charge`, `push_slot`.
- No site cache serves a keyed op. The research note simulated per-site caches: a one-entry cache hits 5.8%, a four-entry cache 18.0%. The key changes at each iteration.

### 1.3 Complexity

With `d` objects on the chain and `n` keys in a shape: a read is O(d) object visits, and each visit is O(1) for a `found` hit, O(n) for a scan (n ≤ 8) or O(1) expected for a probe. An add is O(d) for the `lookup` miss plus two more own searches plus the transition (O(1) for the inline child or `recent`, else a hash probe).

### 1.4 Measured cost (callgrind, symbols build)

Build: release with `CARGO_PROFILE_RELEASE_DEBUG=line-tables-only CARGO_PROFILE_RELEASE_STRIP=false`, own target dir. Run: `type_load /data/bare-tmp/jit-page '#a1' 50`, `BARE_JS_EXEC=auto`, `GLIBC_TUNABLES=glibc.malloc.tcache_count=65535`, one run (callgrind is exact for a run). Total 7,029.93M. Edges parsed with my own script (not committed). Counts are load plus 50 keys.

`Heap::set`: 415,223 calls, 352.61M inclusive (5.02% of the run), 849 per call:

| Part | Inclusive | Calls | Per call | Per `set` call |
|---|---:|---:|---:|---:|
| `lookup` (first search: chain walk, miss on a new key) | 92.90M | 415,223 | 224 | 224 |
| `get_own_property` (second search: own key again) | 42.98M | 371,132 | 116 | 104 |
| `define_own_property` (third search, transition, slot push) | 140.11M | 371,131 | 378 | 337 |
| `exotic` (the `exotic_set` test) | 6.23M | 415,223 | 15 | 15 |
| `set_value` (own store) | 3.15M | 34,090 | 92 | 8 |
| `set` own code (inlined checks, `check_value`, moves) | 67.25M | | | 162 |

Callers of `Heap::set`: `put_prop` 404,261 calls (from `bc_loop` 271,867, from `ao_assign` 96,051, from `bc_set_field` 31,707), `array_ctor` 8,325. So a cheaper `set` also helps `Object.assign` (92.85M in `put_prop` from `ao_assign`).

Inside `define_own_property` over all its callers (1,446,770 calls): `push_slot` 44 per call, the inlined table scan (`Iter::find`) 28, `check_key` 22, `add_miss` 53, `Table::probe` 73 (57,681 calls).

`bc_get_elem`: 663,388 calls, 256.92M, 387 per call: `get_prop` 255 (of it `lookup` 168), `to_property_key` 62, `coerce_primitive` 23, own code 47. `lookup` over all callers: 185 per call, of it `shape_get` 75.

## 2. How other engines do it

No source was read in this session. The claims below are from memory and from the sources listed in `docs/research-keyed-access.md`. Each one is `to verify`.

- **V8.** A keyed site has its own feedback. When the name changes too often, the site goes megamorphic and uses the stub cache: a global table keyed by (receiver map, name) that holds a handler. It has two levels: a primary table and a secondary table. A primary miss probes the secondary table, and a primary fill moves the old entry to the secondary table. The hash mixes the name's hash field with the map address. Sizes are about 2,048 primary and 512 secondary entries (to verify). Names arrive interned (internalized strings); a keyed load first turns a string key into an internalized string or an index (to verify). For `for-in`, the enum cache on the map holds the keys and their field indices, and a keyed load whose key is the loop variable can read the field by the enum index (to verify).
- **SpiderMonkey.** CacheIR stubs per site. A megamorphic keyed site uses a global megamorphic cache keyed by (shape, id) that holds the slot or "missing" (to verify). Atoms are the property ids. In `for-in`, the iterator can keep slot indices, so that `obj[key]` reads the slot by the iterator index (to verify the names `LoadSlotByIteratorIndex` and `IteratorHasIndices`).
- **JavaScriptCore.** `get_by_val` with a per-site cache and array-mode profiling. Property names are uniqued `AtomStringImpl`s. The `for-in` enumerator has its own ops that read by the cached structure and offset (to verify).
- **Ladybird (LibJS).** Per-site caches for keyed access, and VM-wide caches that the interpreter probes directly. It stops filling a site cache when the keys keep changing (to verify; from the PR named in the research note).

Pattern: all four keep a per-site cache, fall back to one global table keyed by (shape, name) when the name changes, and give `for-in` its own fast read by index. Bare has the global table (`stubs`) for native reads only, and no keyed path uses it.

## 3. Measured: the key mix and the hit rate of a global table

### 3.1 The trace

A throwaway patch (not committed) wrote one line for each `GetElem` and `SetElem` of the lane: the op, the site, the class, the receiver's shape, the key, `proto_epoch` and `shape_sweeps`, and the key count of the shape. The class is decided with `Shapes::find`, not `shape_get`, so the trace does not fill `found`. A `SetElem` is an add when the receiver moved to the child shape of the key (`Shapes::added`).
Check: 935,254 lines at 50 keys (load and typing), the same total as the research note. The `SetElem` lines (271,867) equal the `put_prop` calls from `bc_loop` in the callgrind run. The `GetElem` lines (663,388 with the 18,066 primitive receivers counted) equal the `bc_get_elem` calls. A first version lost 124,000 `GetElem` lines whose result register is the base register. It was fixed before every number below.

Steady ops (50 keys minus the 22,992 lines of load):

| Op | Class | Ops |
|---|---|---:|
| GetElem | own data, string key, shape mode | 475,175 |
| GetElem | array, index key | 115,158 |
| GetElem | object, index key, not array (`arguments`) | 20,350 |
| GetElem | absent, string key | 18,123 |
| GetElem | primitive receiver | 17,300 |
| GetElem | dictionary receiver | 2,414 |
| GetElem | accessor on a prototype | 250 |
| SetElem | add of a new string key (the plain transition) | 196,075 |
| SetElem | array, index key | 39,965 |
| SetElem | own store, string key | 24,350 |
| SetElem | dictionary receiver | 3,100 |

Cacheable ops (string key, shape-mode receiver, a kind that the `Ic` holds): 713,973 (517,898 reads and stores, 196,075 adds). Only 2 distinct epochs and no shape sweep were seen in the steady part.
Size of the receiver's shape at a cacheable read or store: at most 8 keys 319,298; 9 to 16 keys 78,850; more than 16 keys 119,750 (23%). At an add, the shape before the add: at most 8 keys 175,175; 9 to 16 keys 20,400; more than 16 keys 500.

### 3.2 Simulated hit rate

Simulator: my own script (not committed). A tag is (shape, key, side): side "read" for reads and own stores (a read fill makes an `IC_STORE` entry that a store can use), side "add" for adds. A prototype, absent or add entry also needs the epoch of its fill. A shape sweep clears the table. 2-way and 4-way sets use LRU. Hash: `recent_at` with a shift for the table size. An alternative hash (a murmur-style mix) changed the direct-mapped rates by -1.9 to +3.1 points, with no clear winner.

| Entries | Layout | Read | Add | All |
|---:|---|---:|---:|---:|
| 64 | direct | 54.0% | 46.7% | 52.0% |
| 64 | 2-way | 64.0% | 54.0% | 61.2% |
| 256 | direct | 71.4% | 68.9% | 70.7% |
| 256 | 2-way | 75.7% | 72.6% | 74.8% |
| 1,024 | direct | 86.4% | 84.2% | 85.8% |
| 1,024 | 2-way | 89.1% | 87.4% | 88.6% |
| 2,048 | direct | 91.1% | 88.9% | 90.5% |
| 2,048 | 2-way | 94.3% | 93.9% | 94.2% |
| 2,048 | 4-way | 96.4% | 96.0% | 96.3% |
| 4,096 | direct | 94.3% | 93.0% | 94.0% |
| 4,096 | 2-way | 97.6% | 97.4% | 97.6% |
| 8,192 | 2-way | 99.1% | 99.2% | 99.1% |
| unbounded | | 99.78% | 99.77% | 99.78% |

Split tables (one for reads, one for adds, each of the size given): 2 × 512 2-way 87.4%; 2 × 1,024 2-way 93.2%; 2 × 2,048 direct 94.3% (the experiment of section 5); 2 × 2,048 2-way 97.1%. A shared table of the same total size is about as good, so one table is enough.
Conflict misses dominate: 1,575 tuples spread over 4,096 direct-mapped entries collide often. Two ways remove about half of the misses at every size.

## 4. Options

Common rules for every option:
- The fast path takes only a `Value::String` key whose record has `digit` clear (an atom that is no index) and an object receiver. Every other key (numbers, index strings like `'1'`, symbols, objects, long strings that are not atoms) takes the old path with `coerce_key`. A symbol key could use the same table later (no symbol keys were seen on reads).
- The receiver must be ordinary (`exotic == 0`, or the `ROOT_PLAIN` rule of `ic_make`), not lazy, and in shape mode. A dictionary-mode receiver, a proxy and a host object take the old path.
- Keys from a `for-in` loop qualify. With the for-in design B, the loop returns `Value::String` of the shape table entry, which is an atom with `digit` clear (a shape-mode object keeps its index keys in `elems`). So the keyed fast path sees them as any other key.

### A. Global keyed table: (shape, key) to an `Ic` (V8 stub cache)

- **Data.** One table of `N` entries. Entry: the key and an `Ic`. Today `(PropertyKey, Ic)` is 36 bytes. A compact entry for string keys only: `shape u32`, `key.index u32`, `key.gn u32`, `slot u32`, `first ObjId` (8), `epoch u32`, `kind u8`, `depth u8`, 2 spare bytes: 32 bytes. A chain deeper than 255 or a dictionary holder at a position over 255 is not filled.
- **Hash.** `recent_at(shape, key)` with the shift for `N` (the mix of `found` and `stubs`). The key index is a slab index, so consecutive atoms differ in the low bits; the multiply spreads them.
- **Lookup.** Read the receiver's shape; hash; compare shape and key; then the `Ic` hit code that exists (`ic_hit` for reads, `ic_store` and `ic_add` for writes).
- **Fill.** The existing `ic_miss` (`ic_make`, `get_dict`) for reads, and `set_fill` for writes. Both already walk the chain, flag it `ic_chain`, check setters, read-only properties, `extensible`, lazy names and exotic objects. No new fill code.
- **Invalidation.** As the site caches: the epoch for prototype, absent and add entries; `ic_chain` for receivers that are prototypes themselves (`ic_add` refuses them); a shape sweep resets the table (as `stubs` today, in `sweep_shapes`). The shape fixes the own slot, so an own entry needs no epoch. A frozen or non-extensible object, or one whose attribute changed, is in dictionary mode or fails `extensible` in `ic_add`. A key that a setter on the chain would catch is refused by `set_fill` (only a writable data property or nothing on the chain makes an add entry), and a setter added later bumps the epoch, because the fill flagged the chain.
- **`IC_MEGA`.** A global entry must not inherit the per-site miss counter. A refill of a slot starts with `misses = 0`. A pair that the fill refuses keeps an empty entry with its shape, as `get_stub` does, so its next ops take the old path at once.
- **Memory.** 2,048 × 32 bytes = 64 KiB. 4,096 × 32 = 128 KiB. The experiment used 2 × 2,048 × 36 = 144 KiB.
- **Complexity.** O(1) per hit for own entries; O(depth) loads for a prototype hit (`ic_hit` walks `depth - 1` prototypes, no key search). A miss is the old path plus a fill.
- **Expected gain.** Measured with a direct-mapped split table of 2 × 2,048 entries: -3.08% at 50 keys (section 5). A 2-way table of 2,048 entries has about the same hit rate (94.2% against 94.3% in the simulation) at 44% of the memory. Hypothesis: -3.0% to -3.3% at 50 keys.

### B. A per-shape key index by key id

- **Data.** Each `Table` gets a compact index from the key's atom index to the entry position: a small open-addressed array of `(key.index u32, pos u16)` sized to a power of two above `count`, or a sorted array for binary search. It replaces the lazy `FxHashMap` index and is built for every table that a keyed op reads. The transition children map gets the same compact form, keyed by key id.
- **Lookup.** Own read: receiver shape, table, index probe by `key.index`, check the generation in the entry, check `pos < count`. Add: an own probe miss, then the child of the shape for the key (inline `child`, else the compact children index).
- **Invalidation.** None for own reads: a shape and its table prefix never change. Adds still need proof that the chain has no setter or read-only property for the key: the full `lookup` walk, or the chain certificate of design C.
- **Memory.** About 8 bytes per key per probed table. The page's table count was not measured. Hypothesis: 10 to 40 KiB.
- **Complexity.** O(1) expected per own read; no global conflicts; a miss on the receiver is O(depth) table probes up the chain.
- **Expected gain.** Hypothesis: about the same as A on own reads with short tables (a scan of up to 8 entries, which `find` already does, is about as cheap as a probe). It does not serve absent and prototype keys (18,373 steady ops) without a chain walk, and it does not remove the chain proof of an add. Hypothesis: -1.5% to -2.5% at 50 keys.

### C. The shape table as the map, and a single-lookup add

- **Data.** No global table. The own read uses `Shapes::find` on the receiver's shape (a scan up to 8 keys, else `Table::probe`). The add path gets one new structure: a chain certificate, like `enum_tail`: a small direct-mapped table of `(prototype ObjId, proto_epoch, filter)`, where `filter` is a 64-bit Bloom filter of the keys that are accessors or read-only data on the chain from that prototype. `Object.prototype` has the accessor `__proto__` (`eval_object_more.rs`). If all its other keys are writable data (to verify), its filter holds one key.
- **Algorithm.**
  - `GetElem`: fast key check, `find` on the receiver's shape; a hit reads the slot. A miss takes the old path.
  - Single-lookup `set`: one own search on the receiver. A hit with writable data stores in place (it exists). A miss with a certified clean chain for the key (filter bit clear, epoch equal, every chain object ordinary) adds at once: `Shapes::add`, `push_slot`. No `lookup`, no `get_own_property`, no second `find`. Any other case runs today's three steps.
- **Invalidation.** The epoch and `ic_chain` flags, as `enum_tail`. Certify flags each chain object. An index key or an exotic chain object is refused (`tail_plain` rules). No shape-sweep work: the certificate holds object ids and an epoch, no shape ids.
- **Memory.** 16 entries of 16 bytes: 256 bytes.
- **Complexity.** Own read O(n) for n ≤ 8, O(1) expected above. Add O(n) own scan plus O(1) certificate check plus the transition.
- **Expected gain.** Hypothesis, from the callgrind split: the add saves `lookup` (224) and `get_own_property` (104) and part of `define_own_property` (`check_key` 22 and the second `find` about 28): about 350 to 380 per add on all 371,131 adds of `Heap::set`, about 130M to 140M (1.9% to 2.0% at 50 keys), on the lane and on `Object.assign`. The keyed read saves the layers (`coerce_key` 85, `get_prop` and `lookup` overhead, the `found` probe): hypothesis 200 to 250 per read on 475,175 reads, -1.4% to -1.7%. Reads on tables over 16 keys (23%) pay a hash probe (73 per call measured in `define_own_property`).

### D. For-in slot hint (complement, after the for-in design B)

- **Data.** The for-in shape iterator (`ForIn::Shape { o, shape, i, n }`) already knows the position `i` of the current key in the shape table. A `GetElem` whose base is the iterated object and whose key is the loop's current key can read `slot = entries(shape)[i].slot` after one shape compare. This is V8's enum cache index and SpiderMonkey's iterator indices.
- **How the op finds the iterator.** Option 1: the generator emits a new op `GetElemForIn dst, base, key, iter` when the key register is the loop variable and the base is the loop's object expression. Option 2: no new op; `bc_get_elem` compares the key value with the current key of the innermost open shape iterator (`st.iters`). Option 2 needs no generator change but costs a compare on every keyed read.
- **Gain.** Hypothesis from the research note: 12M to 28M over A (0.2% to 0.4%). It removes the hash and the table probe at sites 0, 4, 5 to 7 and 13 to 15. It does not help the stores into the new object (`c[u] = l[u]`), which need the add path.
- It depends on the for-in design B being merged. It must not run in the async tree runner, which keeps its list path.

### Comparison

| | A. Global table | B. Per-shape index | C. Shape table and single-lookup add | D. For-in slot hint |
|---|---|---|---|---|
| Own read | hash, compare, slot | probe on the shape | scan or probe on the shape | shape compare, slot |
| Absent and prototype read | hit | chain walk | chain walk | not served |
| Add | `ic_add` hit | chain walk | certificate, one own search | not served |
| Conflicts | yes (94.2% at 2,048 2-way) | none | none | none |
| Memory | 64 KiB (2,048 × 32 B) | 10 to 40 KiB (hypothesis) | 256 B | none |
| New invalidation | none (reuses `Ic`) | none for reads | certificate (as `enum_tail`) | iterator shape |
| Helps `Object.assign` and other `set` callers | no | no | yes | no |
| Expected, 50 keys | -3.08% measured (144 KiB variant) | -1.5% to -2.5% (hypothesis) | -3.3% to -3.7% (hypothesis) | -0.2% to -0.4% over A (hypothesis) |

## 5. Experiment: design A (throwaway, not kept)

Change: about 90 lines (diff saved outside the repo). Two tables `kget` and `kset` of 2,048 `(PropertyKey, Ic)` entries in `Heap`, indexed by `recent_at` with a 21-bit shift, reset in `sweep_shapes`. `get_keyed` is `get_stub` on `kget`. `set_keyed` tries `ic_store`, then `ic_add`, then `set_fill` into `kset`. `bc_get_elem` takes the fast path for a `Value::String` key whose `key_from_string` gives a string key. The `SET_ELEM` arm calls a new out-of-line `bc_set_elem` with the same fast path, then `put_done`. An object value written while exotic objects exist takes the old path (the view conversion of `put_prop`).
Build: normal release profile, own target dir. Base: the shared base build of `37618a8853`. Both binaries copied with `cp --reflink=never`.

Instructions (`/data/bare-tmp/jit-briefs/ic.sh`, min of 3, `Auto`, `#a1`):

| | Base `37618a8853` | Experiment A | Change |
|---|---:|---:|---:|
| Load | 436.88M (spread 0.006%) | 432.57M (0.005%) | -4.31M, -0.99% |
| 50 keys | 7,020.64M (0.002%) | 6,804.68M (0.015%) | -215.96M, -3.08% |

The saving is 321 instructions per cacheable steady op that the simulation says hits (215.96M over about 673,000 hits). Output at 50 keys: the same value, render counts, evaluator steps per key (89,347) and `bcstats` line (`calls_lane=1713027 calls_tree=4720`).

Peak RSS (median of 7 interleaved runs, fresh `cp --reflink=never` copy before every run, `ru_maxrss` from `wait4`):

| | Base | Experiment A | Change |
|---|---:|---:|---:|
| Load | 26,772 KiB (26,356 to 27,132) | 26,968 KiB (26,692 to 27,288) | +196 KiB, +0.73% |
| 50 keys | 35,276 KiB (34,692 to 35,604) | 35,452 KiB (35,036 to 35,684) | +176 KiB, +0.50% |

The growth matches the two tables (144 KiB), within the run spread. It is at or over the RSS gate (+0.5%). The kept design must use 64 KiB or less.

Not run on the experiment: unit tests, clippy, `exec_diff.sh`, the narrow rows, the cost matrix, a hit counter.

## 6. Recommendation

Build A with a compact table, then the single-lookup `set` of C, then D. A is measured and reuses the fill and invalidation code that the site caches have proven. C attacks the three-search add for every caller of `Heap::set`, and A's misses get cheaper with it. D removes the search entirely where a for-in gives the key. Do not build B: it has no gain over A plus C, and it does not remove the chain proof of an add.

### Step plan

1. **A: global keyed table.**
   - A new `keyed: Box<[KeyedEntry; 2048]>` in `Heap`, 2-way (1,024 sets), 32-byte entries (section 4, A). LRU by swapping the hit way into way 0.
   - `Heap::get_keyed` and `Heap::set_keyed` reuse `ic_hit`, `ic_store`, `ic_add`, `ic_miss` and `set_fill` by converting the compact entry to an `Ic` and back. The fill never sets `IC_MEGA` in this table.
   - `bc_get_elem` gets the fast path at its top (the arm does not change). `SET_ELEM` calls a new `#[inline(never)] bc_set_elem`: the arm gets smaller, which is a layout change of `bc_loop`.
   - Reset the table in `sweep_shapes`. Count hits and fills in `IcStats` (`keyed_hits`, `keyed_fills`) for the tests and the gate.
   - Gate: 50 keys at most 6,830M (base 7,020.64M; the experiment reached 6,804.68M with 2 × 2,048 direct-mapped entries); load not above 436.88M.
2. **C: single-lookup `Heap::set`.**
   - Chain certificate table (16 entries, `(ObjId, epoch, u64 filter)`), certified on a miss walk, flagged with `ic_chain`, checked with the `tail_plain` rules.
   - `Heap::set`: one own search; a hit stores in place; a miss with a clean certificate adds through `add_data_fast`'s steps without its second `find`. Any other case: today's code.
   - Gate: `Heap::set` per call from 849 to at most 550 in callgrind (50 keys); page 50 keys at least 1% below step 1.
3. **D: for-in slot hint**, after `forin-keys-in-place` is merged. First measure how many keyed reads have the loop's current key and object (a counter). Build only if the counter shows at least 200,000 steady reads.
4. **Re-measure** the page at 0, 5 and 50 keys with callgrind in the split of section 1.4. The parent records it in `docs/progress.md`.

### Gates (every step)

- **Instructions:** `ic.sh`, min of 3, `Auto`, `#a1`, load and 50 keys. Report the spread.
- **Narrow rows** (`rows.sh`, `byte`, scale 10): `bc_loop` is layout-sensitive. The fast path stays out of line in `bc_get_elem` and `bc_set_elem`. Each row within +0.5% of the base, measured on the same commit (on main `1047c6d30f`: loopfn 837.06M, fib 58.21M, calls 1,599.28M, loop 2,369.05M, closures 466.70M). Only `closures` runs a keyed read (one array read per iteration), so a change there is layout.
- **Cost matrix:** `BARE_JS_EXEC=auto tools/js/cost_matrix.sh BASE_DIR target` against the base build of the start commit. No row over +0.5% under both allocators, except the rows the brief names. Watch `eval_load objects` (199,871 dictionary-mode writes: they must skip the table cheaply) and `wrapper_load` (String wrappers).
- **Peak RSS:** median of 7 interleaved runs, load and 50 keys, `Auto`, `cp --reflink=never` byte copy before every run. At most +0.5%. The 64 KiB table is about 0.2% of the 50-key peak (hypothesis: the experiment's 144 KiB gave +176 KiB).
- **Correctness:** `cargo test -p bare-js --lib` passes; clippy clean for `crates/js`; `tools/js/exec_diff.sh target` gives `differences: 0`. Debug builds keep `ic_verify` and `ic_verify_set` on every keyed hit (the entry keeps the key in debug, as `Ic` does).

### Tests

Run each in `Tree`, `Auto` and `Byte` and compare (pattern: `a_lazy_function_runs_on_the_bytecode_lane`). Check `keyed_hits` where a hit is the point.

| Case | Test |
|---|---|
| hit | `o[k]` over 3 shapes and 4 keys in a loop: the values are right and `keyed_hits` grows |
| holes | `[1, , 3][1]` with `Array.prototype[1]` as a value and as a getter: index keys never use the table |
| prototype index getter | `Object.defineProperty(Object.prototype, '1', {get(){...}})`, then `o['1']` and `o[1]` on a plain object |
| accessor own | `o = {get a(){...}}`: `o[k]` with `k = 'a'` calls the getter each time; `o[k] = 1` calls the setter |
| accessor on the prototype after the fill | fill an add entry for `k`, then define a setter for `k` on `Object.prototype`: the next `o[k] = v` calls the setter (epoch) |
| read-only on the prototype after the fill | as above with `Object.defineProperty(Object.prototype, k, {value: 1, writable: false})`: the add is rejected, and throws in strict code |
| a key added by a setter on the chain | a setter on the prototype that adds the key to `this`: the first write calls it; later writes see the own key |
| frozen and non-extensible | `Object.freeze`, `Object.seal`, `Object.preventExtensions` between two writes of a key that the table knows: the add is refused, the store follows the attributes |
| `length` on arrays | `a['length'] = 0` and `a[k]` with `k = 'length'`: arrays keep their rules (the fill refuses `length` on arrays) |
| symbol keys | `o[s]` and `o[s] = v` with a symbol: the old path, right result |
| numeric string keys | `o['1']`, `o['01']`, `o['-0']`, `o['4294967295']`: `'1'` is an index, the others are strings; the result equals `Tree` |
| long keys | a key of 40 units (not an atom before the first use): the old path, right result |
| object keys | a key object with `toString`: called once per op, as before |
| dictionary mode | an object with 200 keys (more than `MAX_PROPS`), and one after `delete` of a middle key: right values, no table entry |
| delete | `delete o[k]` of the last key between two reads: the shape changes, the read misses |
| shape sweep | a loop of keyed reads, then enough garbage to sweep shapes, then the same loop: right results; the table is empty after the sweep |
| setPrototypeOf | change the prototype of the receiver between two absent reads: the receiver goes to dictionary mode, the entry misses |
| for-in keys | `for (k in src) dst[k] = src[k]` over several shapes, with the for-in design B on: right copy, `keyed_hits` grows |
| views | `ta[k] = obj` with `k = 'foo'` on a typed array: the view conversion rules hold |
| exceptions | a getter that throws through `o[k]`; `null[k]` and `undefined[k] = 1` give the same messages as before |

## 7. Not measured

- Designs B, C and D: no prototype. Their gains are hypotheses.
- The compact 2-way table of the recommendation: only simulated. The experiment used two direct-mapped tables of 36-byte entries.
- The hit rate of the experiment itself (no counter); the 321 instructions per hit assume the simulated hit rate.
- Narrow rows, cost matrix, unit tests, clippy and `exec_diff.sh` on the experiment.
- The number of keyed reads whose key comes from a for-in loop over the same object (needed for step 3).
- The number of shape tables on the page (memory of design B).
- Engine sources: none read in this session; section 2 is from memory and marked `to verify`.
- Wall time, Windows, the private React dashboard journey and other public journeys.
