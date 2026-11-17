# Design: for-in keys without a key list

Status: proposal for owner review. No engine code is kept. Base: origin/main `4d4b5e5442` (PR #639).
Inputs: `docs/research-lane-profile-after-calls.md` (commit `14f728bd`, lever 2), `docs/design-heap-objects.md`, `crates/js/src/heap_shape.rs` (shapes, `proto_epoch`, `enum_tail`).

## Summary

- Every `for-in` entry builds its key list again: `own_keys`, a hash set, one `peek_own_property` for each key, and a `Vec`. At 50 keys this is 304.5M instructions (4.24% of the run), 4,206 for each of 72,406 loop entries. Each step of the loop then runs `has_property` and `key_to_value`: 73.6M more (209 for each of 351,632 steps). Together: 378M, 5.3% of the 50-key run.
- The page uses one kind of `for-in` only. All 72,406 entries at 50 keys iterate a plain object in shape mode: no elements, not exotic, not a dictionary, all own keys enumerable strings, prototype `Object.prototype`. The certified tail check (`enum_tail_clean`) already skips `Object.prototype` in 72,402 of them. No key comes from a prototype. No key is deleted during a loop.
- So the key list already exists. It is the key table of the receiver's shape (`Shapes::entries`). The recommended design (B) reads the keys from the shape table in place. It builds no list, no set and no `Vec`, and it adds no cache. A step checks that the receiver still has the shape of the loop entry, in place of `has_property` (the V8 `ForInNext` idea).
- A throwaway prototype of B on the lane (not kept) measured: the page at 50 keys fell from 7,167.78M to 6,823.40M instructions (-4.80%), and load from 441.29M to 433.92M (-1.67%). The page output and the `bcstats` line did not change. Peak RSS did not grow.
- Design A (a per-shape cached key list, as V8, JavaScriptCore and Ladybird do) gives about the same gain on the page, but it adds a cache with memory and invalidation. It only wins when keys come from a prototype, and the page has none. Design C (a cheaper rebuild) keeps O(keys) work and one allocation per entry. Recommend B for the lane and the synchronous tree walker, then C for the general path, which the dictionary-mode fixtures use.

## 1. Measured: what for-in runs

Machine: archMachine (AMD Ryzen 9 5900HX), shared with other agents. Only instruction counts are used.
Page: the public React 19 page, `type_load /data/bare-tmp/jit-page '#a1' N`, `BARE_JS_EXEC=auto`, N = 0 (load), 5 and 50 keys.
Tools:
- Callgrind: release build with `CARGO_PROFILE_RELEASE_DEBUG=line-tables-only CARGO_PROFILE_RELEASE_STRIP=false`, own target dir, `GLIBC_TUNABLES=glibc.malloc.tcache_count=65535`. One run for each N (callgrind is exact for a run). Totals: load 446.42M, 5 keys 1,159.00M, 50 keys 7,177.91M. These match the profile note (446.0M, 1,158.5M, 7,176.6M) within 0.03%.
- A throwaway counter (not kept): one line for each `for_in_keys` call with the receiver kind, its shape, prototype, `proto_epoch`, the objects walked, the keys scanned and the keys out; one line for each `bc_for_in_next` step. The counter saw 72,406 entries at 50 keys. Callgrind saw 72,406 calls of `for_in_keys` (62,647 + 8,425 from `bc_loop`, 1,334 from `enter_rare`).
- Function costs come from the callgrind call edges, parsed with my own script (`/data/claude-tmp/forin/py/cg.py`, not committed).

### 1.1 Loop entries and receivers (page, `Auto`)

| | Load | 5 keys | 50 keys |
|---|---:|---:|---:|
| Loop entries over an object | 1,481 | 8,573 | 72,406 |
| Entries over a primitive (`null`, `undefined`, string) | 0 | 0 | 0 |
| Receiver plain, shape mode, no elements, not exotic | 1,481 (100%) | 8,573 (100%) | 72,406 (100%) |
| Receiver in dictionary mode, array, with elements, exotic, proxy | 0 | 0 | 0 |
| Receiver with a non-enumerable own key | 0 | 0 | 0 |
| Entries where a prototype gives a key | 0 | 0 | 0 |
| Entries that walk only the receiver (the tail is certified) | 1,478 | 8,569 | 72,402 |
| Distinct prototypes | 1 | 1 | 1 |
| Mean keys out per entry | 5.55 | 5.11 | 5.03 |
| Steps (keys given to the body) | 7,857 | 42,233 | 351,632 |
| Steps skipped because the key was deleted | 0 | 0 | 0 |
| Distinct receiver shapes | 146 | 147 | 147 |
| Share of entries on the top 10 shapes | 51.1% | 61.3% | 63.1% |
| Distinct `proto_epoch` values seen | 3 | 4 | 4 |

Keys out per entry at 50 keys (count of entries): 1 key 8,402; 2 keys 13,158; 3 keys 8,087; 4 keys 2,666; 5 keys 17,583; 6 keys 10,399; 7 to 10 keys 6,135; 11 to 18 keys 5,976. No entry has more than 18 keys.
At 50 keys the loops list 364,237 keys and give 351,632 to the body. Hypothesis: the other 12,605 keys (3.5%) are not reached because the body leaves the loop early (`break` or `return`). The counter did not record the exits.

Simulated cache (from the counter lines): a cache keyed by `(shape, prototype, proto_epoch)` hits 72,066 of 72,406 entries at 50 keys (99.53%; 340 misses). Keyed by `(shape, prototype)` only, it hits 72,259 (147 misses). The shape ids in the log are not checked against shape sweeps. A sweep renumbers shapes, so the simulated hit rate may be a little high (hypothesis).

### 1.2 Fixtures with for-in (cost-matrix rows)

| Row | Loop entries | Receiver | Keys per entry | Objects walked |
|---|---:|---|---:|---:|
| `eval_load objects 10` | 1 | dictionary mode (997 keys, more than `MAX_PROPS`) | 997 | 2 |
| `arrobj_load descriptors 10` | 1 | dictionary mode (2,000 keys) | 2,000 | 2 |
| `wrapper_load forin 1000 3000` | 1,001 | String wrapper (exotic) | 3 index keys | 1 (999 of 1,001 skip the tail) |

None of these rows can use a shape path. They are the "must not grow" rows of the cost gate. `tools/fixtures/js/*-cases.txt` and `crates/js/src/eval_forin_tests.rs` hold the semantic cases (section 6).

### 1.3 Where the instructions go (callgrind, 50 keys, 7,177.9M)

`for_in_keys`, inclusive 304.5M (4.24%), 72,406 calls, **4,206 for each entry**:

| Part | Instructions | Calls | Each |
|---|---:|---:|---:|
| `property_keys` → `own_keys` (three `Vec`s for index, string and symbol keys, then one more for the result) | 81.4M | 72,410 | 1,124 per object |
| `HashMap::insert` of the seen set (`FxSet`), with its growth | 71.4M | 364,237 | 196 per key |
| `peek_own_property` (look up the key just read from the shape, to read its enumerable bit) | 71.2M | 364,237 | 196 per key |
| `RawVec::grow_one` (the result `Vec` grows by push) | 30.8M | 123,614 | 250 |
| `__rust_dealloc` | 5.1M | 144,816 | 35 |
| `for_in_keys` own code (loop, `tick`, `enum_tail_clean`, inlined `Vec` and set code) | 44.6M | | 616 per entry |

Each step, outside `for_in_keys`:

| Part | Instructions | Calls | Each |
|---|---:|---:|---:|
| `has_property` (the deleted-key check of `bc_for_in_next`) | 58.5M | 351,632 | 166 |
| `key_to_value` | 15.1M | 351,632 | 43 |

Total for-in work: 378.1M, 5.3% of the 50-key run (plus the dispatch of `FOR_IN_OPEN` and `ITER_NEXT`, not split).
At 5 keys `for_in_keys` is 37.1M of 1,159.0M (3.20%). At load it is 7.0M of 446.4M (1.57%, 4,735 per entry).

The work does not depend on the data. The receiver has one shape, the shape table already holds the keys in order with their attribute bits, and the tail is already certified empty. The current code copies the keys out of the table (`own_keys`), puts each into a hash set to remove duplicates that cannot exist (a single object has unique keys), and looks each up again to read a bit that the table entry holds.

## 2. Semantics that must hold

From ECMA-262 `EnumerateObjectProperties` (14.7.5.9) and the behaviour the tests check against Node:

1. **Order.** Own keys come in `[[OwnPropertyKeys]]` order: integer keys ascending, then string keys in creation order. Symbols never appear. Then the keys of the prototype, in the same order, and so on up the chain.
2. **Shadowing.** A prototype key with the same name as a key already seen (on a closer object) is not visited. Enumerability does not matter for this: a non-enumerable own key hides an enumerable prototype key (test `shadow_nonenumerable`).
3. **Deleted keys.** A key deleted before the loop reaches it is not visited.
4. **Added keys.** A key added during the loop need not be visited. Bare does not visit it, as V8 does not.
5. **At most once.** No key is visited twice.
6. **Prototype chain.** Enumerable keys of every prototype are visited, unless shadowed. The chain of a proxy asks the proxy (`px_for_in_has` each step, as V8 does).
7. **Receivers.** `null` and `undefined`: no iteration. A string primitive: its indices. A String wrapper: its indices, then its own keys. An array and a typed array: their indices, then their own keys. A proxy: its `ownKeys` and `getOwnPropertyDescriptor` traps. A dictionary-mode object: the same order as a shape-mode object.
8. **Limits.** The step limit and the memory accounting still apply. A for-in over a huge object must stay bounded.

Bare today: rules 1 to 7 hold (the Node-oracle table `eval_forin_tests.rs`, 33 cases, and the fixtures). Rule 4: `for_in_keys` takes a snapshot at entry. Rule 3: each step runs `has_property` on the receiver (or `px_for_in_has` with a proxy).

## 3. How other engines do it (sources read)

I read these sources. The large files were read in part. The limits are named.

- **V8, key collection.** `src/objects/keys.cc` (https://raw.githubusercontent.com/v8/v8/main/src/objects/keys.cc, through a fetch tool summary). `FastKeyAccumulator::Prepare` walks the prototypes; a prototype whose map has enum length 0 and no elements is "empty". With an empty chain, `GetKeysFast` returns the keys of the receiver's map from its enum cache (`GetFastEnumPropertyKeys`: "Users of this function have to make sure to never directly leak the enum cache"). The enum cache lives on the descriptor array, which maps of one transition tree share. With a non-empty chain, a per-prototype cache (`prototype_chain_enum_cache`) is checked with a prototype validity cell. The slow path (`KeyAccumulator`) starts shadow checks only "after the first prototype has added shadowing keys", with an `OrderedHashSet`.
- **V8, the loop step.** `src/interpreter/interpreter-generator.cc`, handler `ForInNext` (downloaded): "Check if the expected map still matches that of the {receiver}. If so, {cache_array} is the enum cache keys FixedArray." On a match the key is loaded and there is no `has` check. Otherwise it calls the filter (`ForInFilter`, a `HasProperty`).
- **SpiderMonkey.** `js/src/vm/Iteration.cpp` (through a fetch tool summary). A `NativeIterator` snapshots the keys and stores the shapes of the whole prototype chain as its guard. It is cached on the receiver's shape (`LookupInShapeIteratorCache`) and in a realm table hashed by the chain's shapes. An object with dense elements is not cacheable. If no prototype may have enumerable keys (`ProtoMayHaveEnumerableProperties`), the walk is own-only and needs no visited set. Deletion during the loop is handled by eager suppression: `SuppressDeletedProperty` removes the key from every active iterator over that object, so a step needs no `has` check.
- **JavaScriptCore.** `runtime/JSPropertyNameEnumerator.h` and `JSPropertyNameEnumeratorInlines.h` (through a fetch tool summary). The enumerator is cached on the `Structure` (`cachedPropertyNameEnumerator`) only when the object has no indexed properties. It is checked with the structure's prototype chain (`StructureChain`). Modes: indexed, own structure, generic. A step in own-structure mode compares the structure id (`m_cachedStructureIDForNames`), and can also read the value by slot (`m_cachedStructureIDForNamesAndOffsets`).
- **Ladybird (LibJS, Rust interpreter).** `Libraries/LibJS/Rust/src/interpreter/slow_paths/property_access.rs` (downloaded, read in full for for-in). `get_object_property_iterator_impl` keeps a key snapshot per bytecode site and per shape (`set_property_iterator_cache`). `object_property_iterator_cache_matches` checks the shape key and a prototype chain validity token. If no prototype has an enumerable string key, it copies the keys straight from the shape "without any shadowing work". The visited set is made lazily (`ensure_seen_properties`). A step falls back to `has_property()` "once the snapshot's shape guards no longer hold".
- **QuickJS.** `quickjs.c` (downloaded, `build_for_in_iterator`, `js_for_in_prepare_prototype_chain_enum`, `js_for_in_next`). No cache across loops. Own keys first; the prototype chain is checked only when the own keys run out. If no prototype has an enumerable key, the loop ends. Else the visited keys go into a property table of the iterator object. A fast array stores only its length. Each step checks the key with `JS_GetOwnPropertyInternal`.
- **Servo** uses SpiderMonkey. Not read separately.

Pattern: all four engines avoid the rebuild for a receiver whose shape is known and whose chain has no enumerable key. V8, JavaScriptCore and Ladybird keep a key list cached on the shape. V8 and JavaScriptCore check the shape at each step in place of `has`. Bare's shapes differ in one point that matters here: the keys of a shape already sit in order in a shared `Table` (`Shapes::entries`), with their attribute bits. So the cached list of the other engines is already there.

## 4. Current design and complexity

- `for_in_keys` (`eval.rs:3869`): for each object on the chain until a certified tail: `property_keys` → `own_keys` (allocates up to four `Vec`s), then for each key: `tick`, `FxSet::insert`, `o_peek` for the enumerable bit, push. Time O(K) with K keys on the walked chain, with about 1,100 fixed and about 600 per key. Memory: a `Vec<PropertyKey>` (12 bytes a key, hypothesis from the enum layout) and a hash set of K entries, both temporary.
- The certified tail (`Heap::enum_tail`, 16 direct-mapped entries `(ObjId, proto_epoch)`, `enum_tail_clean`, `enum_tail_certify`): the walk stops before a tail that has no enumerable string key at the current `proto_epoch`. A change of a key, an attribute or a prototype of a flagged (`ic_chain`) object bumps the epoch. Each use checks `tail_plain` (about 8 loads per tail object) because index keys are not covered by the epoch.
- The lane: `FOR_IN_OPEN` pushes `BcIter::In { obj, keys, i, proxied }` on `RegStack::iters`. `ITER_NEXT` calls `bc_for_in_next`: `has_property` (or `px_for_in_has`) on the receiver, then `key_to_value`.
- The tree walker: `exec_for_each` (`eval.rs:3778`) uses the same list and check. The async runner (`eval_async.rs`, `in_start` and `in_next`) copies the keys into run slots, because a suspended run lives across collections.
- Shapes (`heap_shape.rs`): `Shapes::entries(shape)` is `tables[t].entries[..count]`. A table only grows (`Table::push`). A second branch copies it. A shape sweep (inside `collect`) renumbers shapes and truncates tables past the longest live user. A shape-mode object has no index keys in its shape: an index key lives in `elems` or moves the object to a dictionary.

## 5. Options

### A. Per-shape cached key list (V8 enum cache, JavaScriptCore, Ladybird)

- Data: a side table `FxMap<u32, EnumEntry>` keyed by the receiver's shape id, `EnumEntry { keys: Rc<[PropertyKey]>, proto: Option<ObjId>, epoch: u32 }`. Cleared by a shape sweep (as the inline caches are: `bc_reset`).
- Entry: look up the shape; check `proto` equal, `epoch == proto_epoch` and `tail_plain`; clone the `Rc`. Miss: run `for_in_keys`, store.
- Step: compare the receiver's shape with the entry shape; on a match, no `has` check.
- Complexity: O(1) per entry on a hit (a hash probe and a refcount). O(K) on a miss.
- Memory: one list per distinct enumerated shape: 147 to 340 on the page, about 5 keys each. Hypothesis: 25 to 40 KiB with the map. It can also cover receivers with prototype keys (the key list of the whole chain), which B does not.
- Expected gain on the page: about the same as B, minus a hash probe and a refcount per entry (hypothesis: 60 to 100 more instructions per entry than B).
- Cost: a new cache, its invalidation (sweep, epoch, `tail_plain`), and its memory accounting. A cache that holds keys must also be safe for GC when the collector runs inside a call later (`design-gc-in-call.md`).

### B. Read the shape table in place (recommended)

- Data: no new heap data. A new iterator kind `ForIn::Shape { o: ObjId, shape: u32, i: u32, n: u32 }` (16 bytes plus the tag) next to the list kind `ForIn::List`.
- Entry: the receiver is an object with `exotic == 0`, `lazy == 0`, shape mode (`shape != 0`), no elements, and its prototype is `None` or a certified clean tail (`enum_tail_clean`). Then `n = count(shape)`. Else the list path as today. Cost: a slab read, four flag tests, `enum_tail_clean`. No allocation.
- Step: read `entries(shape)[i]`; skip an entry that is not enumerable or is a symbol (one bit test); if the receiver still has `shape`, the key is live; else run `has_property` as today. Return `Value::String` directly (the key is a string: no `key_to_value`).
- Why it is correct:
  - Order: the shape table lists string keys in creation order. A shape-mode object has no integer keys (rule 1).
  - Shadowing and the prototype: the certified tail has no enumerable string key and no element (rule 2, 6). The receiver's own keys are unique (rule 5).
  - Deleted keys: any delete changes the shape (to the parent for the last key, else to a dictionary). Then each step runs `has_property` (rule 3). A key that is deleted and added again gives the same shape back, and `has_property` would say "present" too: the result is the same as today.
  - Added keys: `n` is fixed at entry. An add moves the receiver to a child shape. The prefix `entries(shape)[..n]` does not change, because a table only grows (rule 4).
  - Attribute changes (`defineProperty`, `freeze`) move the receiver to a dictionary. Then the `has` path runs, as today.
  - A prototype change during the loop does not change the keys of today's snapshot either: the snapshot is taken at entry in both designs.
- The one new invariant: **no shape sweep while the iterator is open.** Today `Interp::collect` returns an error when `depth != 0 || stmt_frames != 0` (`eval.rs:984`), the lane runs functions only (verified by reading; to verify with a debug assertion in step 1 that `depth >= 1` during a lane run), and generators and async functions are refused by the lane (`"await or yield"`). The async tree runner keeps its list path, because its run can be suspended across a collection. If GC inside a call is built later (`design-gc-in-call.md`), it must either remap the shape id of open `ForIn::Shape` iterators or turn them into lists before the sweep.
- Complexity: O(1) per entry, O(1) per step. Memory: none on the heap. The iterator is smaller than `BcIter::In` (no `Vec`).
- Fit with `design-heap-objects.md`: it uses the shapes and the shared key tables as they are, and the `proto_epoch` and `ic_chain` contract through `enum_tail_clean`. It adds no epoch and no cache. The same per-step shape compare is what a JIT would emit (one load and one compare).

### C. A cheaper rebuild (QuickJS, the V8 slow path, Ladybird's set-free path)

- Keep the list. Remove the parts that are not needed:
  - no seen set for the receiver: its own keys are unique; make the set only when a second object gives keys (V8 "start the shadow checks only after the first prototype has added shadowing keys", Ladybird `ensure_seen_properties`);
  - read the enumerable bit from the shape entry or the dictionary entry (`each_key` with attributes), not with `peek_own_property`;
  - one `Vec` with the exact capacity, no `own_keys` copies.
- Complexity: still O(K) per entry, one allocation. Expected: about 1,000 to 1,500 per entry on the page in place of 4,206 (hypothesis from the table in 1.3: the set and the peek are 392 per key, the `own_keys` copies 1,124 per object).
- It helps every receiver: dictionary-mode objects (the two fixture rows with 997 and 2,000 keys), arrays, the first loop before a tail is certified.

### D. Lazy snapshot with a per-key liveness check by shape

- The step part of A and B: `has_property` only when the receiver's shape changed (V8 `ForInNext`, JavaScriptCore own-structure mode). On the page: 58.5M of `has_property` become one compare per step. It is not a design on its own. It needs a key list or a shape to compare with.
- SpiderMonkey's alternative (suppress deleted keys in every open iterator at delete time) moves the cost into `delete` and needs a list of open iterators. Bare's `delete` is rare on the page. B gets the same effect with one compare and no list.

### Comparison

| | A. Cached list per shape | B. Shape table in place | C. Cheaper rebuild |
|---|---|---|---|
| Work at entry, page | hash probe, checks, refcount | slab read, checks, `enum_tail_clean` | own keys copy, one `Vec` |
| Work per key at entry | none | none | push |
| Work per step | shape compare | shape compare, attribute bit | `has_property` (166) unless combined with D |
| Heap memory | 25 to 40 KiB (hypothesis) | none | none (temporary list) |
| Invalidation | sweep, epoch, `tail_plain` | existing `enum_tail` only | none |
| Covers prototype keys | yes | no (list path) | yes |
| Covers dictionary, arrays, exotic | no | no | yes |
| Expected gain, page 50 keys | about 4.7% (hypothesis) | 4.80% (measured, prototype on the lane) | about 3% (hypothesis) |

## 6. Recommendation

Build B with the shape check of D, on the lane and in the synchronous tree walker. Then build C for the list path. Do not build A: the page has no receiver that A covers and B does not. Revisit A only if a profile shows for-in over objects whose prototypes give keys (class instances with enumerable prototype methods, for example).

### Step plan

Every step: a test that fails first; `cargo test -p bare-js --lib` passes; clippy clean for `crates/js`; `tools/js/exec_diff.sh target` gives `differences: 0`; the gates below.

1. **Shared iterator, shape path.** Add `ForIn { Shape {..}, List {..} }` in the evaluator. `FOR_IN_OPEN` and `exec_for_each` choose the kind. `ITER_NEXT` and the tree loop step through it. Keep `BcIter::In`'s list path for every other receiver. The async runner (`in_start`, `in_next`) is not changed. Add a counter `for_in_shape` to the stats for the test (the failing test: "a for-in over a plain object reads the shape and builds no list"). Add a debug assertion that no collect runs while a `ForIn::Shape` is open.
2. **Cheaper list path (C).** No seen set until a second object gives a key; attribute bits from the entries; one exact `Vec`. Gate: the fixture rows `eval_load objects`, `arrobj_load descriptors` and `wrapper_load forin` get cheaper or stay within +0.5%.
3. **Re-measure.** Callgrind of the page at 0, 5 and 50 keys, in the split of section 1.3. The numbers go to `docs/progress.md` (by the parent).

### Gates

- **Instructions, page** (`/data/bare-tmp/jit-briefs/ic.sh`, min of 3, `Auto`, `#a1`): load not worse than 441.03M; 50 keys at most 6,900M after step 1 (base 7,167.71M; the prototype measured 6,823.40M). Report the spread.
- **Narrow rows** (`/data/bare-tmp/jit-briefs/rows.sh`, `byte`, scale 10): `FOR_IN_OPEN` and `ITER_NEXT` are arms of `bc_loop`, which is layout-sensitive. Each row within +0.5% of loopfn 837.05M, fib 58.20M, calls 1,599.28M, loop 2,369.05M, closures 469.74M. A worse row needs a fix or a clear reason. Keep the step code in a helper function, as `bc_for_in_next` is now, and keep the arms small.
- **Cost matrix** (`BARE_JS_EXEC=auto tools/js/cost_matrix.sh BASE_DIR target`, base `4d4b5e5442`): no row over +0.5% under both allocators. The known layout rows (`builtins_load replace`) and the two `parse_load deep` rows are named in the brief.
- **Peak RSS**: median of 7 interleaved runs, load and 50 keys, `Auto`. Copy each binary with `cp --reflink=never` before every run. No growth over 0.5% (hypothesis: B lowers it a little, because the per-entry `Vec` and set go).

### Tests (one or more for each rule of section 2)

Run each source in `Tree`, `Auto` and `Byte` and compare (the pattern of `a_lazy_function_runs_on_the_bytecode_lane`). Add Node-oracle rows to `eval_forin_tests.rs` (`CASES`) for the results; take the expected texts from Node 22.

| Rule | Test |
|---|---|
| 1 order | `{b:1, 2:1, a:1, 1:1}` gives `1+2+b+a` (list path, elements); `{z:1, y:1, x:1}` gives `z+y+x` (shape path); the same keys in a dictionary-mode object (more than 128 keys) give creation order |
| 1 symbols | an own symbol key in the shape is skipped; a symbol on `Object.prototype` is skipped (exists: `symbol_keys_ignored`) |
| non-enumerable own key | `Object.defineProperty(o, 'h', {value: 1})` on a shape-mode object: the shape has a non-enumerable entry; `h` is skipped |
| 2 shadowing | exists: `shadow_nonenumerable`, `shadow_by_own_key_in_tail`, `class_syntax_methods` |
| 3 deleted | exists: `delete_during_loop` (now on the shape path); new: delete the last key (the shape goes to the parent), delete then add the same key again, delete from a getter that the loop body calls, delete in a nested for-in over the same object |
| 4 added | exists: `add_own_during_loop`; new: add a key, then delete it, during the loop (the shape changes twice) |
| 5 once | two nested loops over the same object; a key both own and on the prototype |
| 6 prototype | exists: `class_proto_enumerable_method`, `inheritance_dirty_middle`, `object_create_null`, `object_create_array_proto`, `add_during_loop`, `set_prototype_of_*`; new: `setPrototypeOf` of the receiver during the loop |
| 7 receivers | `for (k in null)` and `for (k in undefined)` run 0 times; exists: `string_object`, `plain` (string primitive), `typed_array_*`, `proxy_receiver`, `proxy_proto`; new: an array with a hole, a function (its `lazy` bit keeps the list path), a frozen object (dictionary) |
| exits | `break`, `return` and a `throw` from the body on the shape path (the iterator is popped: `ITER_CLOSE` and the unwind) |
| sweep | a collect between two scripts that loop over the same object: shape ids change, the result does not |
| 8 limits | a loop over a 100,000-key dictionary object still hits the step limit in `Tree`; the shape path does no per-key work at entry |
| counters | the failing-first test: `for_in_shape` counts one for each entry over a plain object, zero for a dictionary, an array, a proxy and a String wrapper |

## 7. Prototype of B (throwaway, not kept)

Change: about 50 lines in `bc_vm.rs` and `heap_shape.rs` (saved outside the repo; not committed). `FOR_IN_OPEN` pushes a new `BcIter::Shape { o, shape, i, n }` when `Heap::for_in_shape` accepts the receiver (the entry rules of B). `ITER_NEXT` reads the next enumerable string entry of the shape, and runs `o_has_property` only when the receiver's shape changed. The tree walker and the list path did not change.
Build: normal release profile, own target dir. Base: the shared base build of `4d4b5e5442` (`/data/bare-tmp/base/4d4b5e5442/target`). Both binaries were copied with `cp --reflink=never`.

Instructions (`ic.sh`, min of 3, `Auto`, `#a1`):

| | Base `4d4b5e5442` | Prototype B | Change |
|---|---:|---:|---:|
| Load | 441.29M (spread 0.003%) | 433.92M (0.001%) | -7.37M, -1.67% |
| 5 keys | 1,152.39M (0.003%) | 1,111.83M (0.027%) | -40.56M, -3.52% |
| 50 keys | 7,167.78M (0.001%) | 6,823.40M (0.002%) | -344.38M, -4.80% |

The cut at 50 keys (344.4M) is 91% of the for-in work measured in section 1.3 (378.1M). It is more than `for_in_keys` alone (304.5M), because the steps lost `has_property` and `key_to_value` too.

Output at 50 keys: the same value, render counts and `bcstats` line (`calls_lane=1713027 calls_tree=4720`). The evaluator steps per key fell from 89,347 to 82,471: the shape path does not `tick` once per key at entry. A run that ends at the step limit can end later with B. The step limit guards against runaway loops, and B does no per-key work at entry, so this is the intended effect. Rows of `exec_diff.sh` that end in `limit` match any result.

Peak RSS (median of 7 interleaved runs, fresh `cp --reflink=never` copy before every run, `ru_maxrss` from `wait4`):

| | Base | Prototype B |
|---|---:|---:|
| Load | 26,968 KiB (min 26,612, max 27,256) | 26,824 KiB (26,448 to 26,992) |
| 50 keys | 35,184 KiB (34,936 to 35,492) | 35,088 KiB (34,980 to 35,420) |

The differences (-144 KiB and -96 KiB) are inside the spread. No growth.

Unit tests: `cargo test -p bare-js --lib for_in` with the prototype: 4 passed. The 33-case Node table runs in the default mode, `Exec::Tree`, which the prototype did not change. Of the 4, only `for_in_and_for_of_run_on_the_bytecode_lane` ran the new lane path. The page output is the main check of the prototype. The full suite, clippy, `exec_diff.sh`, the narrow rows and the cost matrix were not run on the prototype.

## 8. Not measured

- The exits of the loops (`break`, `return`): the 3.5% of listed keys that no step reached is a hypothesis about early exits.
- Shape sweeps during the page run: the simulated cache hit rate in 1.1 ignores them.
- The size of `PropertyKey` and of the A cache: estimates.
- Design A and design C: no prototype. Their gains are hypotheses.
- Wall time. Windows. The tree walker share of the prototype (it changed the lane only).
- The page in `byte` or `tree` mode.
- Other public journeys and the load-test ramps: only the reference page and the three fixture rows were measured.
